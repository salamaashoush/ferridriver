//! `ferridriver upgrade` — replace this binary with the newest release.
//!
//! The same artifacts `install.sh` fetches: a `ferridriver-{version}-{target}
//! .tar.gz` on the GitHub release for `v{version}`, with a `.sha256` beside
//! it. The contract lives in `.github/workflows/release.yml`, and the target
//! is derived from the running OS and architecture rather than from the
//! compile-time triple — a locally built Linux binary is `-gnu` while the
//! release is `-musl`, and they are the same download.
//!
//! Replacing the running executable is a `rename(2)` over it, which unix
//! allows: the running process keeps the old inode until it exits, and the
//! next invocation gets the new one. The temporary file is written into the
//! install directory rather than `/tmp` so the rename stays on one
//! filesystem, where it is atomic — a half-written binary on someone's PATH
//! is not a recoverable state.
//!
//! Two channels, and a build follows its own by default: a canary that
//! upgraded itself onto stable would be a one-way door nobody asked for.
//! Stable is a per-version tag (`v0.5.0`); canary is one rolling prerelease
//! tagged `canary` whose assets are replaced on every push, so the channel
//! costs one release rather than one per commit. The version a canary
//! carries lives in the release NAME, because its tag never changes.

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use crate::build_info;
use crate::cli;
use crate::ui;

/// Where the releases live. Same repository `install.sh` reads.
const REPO: &str = "salamaashoush/ferridriver";

/// The binary this command replaces, and the helper shipped beside it.
const BINARY: &str = "ferridriver";
const WEBKIT_HOST: &str = "fd_webkit_host";

/// The rolling prerelease canary builds are published to.
const CANARY_TAG: &str = "canary";

pub async fn run(args: cli::UpgradeArgs) -> anyhow::Result<()> {
  let current = build_info::VERSION;
  let exe = std::env::current_exe()?.canonicalize()?;
  let channel = args.channel();

  let release = fetch_release(&channel, args.tag.as_deref()).await?;
  let latest = release.version();
  let newer = should_install(&latest, current, channel);

  if ui::json() && (args.check || (!newer && !args.force)) {
    return ui::print_json(&serde_json::json!({
      "current": current,
      "latest": latest,
      "channel": channel.name(),
      "upToDate": !newer,
      "url": release.html_url,
      "executable": exe.display().to_string(),
    }));
  }

  if !newer && !args.force {
    ui::say(&ui::success(&format!(
      "ferridriver {current} is already the latest {} version",
      channel.name()
    )));
    if channel == Channel::Stable && !build_info::is_canary() {
      ui::say(&ui::dim(
        "  `ferridriver upgrade --canary` follows the unreleased builds",
      ));
    }
    if build_info::is_canary() && channel == Channel::Canary {
      ui::say(&ui::dim(&format!(
        "  `ferridriver upgrade --stable` moves onto the {} release line",
        build_info::RELEASE_VERSION
      )));
    }
    return Ok(());
  }
  if args.check {
    ui::say(&ui::info(&format!(
      "ferridriver {} is available on {} (you have {current})",
      ui::bold(&latest),
      channel.name()
    )));
    ui::next_steps(&[("upgrade", format!("ferridriver upgrade{}", channel.flag()))]);
    return Ok(());
  }

  let target = release_target()?;
  let dir = exe
    .parent()
    .ok_or_else(|| anyhow::anyhow!("{} has no parent directory", exe.display()))?;
  writable(dir, &exe)?;

  let archive = release.archive_name(target);
  let base = format!("https://github.com/{REPO}/releases/download/{}", release.tag_name);

  let staged =
    tempfile::tempdir_in(dir).map_err(|e| anyhow::anyhow!("stage the download in {}: {e}", dir.display()))?;
  let tarball = staged.path().join(&archive);

  let mut bar = ui::Progress::new(latest.clone());
  let bytes = match download(&format!("{base}/{archive}"), &tarball, &mut bar).await {
    Ok(bytes) => bytes,
    Err(error) => {
      bar.finish_fail(&format!("downloading {archive}"));
      return Err(error);
    },
  };
  // The checksum is published beside the archive; a truncated or tampered
  // download must not become the binary on someone's PATH.
  match verify(&format!("{base}/{archive}.sha256"), &tarball, &archive).await {
    Ok(()) => bar.finish_ok(&format!("downloaded {archive} ({})", ui::bytes(bytes))),
    Err(error) => {
      bar.finish_fail(&format!("verifying {archive}"));
      return Err(error);
    },
  }

  unpack(&tarball, staged.path())?;
  install(staged.path(), dir)?;

  ui::say(&ui::success(&format!(
    "upgraded {} → {}",
    ui::dim(current),
    ui::bold(&latest)
  )));
  ui::say(&format!("  {}", ui::url(&release.html_url)));
  if in_cargo_bin(&exe) {
    // Not an error — the new binary is in place and works — but `cargo
    // install-update` reads its own registry metadata, which now disagrees
    // with what is on disk.
    ui::say(&format!(
      "\n{}",
      ui::warning("this binary came from `cargo install`; cargo's own metadata still says the old version")
    ));
  }
  Ok(())
}

/// Which line of releases a run follows.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Channel {
  Stable,
  Canary,
}

impl Channel {
  fn name(self) -> &'static str {
    match self {
      Self::Stable => "stable",
      Self::Canary => "canary",
    }
  }

  /// The flag that would have selected this channel explicitly.
  fn flag(self) -> &'static str {
    match self {
      Self::Stable => "",
      Self::Canary => " --canary",
    }
  }
}

// ── the release ─────────────────────────────────────────────────────────

/// The fields of a GitHub release this command reads.
#[derive(serde::Deserialize)]
struct Release {
  tag_name: String,
  html_url: String,
  /// The full version string. Only a canary needs it — its tag never
  /// changes, so the tag cannot say which build is behind it.
  #[serde(default)]
  name: Option<String>,
}

impl Release {
  /// The version this release publishes.
  ///
  /// A stable release is identified by its tag; the rolling canary tag says
  /// nothing, so its name carries the version instead.
  fn version(&self) -> String {
    if self.tag_name == CANARY_TAG {
      return self
        .name
        .clone()
        .unwrap_or_else(|| CANARY_TAG.to_string())
        .trim()
        .trim_start_matches('v')
        .to_string();
    }
    self.tag_name.trim_start_matches('v').to_string()
  }

  /// The asset to download for `target`.
  ///
  /// Stable assets are named for their version; the canary's are named for
  /// the channel, because they are replaced in place on every push and a
  /// versioned name would leave the old ones behind on the same release.
  fn archive_name(&self, target: &str) -> String {
    if self.tag_name == CANARY_TAG {
      format!("{BINARY}-{CANARY_TAG}-{target}.tar.gz")
    } else {
      format!("{BINARY}-{}-{target}.tar.gz", self.version())
    }
  }
}

/// Resolve `--tag`, or the newest release on `channel`.
async fn fetch_release(channel: &Channel, tag: Option<&str>) -> anyhow::Result<Release> {
  let url = match (tag, channel) {
    (Some(tag), _) => format!("https://api.github.com/repos/{REPO}/releases/tags/{tag}"),
    // `/releases/latest` skips prereleases, which is exactly the stable
    // channel; the canary is one known tag.
    (None, Channel::Stable) => format!("https://api.github.com/repos/{REPO}/releases/latest"),
    (None, Channel::Canary) => format!("https://api.github.com/repos/{REPO}/releases/tags/{CANARY_TAG}"),
  };
  let mut request = client()?
    .get(&url)
    // GitHub rejects an API request with no User-Agent outright.
    .header("User-Agent", user_agent())
    .header("Accept", "application/vnd.github+json");
  // Unauthenticated API calls are rate-limited per IP; a token the user
  // already has for `gh` lifts that without asking them for anything.
  if let Some(token) = github_token() {
    request = request.header("Authorization", format!("Bearer {token}"));
  }
  let response = request.send().await.map_err(|e| anyhow::anyhow!("asking {url}: {e}"))?;
  let status = response.status();
  if status == reqwest::StatusCode::NOT_FOUND {
    return match (tag, channel) {
      (Some(tag), _) => Err(anyhow::anyhow!("no release tagged {tag}")),
      (None, Channel::Canary) => Err(anyhow::anyhow!(
        "no canary has been published yet — `ferridriver upgrade` follows the stable releases"
      )),
      (None, Channel::Stable) => Err(anyhow::anyhow!("{REPO} has published no releases yet")),
    };
  }
  if status == reqwest::StatusCode::FORBIDDEN {
    anyhow::bail!("GitHub rate-limited this check; set GITHUB_TOKEN (or GH_TOKEN) and try again");
  }
  if !status.is_success() {
    anyhow::bail!("GitHub answered {status} for {url}");
  }
  response
    .json::<Release>()
    .await
    .map_err(|e| anyhow::anyhow!("reading the release from GitHub: {e}"))
}

/// GitHub requires one, and naming the exact build makes the request
/// traceable in the same terms `--version` prints.
fn user_agent() -> String {
  format!("ferridriver/{} ({})", build_info::VERSION, build_info::CHANNEL)
}

fn github_token() -> Option<String> {
  std::env::var("GITHUB_TOKEN")
    .or_else(|_| std::env::var("GH_TOKEN"))
    .ok()
    .filter(|t| !t.is_empty())
}

fn client() -> anyhow::Result<reqwest::Client> {
  reqwest::Client::builder()
    .build()
    .map_err(|e| anyhow::anyhow!("building an HTTP client: {e}"))
}

// ── versions ────────────────────────────────────────────────────────────

/// Whether `candidate` is worth installing over `current`, on `channel`.
///
/// Stable compares as semver, so `0.10.0` beats `0.9.0` — a string comparison
/// would say the opposite.
///
/// Canary compares by identity, not by order. Its versions differ only in a
/// commit sha (`0.5.0-canary.a1b2c3d4e`), and semver orders prerelease
/// identifiers alphanumerically — which has nothing to do with which commit
/// came first. There is only ever one canary and it is the tip of `main`, so
/// "different" is the whole question.
fn should_install(candidate: &str, current: &str, channel: Channel) -> bool {
  if channel == Channel::Canary {
    return candidate != current;
  }
  match (semver::Version::parse(candidate), semver::Version::parse(current)) {
    (Ok(candidate), Ok(current)) => candidate > current,
    // A `--tag` naming something semver cannot read is still installable;
    // "different" is the safe answer.
    _ => candidate != current,
  }
}

// ── platform ────────────────────────────────────────────────────────────

/// The release target triple for the running machine.
///
/// Derived from OS and architecture, not from the compile-time triple: a
/// locally built Linux binary is `-gnu`, the published one is `-musl`, and
/// `upgrade` on the former must still find the latter.
fn release_target() -> anyhow::Result<&'static str> {
  match (std::env::consts::OS, std::env::consts::ARCH) {
    ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
    ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
    ("linux", "aarch64") => Ok("aarch64-unknown-linux-musl"),
    ("linux", "x86_64") => Ok("x86_64-unknown-linux-musl"),
    (os, arch) => Err(anyhow::anyhow!(
      "no published binary for {os}/{arch} — build from source with `cargo install ferridriver-cli`"
    )),
  }
}

fn in_cargo_bin(exe: &Path) -> bool {
  dirs::home_dir().is_some_and(|home| exe.starts_with(home.join(".cargo").join("bin")))
}

/// Fail before downloading anything if the replacement could not land.
///
/// A permission error after a 40MB download, with a temporary file already
/// written next to the binary, is a worse place to find out.
fn writable(dir: &Path, exe: &Path) -> anyhow::Result<()> {
  let probe = dir.join(format!(".ferridriver-upgrade-probe-{}", std::process::id()));
  match std::fs::File::create(&probe) {
    Ok(_) => {
      let _ = std::fs::remove_file(&probe);
      Ok(())
    },
    Err(e) => Err(anyhow::anyhow!(
      "cannot write to {} ({e}) — reinstall with `curl -fsSL https://raw.githubusercontent.com/{REPO}/main/install.sh | bash`, \
       or re-run this with permission to write {}",
      dir.display(),
      exe.display()
    )),
  }
}

// ── download, verify, unpack, install ───────────────────────────────────

/// Stream the archive to `dest`, reporting progress. Returns its size.
async fn download(url: &str, dest: &Path, bar: &mut ui::Progress) -> anyhow::Result<u64> {
  use futures::StreamExt as _;

  let response = client()?
    .get(url)
    .header("User-Agent", user_agent())
    .send()
    .await
    .map_err(|e| anyhow::anyhow!("fetching {url}: {e}"))?;
  if !response.status().is_success() {
    anyhow::bail!("{} for {url}", response.status());
  }
  let total = response.content_length();

  let mut file = std::fs::File::create(dest)?;
  let mut written: u64 = 0;
  let mut stream = response.bytes_stream();
  while let Some(chunk) = stream.next().await {
    let chunk = chunk.map_err(|e| anyhow::anyhow!("reading {url}: {e}"))?;
    file.write_all(&chunk)?;
    written += chunk.len() as u64;
    bar.set(written, total);
  }
  file.flush()?;
  Ok(written)
}

/// Check the archive against the `.sha256` published beside it.
async fn verify(url: &str, archive: &Path, name: &str) -> anyhow::Result<()> {
  use sha2::Digest as _;

  let response = client()?
    .get(url)
    .header("User-Agent", user_agent())
    .send()
    .await
    .map_err(|e| anyhow::anyhow!("fetching {url}: {e}"))?;
  if !response.status().is_success() {
    anyhow::bail!("no checksum published for {name} ({} for {url})", response.status());
  }
  let published = response
    .text()
    .await
    .map_err(|e| anyhow::anyhow!("reading {url}: {e}"))?;
  // `shasum -a 256 file` writes "<hex>  <name>"; only the digest matters.
  let expected = published
    .split_whitespace()
    .next()
    .ok_or_else(|| anyhow::anyhow!("empty checksum file at {url}"))?
    .to_ascii_lowercase();

  // Read in chunks rather than slurping: a release tarball is tens of
  // megabytes and there is no reason for all of it to be resident.
  let mut hasher = sha2::Sha256::new();
  let mut file = std::io::BufReader::new(std::fs::File::open(archive)?);
  let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
  loop {
    let read = std::io::Read::read(&mut file, &mut buffer)?;
    if read == 0 {
      break;
    }
    hasher.update(&buffer[..read]);
  }
  let actual: String = hasher.finalize().iter().fold(String::new(), |mut out, byte| {
    use std::fmt::Write as _;
    let _ = write!(out, "{byte:02x}");
    out
  });

  if actual != expected {
    anyhow::bail!("checksum mismatch for {name}: expected {expected}, got {actual}");
  }
  Ok(())
}

/// Unpack the tarball beside itself.
fn unpack(archive: &Path, into: &Path) -> anyhow::Result<()> {
  let file = std::fs::File::open(archive)?;
  let decoder = flate2::read::GzDecoder::new(file);
  tar::Archive::new(decoder)
    .unpack(into)
    .map_err(|e| anyhow::anyhow!("unpacking {}: {e}", archive.display()))?;
  if !into.join(BINARY).is_file() {
    anyhow::bail!("{} contains no `{BINARY}` binary", archive.display());
  }
  Ok(())
}

/// Move the unpacked files into place, the binary last.
///
/// Last because it is the one that decides which version is installed: if the
/// `WebKit` helper cannot be replaced, the upgrade stops with the old pair
/// intact rather than a new binary beside a stale helper.
fn install(staged: &Path, dir: &Path) -> anyhow::Result<()> {
  let helper = staged.join(WEBKIT_HOST);
  if helper.is_file() {
    place(&helper, &dir.join(WEBKIT_HOST))?;
  }
  place(&staged.join(BINARY), &dir.join(BINARY))
}

/// `rename(2)` one file over another, executable.
fn place(from: &Path, to: &Path) -> anyhow::Result<()> {
  std::fs::set_permissions(from, std::fs::Permissions::from_mode(0o755))
    .map_err(|e| anyhow::anyhow!("making {} executable: {e}", from.display()))?;
  std::fs::rename(from, to).map_err(|e| anyhow::anyhow!("replacing {}: {e}", to.display()))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn stable_versions_compare_as_semver_not_as_strings() {
    assert!(
      should_install("0.10.0", "0.9.0", Channel::Stable),
      "0.10.0 supersedes 0.9.0"
    );
    assert!(!should_install("0.9.0", "0.10.0", Channel::Stable));
    assert!(!should_install("0.5.0", "0.5.0", Channel::Stable));
  }

  #[test]
  fn moving_from_a_canary_onto_stable_is_an_upgrade() {
    assert!(should_install("0.5.0", "0.5.0-canary.a1b2c3d4e", Channel::Stable));
  }

  #[test]
  fn canaries_compare_by_identity_because_shas_have_no_order() {
    // `abc` sorts before `def` alphanumerically, but the commit behind it
    // may well be the newer one — there is only ever one canary, so any
    // difference from what is installed means there is something to install.
    assert!(should_install(
      "0.5.0-canary.aaa111222",
      "0.5.0-canary.fff999888",
      Channel::Canary
    ));
    assert!(should_install(
      "0.5.0-canary.fff999888",
      "0.5.0-canary.aaa111222",
      Channel::Canary
    ));
    assert!(!should_install(
      "0.5.0-canary.aaa111222",
      "0.5.0-canary.aaa111222",
      Channel::Canary
    ));
  }

  #[test]
  fn an_unparseable_tag_counts_as_newer_when_it_differs() {
    assert!(should_install("nightly", "0.5.0", Channel::Stable));
    assert!(!should_install("nightly", "nightly", Channel::Stable));
  }

  #[test]
  fn every_published_target_is_reachable_from_some_platform() {
    // The mapping must stay in step with release.yml's matrix; this catches
    // a triple renamed on one side only.
    let published = [
      "aarch64-apple-darwin",
      "x86_64-apple-darwin",
      "aarch64-unknown-linux-musl",
      "x86_64-unknown-linux-musl",
    ];
    let target = release_target().expect("this platform publishes a binary");
    assert!(published.contains(&target), "{target} is not a published target");
  }
}
