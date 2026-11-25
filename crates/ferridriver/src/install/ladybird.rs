//! Ladybird's official builds: the macOS disk image and the Ubuntu
//! package, both listed in `releases.json` with their SHA-256.
//!
//! The Linux package is self-contained under `/opt/ladybird` (its
//! binaries find their libraries through `$ORIGIN/../lib`), so unpacking
//! it into the cache runs on any glibc new enough for the oldest Ubuntu
//! release it is built for.

use std::path::{Path, PathBuf};

use super::{BrowserInstaller, InstallProgress, backend_err};
use crate::error::{FerriError, Result};

const LADYBIRD_RELEASES_URL: &str = "https://download.ladybird.org/releases.json";

#[derive(serde::Deserialize)]
struct Releases {
  releases: Vec<Release>,
}

#[derive(serde::Deserialize)]
struct Release {
  version: String,
  #[serde(default)]
  downloads: Vec<Download>,
  #[serde(default)]
  repositories: Repositories,
}

#[derive(serde::Deserialize)]
struct Download {
  platform: String,
  #[serde(default)]
  architecture: Option<String>,
  url: String,
  sha256: String,
}

#[derive(serde::Deserialize, Default)]
struct Repositories {
  #[serde(default)]
  apt: Option<AptRepository>,
}

#[derive(serde::Deserialize)]
struct AptRepository {
  package: String,
  systems: Vec<AptSystem>,
}

#[derive(serde::Deserialize)]
struct AptSystem {
  id: String,
  version_id: String,
  sources: String,
  architectures: Vec<String>,
}

/// Where the Ladybird browser executable sits in an install directory:
/// the app bundle the macOS disk image holds, or the `/opt/ladybird`
/// tree of the Linux package.
pub(super) fn executable_path(install_dir: &Path) -> PathBuf {
  if cfg!(target_os = "macos") {
    install_dir.join("Ladybird.app/Contents/MacOS/Ladybird")
  } else {
    install_dir.join("opt/ladybird/bin/Ladybird")
  }
}

impl BrowserInstaller {
  /// Install the newest Ladybird release into the cache.
  ///
  /// # Errors
  ///
  /// Returns an error when the release index or package repository is
  /// unreachable, the host has no published build, a download fails or
  /// does not match its published SHA-256, or unpacking fails.
  pub async fn install_ladybird<F>(&self, progress: F) -> Result<String>
  where
    F: Fn(InstallProgress),
  {
    progress(InstallProgress::Resolving);
    let releases: Releases = self
      .client()?
      .get(LADYBIRD_RELEASES_URL)
      .send()
      .await
      .and_then(reqwest::Response::error_for_status)
      .map_err(|e| backend_err(format!("failed to fetch Ladybird releases: {e}")))?
      .json()
      .await
      .map_err(|e| backend_err(format!("failed to parse Ladybird releases: {e}")))?;
    let release = releases
      .releases
      .into_iter()
      .next()
      .ok_or_else(|| backend_err("Ladybird publishes no release"))?;

    let install_dir = self.cache_dir.join(format!("ladybird-{}", release.version));
    let marker = install_dir.join(".downloaded");
    let executable = executable_path(&install_dir);
    if marker.exists() && executable.exists() {
      let path = executable.to_string_lossy().into_owned();
      progress(InstallProgress::AlreadyInstalled {
        version: release.version,
        path: path.clone(),
      });
      return Ok(path);
    }
    if install_dir.exists() {
      tokio::fs::remove_dir_all(&install_dir).await?;
    }
    let tmp_dir = self.cache_dir.join(".tmp");
    tokio::fs::create_dir_all(&tmp_dir).await?;

    let (url, sha256) = if cfg!(target_os = "macos") {
      macos_download(&release)?
    } else if cfg!(target_os = "linux") {
      self.linux_package(&release).await?
    } else {
      return Err(FerriError::unsupported(
        "Ladybird publishes builds for macOS and Linux only",
      ));
    };
    let archive = tmp_dir.join(
      url
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or("ladybird-download"),
    );
    self
      .download_verified(&url, &archive, &sha256, "Ladybird", &progress)
      .await?;

    progress(InstallProgress::Extracting);
    tokio::fs::create_dir_all(&install_dir).await?;
    let (from, to) = (archive.clone(), install_dir.clone());
    let unpacked = tokio::task::spawn_blocking(move || {
      if cfg!(target_os = "macos") {
        super::extract_dmg(&from, &to)
      } else {
        extract_deb(&from, &to)
      }
    })
    .await
    .map_err(|e| backend_err(format!("extract task failed: {e}")))?;
    let _ = tokio::fs::remove_file(&archive).await;
    unpacked?;

    let path = executable.to_string_lossy().into_owned();
    if !executable.exists() {
      return Err(backend_err(format!(
        "extraction completed but the Ladybird executable is not at {path}"
      )));
    }
    tokio::fs::write(&marker, release.version.as_bytes()).await?;
    progress(InstallProgress::Complete {
      version: release.version,
      path: path.clone(),
    });
    Ok(path)
  }

  /// The `.deb` of `release` for this host, from Ladybird's apt
  /// repository: its URL and SHA-256. The `Packages` index it is listed
  /// in is checked against the repository's `Release` file.
  async fn linux_package(&self, release: &Release) -> Result<(String, String)> {
    let apt = release
      .repositories
      .apt
      .as_ref()
      .ok_or_else(|| FerriError::unsupported("Ladybird publishes no Linux package for this release"))?;
    let arch = match std::env::consts::ARCH {
      "x86_64" => "amd64",
      "aarch64" => "arm64",
      other => {
        return Err(FerriError::unsupported(format!(
          "Ladybird publishes no Linux build for {other}"
        )));
      },
    };
    let os_release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let field = |key: &str| {
      os_release
        .lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
        .map(|value| value.trim_matches('"').to_owned())
        .unwrap_or_default()
    };
    let (id, version_id) = (field("ID"), field("VERSION_ID"));
    let candidates: Vec<&AptSystem> = apt
      .systems
      .iter()
      .filter(|system| system.architectures.iter().any(|a| a == arch))
      .collect();
    // The host's own release if it is listed, otherwise the oldest one,
    // whose glibc the most hosts have.
    let system = candidates
      .iter()
      .find(|system| system.id == id && system.version_id == version_id)
      .or_else(|| candidates.first())
      .ok_or_else(|| FerriError::unsupported(format!("Ladybird publishes no Linux package for {arch}")))?;

    let client = self.client()?;
    let text = |url: String| async move {
      client
        .get(&url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| backend_err(format!("failed to fetch {url}: {e}")))?
        .text()
        .await
        .map_err(|e| backend_err(format!("failed to read {url}: {e}")))
    };
    let sources = text(system.sources.clone()).await?;
    let source = |key: &str| {
      sources
        .lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix(':'))
        .and_then(|value| value.split_whitespace().next())
        .map(ToOwned::to_owned)
        .ok_or_else(|| backend_err(format!("Ladybird's apt source names no {key}")))
    };
    let uri = source("URIs")?;
    let uri = uri.trim_end_matches('/');
    let suite = source("Suites")?;
    let component = source("Components")?;
    let index = format!("{component}/binary-{arch}/Packages");
    let release_file = text(format!("{uri}/dists/{suite}/Release")).await?;
    let packages = text(format!("{uri}/dists/{suite}/{index}")).await?;
    let listed = release_file
      .lines()
      .skip_while(|line| !line.starts_with("SHA256:"))
      .skip(1)
      .take_while(|line| line.starts_with(' '))
      .find_map(|line| {
        let mut fields = line.split_whitespace();
        let (hash, _, name) = (fields.next()?, fields.next()?, fields.next()?);
        (name == index).then(|| hash.to_owned())
      })
      .ok_or_else(|| backend_err(format!("Ladybird's apt Release lists no {index}")))?;
    if super::sha256_hex(packages.as_bytes()) != listed {
      return Err(backend_err(format!(
        "Ladybird's {index} does not match the SHA-256 its Release lists"
      )));
    }
    let (filename, sha256) = package_entry(&packages, &apt.package, &release.version).ok_or_else(|| {
      backend_err(format!(
        "Ladybird's {index} lists no {} {}",
        apt.package, release.version
      ))
    })?;
    Ok((format!("{uri}/{filename}"), sha256))
  }
}

fn macos_download(release: &Release) -> Result<(String, String)> {
  let arch = match std::env::consts::ARCH {
    "aarch64" => "arm64",
    other => other,
  };
  release
    .downloads
    .iter()
    .find(|download| download.platform == "macOS" && download.architecture.as_deref() == Some(arch))
    .map(|download| (download.url.clone(), download.sha256.clone()))
    .ok_or_else(|| FerriError::unsupported(format!("Ladybird publishes no macOS build for {arch}")))
}

/// The `Filename` and `SHA256` of `package` at `version` in an apt
/// `Packages` index.
fn package_entry(index: &str, package: &str, version: &str) -> Option<(String, String)> {
  index.split("\n\n").find_map(|stanza| {
    let field = |key: &str| {
      stanza
        .lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix(": "))
        .map(str::trim)
    };
    let matches = field("Package") == Some(package)
      && field("Version").is_some_and(|listed| listed.split('-').next() == Some(version));
    if !matches {
      return None;
    }
    Some((field("Filename")?.to_owned(), field("SHA256")?.to_owned()))
  })
}

/// Unpack the `data.tar` member of a Debian package into `dest`. A
/// package is an `ar` archive of `debian-binary`, `control.tar.*` and
/// `data.tar.*`.
fn extract_deb(deb: &Path, dest: &Path) -> Result<()> {
  let bytes = std::fs::read(deb)?;
  let (name, data) = ar_members(&bytes)?
    .into_iter()
    .find(|(name, _)| name.starts_with("data.tar"))
    .ok_or_else(|| backend_err("the Ladybird package holds no data archive"))?;
  match name.as_str() {
    "data.tar.xz" => tar::Archive::new(liblzma::read::XzDecoder::new(data)).unpack(dest)?,
    "data.tar.gz" => tar::Archive::new(flate2::read::GzDecoder::new(data)).unpack(dest)?,
    "data.tar" => tar::Archive::new(data).unpack(dest)?,
    other => {
      return Err(FerriError::unsupported(format!(
        "the Ladybird package's {other} is in a compression ferridriver does not read"
      )));
    },
  }
  Ok(())
}

/// The members of a System V `ar` archive: an `!<arch>` line, then per
/// member a 60-byte header (name, mtime, uid, gid, mode, decimal size, and
/// a two-byte terminator) and its data, padded to an even length.
fn ar_members(bytes: &[u8]) -> Result<Vec<(String, &[u8])>> {
  let mut rest = bytes
    .strip_prefix(b"!<arch>\n")
    .ok_or_else(|| backend_err("the Ladybird package is not an ar archive"))?;
  let mut members = Vec::new();
  while rest.len() >= 60 {
    let (header, after) = rest.split_at(60);
    let name = String::from_utf8_lossy(&header[..16])
      .trim_end()
      .trim_end_matches('/')
      .to_owned();
    let size: usize = std::str::from_utf8(&header[48..58])
      .ok()
      .and_then(|size| size.trim().parse().ok())
      .ok_or_else(|| backend_err("the Ladybird package has a malformed member header"))?;
    let data = after
      .get(..size)
      .ok_or_else(|| backend_err("the Ladybird package is truncated"))?;
    members.push((name, data));
    rest = after.get(size + size % 2..).unwrap_or_default();
  }
  Ok(members)
}

#[cfg(test)]
mod tests {
  #[test]
  fn finds_the_release_build_in_an_apt_index() {
    let index = "Package: ladybird\nVersion: 2026.47.6-1~ubuntu24.04\nFilename: pool/a.deb\nSHA256: aa\n\nPackage: ladybird\nVersion: 2026.47.7-1~ubuntu24.04\nFilename: pool/b.deb\nSHA256: bb\n";
    assert_eq!(
      super::package_entry(index, "ladybird", "2026.47.7"),
      Some(("pool/b.deb".to_owned(), "bb".to_owned()))
    );
    assert_eq!(super::package_entry(index, "ladybird", "2026.48.1"), None);
  }

  #[test]
  fn reads_the_members_of_an_ar_archive() {
    let mut archive = b"!<arch>\n".to_vec();
    for (name, data) in [("debian-binary", &b"2.0\n"[..]), ("data.tar.xz", &b"abc"[..])] {
      archive.extend(format!("{name:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n", 0, 0, 0, 100_644, data.len()).as_bytes());
      archive.extend(data);
      if data.len() % 2 == 1 {
        archive.push(b'\n');
      }
    }
    let members = super::ar_members(&archive).unwrap();
    assert_eq!(members[0], ("debian-binary".to_owned(), &b"2.0\n"[..]));
    assert_eq!(members[1], ("data.tar.xz".to_owned(), &b"abc"[..]));
  }
}
