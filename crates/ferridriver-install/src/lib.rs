//! Browser installation module.
//!
//! Downloads Chrome for Testing binaries from Google's official CDN
//! and installs them to a local cache directory. Provides the same
//! functionality as `npx playwright install chromium` but as a native
//! Rust implementation. Includes `--with-deps` support for installing
//! system-level dependencies on Linux (matching Playwright's nativeDeps).
//!
//! # Usage
//!
//! ```ignore
//! use ferridriver::install::{BrowserInstaller, DependencyGroup, InstallProgress};
//!
//! let installer = BrowserInstaller::new();
//! let path = installer.install_chromium(|p| { /* handle progress */ }).await?;
//!
//! // Install system deps on Linux (requires sudo)
//! installer.install_system_deps(&[DependencyGroup::Chromium], |p| { /* handle progress */ }).await?;
//! ```

mod android;
mod archive;
mod ios;
mod ladybird;
mod lightpanda;
#[cfg(any(target_os = "linux", test))]
mod linux;
mod platform;
mod playwright;
mod playwright_cache;
#[cfg(test)]
mod tests;

pub use archive::ANDROID_DRIVER_APKS;
use archive::{extract_dmg, extract_firefox_archive, extract_zip, firefox_archive_info, firefox_executable_path};
#[cfg(test)]
use archive::{extract_tar_xz, link_stays_inside};
#[cfg(test)]
use platform::{WEBKIT_EXECUTABLE, mac_host_platform, webkit_archive, webkit_platform_for_host};
use platform::{
  chrome_executable_path, chromedriver_executable_path, current_platform, headless_shell_executable_path,
};
use playwright::PW_CDN_MIRRORS;
#[cfg(test)]
use playwright::{NpmRelease, PwBrowsersJson, browser_revision, released_browsers_json_url};

pub use android::{AndroidOptions, PreparedAndroidSdk};
pub use playwright_cache::{
  CachedBrowser, INSTALLATION_COMPLETE, build_dir, find_playwright_browser, installed_build, newest_build_in,
  playwright_cache_dir,
};

use std::path::{Path, PathBuf};

use futures::StreamExt;
use reqwest::Client;
use serde::Deserialize;
use tokio::io::AsyncWriteExt;

use ferridriver_error::{FerriError, Result};

fn backend_err(context: impl std::fmt::Display) -> FerriError {
  FerriError::backend(context.to_string())
}

async fn install_lock(path: &Path) -> Result<std::fs::File> {
  let lock = std::fs::OpenOptions::new()
    .create(true)
    .truncate(false)
    .write(true)
    .open(path)?;
  let deadline = tokio::time::Instant::now() + std::time::Duration::from_mins(30);
  loop {
    match lock.try_lock() {
      Ok(()) => return Ok(lock),
      Err(std::fs::TryLockError::WouldBlock) if tokio::time::Instant::now() < deadline => {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
      },
      Err(error) => return Err(backend_err(format!("locking device dependencies: {error}"))),
    }
  }
}

/// Lowercase hex SHA-256, for pinning a downloaded artifact to its
/// published bytes.
fn sha256_hex(bytes: &[u8]) -> String {
  use sha2::{Digest, Sha256};
  let digest = Sha256::digest(bytes);
  digest.iter().fold(String::with_capacity(64), |mut out, byte| {
    use std::fmt::Write as _;
    let _ = write!(out, "{byte:02x}");
    out
  })
}

// ---------------------------------------------------------------------------
// Chrome for Testing API types
// ---------------------------------------------------------------------------

const CFT_VERSIONS_URL: &str =
  "https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions-with-downloads.json";

/// Where Chrome for Testing publishes every version's builds, by version
/// and platform.
const CFT_DOWNLOADS_URL: &str = "https://storage.googleapis.com/chrome-for-testing-public";

/// Numbers this process's `ChromeDriver` staging directories apart.
static CHROMEDRIVER_STAGED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Mozilla product-details API for resolving Firefox versions.
const FIREFOX_VERSIONS_URL: &str = "https://product-details.mozilla.org/1.0/firefox_versions.json";

/// Base URL for Firefox stable releases.
const FIREFOX_RELEASES_URL: &str = "https://archive.mozilla.org/pub/firefox/releases";

/// axe-core, the accessibility rule engine.
///
/// Pinned to the version Lighthouse bundles rather than to the latest
/// release: the two are compared against each other, and a rule that
/// only one of them has would read as a disagreement about the page.
const AXE_CORE_VERSION: &str = "4.12.1";

/// Published tarball contents are immutable per version, so the digest
/// pins the bytes as well as the number. A mismatch means the artifact
/// changed under a fixed version, which is not a thing to shrug at.
const AXE_CORE_SHA256: &str = "66a8aaa95a8b044a7fd74a5435873bf04ff65a1ca75567c921b7509742085a14";

/// Download retry count (matches Playwright's 5 attempts).
const DOWNLOAD_RETRIES: u32 = 5;

#[derive(Debug, Deserialize)]
struct CftResponse {
  channels: CftChannels,
}

#[derive(Debug, Deserialize)]
struct CftChannels {
  #[serde(rename = "Stable")]
  stable: CftChannel,
}

#[derive(Debug, Deserialize)]
struct CftChannel {
  version: String,
  downloads: CftDownloads,
}

#[derive(Debug, Deserialize)]
struct CftDownloads {
  chrome: Vec<CftDownload>,
  #[serde(rename = "chrome-headless-shell")]
  chrome_headless_shell: Vec<CftDownload>,
}

#[derive(Debug, Deserialize)]
struct CftDownload {
  platform: String,
  url: String,
}

// ---------------------------------------------------------------------------
// Progress reporting
// ---------------------------------------------------------------------------

/// Progress updates during browser installation.
#[derive(Debug, Clone)]
pub enum InstallProgress {
  /// Resolving the latest stable Chrome version.
  Resolving,
  /// Downloading the browser archive.
  Downloading {
    bytes_downloaded: u64,
    total_bytes: Option<u64>,
  },
  /// Extracting the archive to disk.
  Extracting,
  /// Installation complete.
  Complete { version: String, path: String },
  /// Browser already installed, skipping download.
  AlreadyInstalled { version: String, path: String },
  /// Installing system dependencies.
  InstallingDeps { distro: String },
  /// System dependencies installed.
  DepsInstalled,
}

/// A set of versioned directories in the cache that `install` adds to,
/// one version at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheFamily {
  Chromium,
  ChromiumHeadlessShell,
  ChromeDriver,
  Firefox,
  WebKit,
  AxeCore,
  Ffmpeg,
  Ladybird,
  Lightpanda,
}

impl CacheFamily {
  /// The family `ferridriver install <target>` adds to, if it has one.
  /// Safari, Android and iOS are not versioned directories of the cache.
  #[must_use]
  pub fn of_target(target: &str) -> Option<Self> {
    Some(match target {
      "chromium" => Self::Chromium,
      "chromium-headless-shell" => Self::ChromiumHeadlessShell,
      "chromedriver" => Self::ChromeDriver,
      "firefox" => Self::Firefox,
      "webkit" => Self::WebKit,
      "axe" => Self::AxeCore,
      "ffmpeg" => Self::Ffmpeg,
      "ladybird" => Self::Ladybird,
      "lightpanda" => Self::Lightpanda,
      _ => return None,
    })
  }

  fn root(self, cache_dir: &Path) -> PathBuf {
    match self {
      Self::WebKit => cache_dir.join("webkit"),
      _ => cache_dir.to_path_buf(),
    }
  }

  fn prefix(self) -> &'static str {
    match self {
      Self::Chromium => "chromium-",
      Self::ChromiumHeadlessShell => "chromium-headless-shell-",
      Self::ChromeDriver => "chromedriver-",
      Self::Firefox => "firefox-",
      Self::WebKit => "webkit-",
      Self::AxeCore => "axe-core-",
      Self::Ffmpeg => "ffmpeg-",
      Self::Ladybird => "ladybird-",
      Self::Lightpanda => "lightpanda-",
    }
  }

  /// The version a directory of this family is named after.
  fn version_of(self, name: &str) -> Option<&str> {
    // `chromium-headless-shell-*` also starts with `chromium-`, and it
    // is another family.
    if self == Self::Chromium && name.starts_with(Self::ChromiumHeadlessShell.prefix()) {
      return None;
    }
    name.strip_prefix(self.prefix()).filter(|version| !version.is_empty())
  }
}

/// What a browser needs from the system on Linux. `Tools` is the virtual
/// display and fonts every browser uses, and is always installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyGroup {
  Tools,
  Chromium,
  Firefox,
  WebKit,
}

impl DependencyGroup {
  /// The group `ferridriver install <target>` needs, if it needs one.
  #[must_use]
  pub fn of_target(target: &str) -> Option<Self> {
    Some(match target {
      "chromium" | "chromium-headless-shell" => Self::Chromium,
      "firefox" => Self::Firefox,
      "webkit" => Self::WebKit,
      _ => return None,
    })
  }
}

/// A version `install` cleared out of the cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovedVersion {
  pub version: String,
  pub path: PathBuf,
}

/// Where a version sorts among the others: by its runs of digits as
/// numbers, so `99.0` comes before `155.0` and `158.0` before `158.0b5`.
/// Comparing the names as text puts `99` after `155`.
fn version_order(name: &str) -> (Vec<u64>, String) {
  let numbers = name
    .split(|c: char| !c.is_ascii_digit())
    .filter(|run| !run.is_empty())
    .map(|run| run.parse().unwrap_or(u64::MAX))
    .collect();
  (numbers, name.to_string())
}

// ---------------------------------------------------------------------------
// Installer
// ---------------------------------------------------------------------------

/// Browser installer that downloads Chrome for Testing from Google's CDN.
pub struct BrowserInstaller {
  cache_dir: PathBuf,
  client: std::sync::OnceLock<Client>,
  download_hosts: Vec<String>,
}

impl BrowserInstaller {
  /// # Errors
  /// Safari and its driver are supplied by macOS; other hosts require a remote endpoint.
  pub async fn install_safari<F>(&self, progress: F) -> Result<String>
  where
    F: Fn(InstallProgress) + Send + Sync,
  {
    if !cfg!(target_os = "macos") {
      return Err(FerriError::unsupported(
        "Safari and safaridriver are bundled with macOS; use a remote Safari endpoint on this platform",
      ));
    }
    let path = "/usr/bin/safaridriver";
    let output = tokio::time::timeout(
      std::time::Duration::from_secs(10),
      tokio::process::Command::new(path)
        .arg("--version")
        .kill_on_drop(true)
        .output(),
    )
    .await
    .map_err(|_| FerriError::timeout("detecting safaridriver", 10_000))??;
    if !output.status.success() {
      return Err(FerriError::backend(format!(
        "safaridriver detection failed: {}",
        String::from_utf8_lossy(&output.stderr)
      )));
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if version.is_empty() {
      return Err(FerriError::protocol(
        "safaridriver --version",
        "driver returned no version",
      ));
    }
    progress(InstallProgress::AlreadyInstalled {
      version,
      path: path.to_owned(),
    });
    Ok(path.to_owned())
  }

  /// Create a new installer with the default cache directory.
  ///
  /// Cache locations:
  /// - Linux: `~/.cache/ferridriver/`
  /// - macOS: `~/Library/Caches/ferridriver/`
  /// - Windows: `%LOCALAPPDATA%/ferridriver/`
  ///
  /// Override with `FERRIDRIVER_BROWSERS_PATH` env var.
  #[must_use]
  pub fn new() -> Self {
    Self::with_cache_dir(default_cache_dir())
  }

  /// Create an installer with a custom cache directory.
  #[must_use]
  pub fn with_cache_dir(cache_dir: PathBuf) -> Self {
    Self {
      cache_dir,
      client: std::sync::OnceLock::new(),
      download_hosts: PW_CDN_MIRRORS.iter().map(|host| (*host).to_string()).collect(),
    }
  }

  /// Download Playwright's builds from `hosts`, tried in order, instead of
  /// Playwright's CDN mirrors. What `PLAYWRIGHT_DOWNLOAD_HOST` does for
  /// Playwright: a mirror inside a network that cannot reach the CDN, or a
  /// local server under test. Each host serves `builds/<browser>/<revision>/<archive>`.
  #[must_use]
  pub fn with_download_hosts(mut self, hosts: Vec<String>) -> Self {
    self.download_hosts = hosts;
    self
  }

  /// The HTTP client, built on first use. Building it loads the system's CA
  /// certificates, which a bare image only gains from
  /// [`Self::install_system_deps`], so it waits until a download needs it.
  fn client(&self) -> Result<&Client> {
    if let Some(client) = self.client.get() {
      return Ok(client);
    }
    let built = Client::builder()
      .user_agent(concat!("ferridriver/", env!("CARGO_PKG_VERSION")))
      .build()
      .map_err(|e| backend_err(format!("no HTTP client for downloads: {e}")))?;
    Ok(self.client.get_or_init(|| built))
  }

  /// Return the cache directory path.
  #[must_use]
  pub fn cache_dir(&self) -> &Path {
    &self.cache_dir
  }

  /// Remove the versions of `family` that `kept` replaced: the ones
  /// that sort before it. Playwright's `install` removes the browsers no
  /// installation refers to any more (`--no-remove` keeps them); here a
  /// launch takes the newest version in the cache, so an older one is
  /// what nothing will run again. A version newer than `kept` is what a
  /// launch would pick, and is left alone.
  ///
  /// Only directories of this family, directly in the cache, are
  /// touched; a symlink is not followed or removed.
  ///
  /// # Errors
  ///
  /// Returns an error if a version that should go cannot be removed.
  pub async fn remove_older_versions(&self, family: CacheFamily, kept: &str) -> Result<Vec<RemovedVersion>> {
    let root = family.root(&self.cache_dir);
    let Ok(entries) = std::fs::read_dir(&root) else {
      return Ok(Vec::new());
    };
    let kept_order = version_order(kept);
    let mut removed = Vec::new();
    for entry in entries.filter_map(std::result::Result::ok) {
      let name = entry.file_name().to_string_lossy().into_owned();
      let Some(version) = family.version_of(&name) else {
        continue;
      };
      let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
      if !is_dir || version == kept || version_order(version) >= kept_order {
        continue;
      }
      let path = entry.path();
      tokio::fs::remove_dir_all(&path)
        .await
        .map_err(|e| backend_err(format!("could not remove {}: {e}", path.display())))?;
      removed.push(RemovedVersion {
        version: version.to_string(),
        path,
      });
    }
    removed.sort_by_key(|gone| version_order(&gone.version));
    Ok(removed)
  }

  /// Install the latest stable Chromium.
  ///
  /// Downloads from Chrome for Testing CDN, extracts to the cache directory,
  /// and returns the absolute path to the chrome executable.
  ///
  /// If the version is already installed (marker file exists), skips the download.
  /// Retries up to 5 times on download failure (matching Playwright behavior).
  ///
  /// # Errors
  ///
  /// Returns an error if the Chrome for Testing API is unreachable, the download
  /// fails after all retries, extraction fails, or the platform is unsupported.
  pub async fn install_chromium<F>(&self, progress: F) -> Result<String>
  where
    F: Fn(InstallProgress),
  {
    progress(InstallProgress::Resolving);

    // Fetch the latest stable version info
    let cft: CftResponse = self
      .client()?
      .get(CFT_VERSIONS_URL)
      .send()
      .await
      .map_err(|e| backend_err(format!("failed to fetch Chrome for Testing versions: {e}")))?
      .json()
      .await
      .map_err(|e| backend_err(format!("failed to parse Chrome for Testing response: {e}")))?;

    let version = &cft.channels.stable.version;
    let platform = current_platform();

    // Find the download URL for our platform
    let download = cft
      .channels
      .stable
      .downloads
      .chrome
      .iter()
      .find(|d| d.platform == platform)
      .ok_or_else(|| FerriError::unsupported(format!("no Chrome for Testing build for platform: {platform}")))?;

    // Check if already installed via marker file (matches Playwright's .downloaded marker)
    let install_dir = self.cache_dir.join(format!("chromium-{version}"));
    let marker_file = install_dir.join(".downloaded");
    let executable = chrome_executable_path(&install_dir, &platform);

    if marker_file.exists() && executable.exists() {
      let path = executable.to_string_lossy().to_string();
      progress(InstallProgress::AlreadyInstalled {
        version: version.clone(),
        path: path.clone(),
      });
      return Ok(path);
    }

    // Clean up partial install if exists
    if install_dir.exists() {
      let _ = tokio::fs::remove_dir_all(&install_dir).await;
    }

    // Download with retries (matching Playwright's 5-attempt strategy)
    let tmp_dir = self.cache_dir.join(".tmp");
    tokio::fs::create_dir_all(&tmp_dir).await?;

    let zip_path = tmp_dir.join(format!("chrome-{version}-{platform}.zip"));
    let mut last_error = String::new();

    for attempt in 1..=DOWNLOAD_RETRIES {
      progress(InstallProgress::Downloading {
        bytes_downloaded: 0,
        total_bytes: None,
      });

      match self.download_file(&download.url, &zip_path, &progress).await {
        Ok(()) => {
          last_error.clear();
          break;
        },
        Err(e) => {
          last_error = format!("attempt {attempt}/{DOWNLOAD_RETRIES}: {e}");
          let _ = tokio::fs::remove_file(&zip_path).await;
          if attempt == DOWNLOAD_RETRIES {
            return Err(backend_err(format!(
              "download failed after {DOWNLOAD_RETRIES} attempts: {last_error}"
            )));
          }
        },
      }
    }

    // Extract
    progress(InstallProgress::Extracting);

    tokio::fs::create_dir_all(&install_dir).await?;

    let install_dir_clone = install_dir.clone();
    let zip_path_clone = zip_path.clone();
    tokio::task::spawn_blocking(move || extract_zip(&zip_path_clone, &install_dir_clone))
      .await
      .map_err(|e| backend_err(format!("extract task failed: {e}")))??;

    // Clean up temp file
    let _ = tokio::fs::remove_file(&zip_path).await;

    // Set executable permissions on Unix
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt;
      if executable.exists() {
        let _ = std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755));
      }
    }

    let path = executable.to_string_lossy().to_string();
    if !executable.exists() {
      return Err(backend_err(format!(
        "extraction completed but chrome executable not found at: {path}"
      )));
    }

    // Write marker file (matches Playwright's .downloaded marker)
    let _ = tokio::fs::write(&marker_file, version.as_bytes()).await;

    progress(InstallProgress::Complete {
      version: version.clone(),
      path: path.clone(),
    });

    Ok(path)
  }

  /// Where axe-core lives once installed.
  ///
  /// Alongside the browsers rather than inside the binary: it is a
  /// third-party artifact under a different licence to this repository,
  /// and it is fetched the same way and to the same place as every
  /// other third-party artifact ferridriver needs.
  #[must_use]
  pub fn axe_core_path(&self) -> PathBuf {
    self
      .cache_dir
      .join(format!("axe-core-{AXE_CORE_VERSION}"))
      .join("axe.min.js")
  }

  /// Install axe-core, the engine behind every accessibility check.
  ///
  /// Returns the path to the script. Already-installed is decided by
  /// the digest and not by the file merely existing, so a truncated
  /// download is re-fetched rather than injected into a page.
  ///
  /// # Errors
  ///
  /// Returns an error if the download fails after all retries, or if
  /// what arrived does not match `AXE_CORE_SHA256`.
  pub async fn install_axe_core<F>(&self, progress: F) -> Result<String>
  where
    F: Fn(InstallProgress),
  {
    let dest = self.axe_core_path();
    let display = dest.display().to_string();
    if let Ok(existing) = tokio::fs::read(&dest).await
      && sha256_hex(&existing) == AXE_CORE_SHA256
    {
      progress(InstallProgress::AlreadyInstalled {
        version: AXE_CORE_VERSION.to_string(),
        path: display.clone(),
      });
      return Ok(display);
    }

    progress(InstallProgress::Resolving);
    if let Some(parent) = dest.parent() {
      tokio::fs::create_dir_all(parent).await?;
    }
    let url = format!("https://cdn.jsdelivr.net/npm/axe-core@{AXE_CORE_VERSION}/axe.min.js");

    let mut last: Option<FerriError> = None;
    for _ in 0..DOWNLOAD_RETRIES {
      match self.download_file(&url, &dest, &progress).await {
        Ok(()) => {
          let bytes = tokio::fs::read(&dest).await?;
          let digest = sha256_hex(&bytes);
          if digest == AXE_CORE_SHA256 {
            progress(InstallProgress::Complete {
              version: AXE_CORE_VERSION.to_string(),
              path: display.clone(),
            });
            return Ok(display);
          }
          let _ = tokio::fs::remove_file(&dest).await;
          last = Some(backend_err(format!(
            "axe-core {AXE_CORE_VERSION} digest mismatch: expected {AXE_CORE_SHA256}, got {digest}"
          )));
        },
        Err(e) => last = Some(e),
      }
    }
    Err(last.unwrap_or_else(|| backend_err("axe-core download failed")))
  }

  /// Download a file with streaming progress.
  async fn download_file<F>(&self, url: &str, dest: &Path, progress: &F) -> Result<()>
  where
    F: Fn(InstallProgress),
  {
    let response = self
      .client()?
      .get(url)
      .send()
      .await
      .map_err(|e| backend_err(format!("request failed: {e}")))?;

    if !response.status().is_success() {
      return Err(backend_err(format!("HTTP {}: {url}", response.status())));
    }

    let total_bytes = response.content_length();
    let mut bytes_downloaded: u64 = 0;

    let mut file = tokio::fs::File::create(dest).await?;

    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
      let chunk = chunk.map_err(|e| backend_err(format!("download error: {e}")))?;
      file.write_all(&chunk).await?;
      bytes_downloaded += chunk.len() as u64;
      progress(InstallProgress::Downloading {
        bytes_downloaded,
        total_bytes,
      });
    }
    file.flush().await?;

    Ok(())
  }

  /// Download `url` to `dest`, retrying, and keep it only if its SHA-256
  /// is `sha256`.
  async fn download_verified<F>(&self, url: &str, dest: &Path, sha256: &str, product: &str, progress: &F) -> Result<()>
  where
    F: Fn(InstallProgress),
  {
    let mut last_error = String::new();
    for attempt in 1..=DOWNLOAD_RETRIES {
      match self.download_file(url, dest, progress).await {
        Ok(()) => {
          last_error.clear();
          break;
        },
        Err(error) => {
          let _ = tokio::fs::remove_file(dest).await;
          last_error = format!("attempt {attempt}/{DOWNLOAD_RETRIES}: {error}");
        },
      }
    }
    if !last_error.is_empty() {
      return Err(backend_err(format!(
        "{product} download failed after {DOWNLOAD_RETRIES} attempts: {last_error}"
      )));
    }
    let path = dest.to_path_buf();
    let digest = tokio::task::spawn_blocking(move || -> std::io::Result<String> {
      use sha2::{Digest, Sha256};
      use std::io::Read as _;
      let mut hasher = Sha256::new();
      let mut file = std::fs::File::open(&path)?;
      let mut chunk = vec![0; 1 << 20];
      loop {
        let read = file.read(&mut chunk)?;
        if read == 0 {
          break;
        }
        hasher.update(&chunk[..read]);
      }
      Ok(
        hasher
          .finalize()
          .iter()
          .fold(String::with_capacity(64), |mut out, byte| {
            use std::fmt::Write as _;
            let _ = write!(out, "{byte:02x}");
            out
          }),
      )
    })
    .await
    .map_err(|e| backend_err(format!("hashing task failed: {e}")))??;
    if !digest.eq_ignore_ascii_case(sha256) {
      let _ = tokio::fs::remove_file(dest).await;
      return Err(backend_err(format!(
        "{product} download {url} has SHA-256 {digest}, expected {sha256}"
      )));
    }
    Ok(())
  }

  /// Install the system packages `browsers` need on Linux, plus the
  /// virtual display and fonts every browser uses.
  ///
  /// Detects the distribution from `/etc/os-release` and installs with
  /// `apt-get` or `pacman`, through `sudo` unless already root. The same as
  /// `npx playwright install-deps <browsers>`. Other platforms need nothing.
  ///
  /// # Errors
  ///
  /// Returns an error if there is no package list for this distribution or
  /// the package manager fails.
  #[cfg(target_os = "linux")]
  pub async fn install_system_deps<F>(&self, browsers: &[DependencyGroup], progress: F) -> Result<()>
  where
    F: Fn(InstallProgress),
  {
    let os_release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let platform = linux::host_platform(&os_release, std::env::consts::ARCH);
    let mut groups = vec![DependencyGroup::Tools];
    groups.extend_from_slice(browsers);
    let (manager, packages) = linux::packages(&platform, &groups)?;

    progress(InstallProgress::InstallingDeps {
      distro: platform.clone(),
    });

    let (tool, commands) = match manager {
      linux::PackageManager::Apt => (
        "apt-get",
        format!(
          "apt-get update && apt-get install -y --no-install-recommends {}",
          packages.join(" ")
        ),
      ),
      linux::PackageManager::Pacman => (
        "pacman",
        format!("pacman -Sy --noconfirm --needed {}", packages.join(" ")),
      ),
    };

    let (cmd, args) = if rustix::process::getuid().is_root() {
      ("sh".to_string(), vec!["-c".to_string(), commands])
    } else {
      (
        "sudo".to_string(),
        vec!["--".to_string(), "sh".to_string(), "-c".to_string(), commands],
      )
    };

    let status = tokio::process::Command::new(&cmd)
      .args(&args)
      .stdin(std::process::Stdio::inherit())
      .stdout(std::process::Stdio::inherit())
      .stderr(std::process::Stdio::inherit())
      .status()
      .await?;

    if !status.success() {
      return Err(backend_err(format!(
        "{tool} exited with code: {}",
        status.code().unwrap_or(-1)
      )));
    }

    progress(InstallProgress::DepsInstalled);
    Ok(())
  }

  /// No system dependencies are required on this platform.
  #[cfg(not(target_os = "linux"))]
  pub fn install_system_deps<F>(
    &self,
    browsers: &[DependencyGroup],
    progress: F,
  ) -> impl std::future::Future<Output = Result<()>>
  where
    F: Fn(InstallProgress),
  {
    futures::FutureExt::map(std::future::ready((self, browsers, progress)), |_| Ok(()))
  }

  /// Return the path to an installed chromium, or `None` if not installed.
  #[must_use]
  pub fn find_installed_chromium(&self) -> Option<String> {
    let entries = std::fs::read_dir(&self.cache_dir).ok()?;
    let mut candidates: Vec<_> = entries
      .filter_map(std::result::Result::ok)
      // `chromium-headless-shell-*` also starts with `chromium-`, and it is a
      // different binary with a different layout.
      .filter(|e| {
        let name = e.file_name().to_string_lossy().into_owned();
        name.starts_with("chromium-") && !name.starts_with("chromium-headless-shell-")
      })
      .collect();
    // Newest version first.
    candidates.sort_by_key(|entry| std::cmp::Reverse(version_order(&entry.file_name().to_string_lossy())));

    let platform = current_platform();
    for entry in candidates {
      let marker = entry.path().join(".downloaded");
      let exe = chrome_executable_path(&entry.path(), &platform);
      if marker.exists() && exe.exists() {
        return Some(exe.to_string_lossy().to_string());
      }
    }
    None
  }

  /// Install the latest stable Chrome Headless Shell.
  ///
  /// Downloads the purpose-built headless-only Chrome binary from Chrome for Testing CDN.
  /// This binary is optimized for headless automation (faster startup, less memory than
  /// full Chrome). Same binary that Playwright uses when `headless: true`.
  ///
  /// Follows the same cache/marker/retry pattern as `install_chromium()`.
  /// Installs to `{cache}/chromium-headless-shell-{version}/`.
  ///
  /// # Errors
  ///
  /// Returns an error if the Chrome for Testing API is unreachable, the download
  /// fails after all retries, extraction fails, or the platform is unsupported.
  pub async fn install_chromium_headless_shell<F>(&self, progress: F) -> Result<String>
  where
    F: Fn(InstallProgress),
  {
    progress(InstallProgress::Resolving);

    let cft: CftResponse = self
      .client()?
      .get(CFT_VERSIONS_URL)
      .send()
      .await
      .map_err(|e| backend_err(format!("failed to fetch Chrome for Testing versions: {e}")))?
      .json()
      .await
      .map_err(|e| backend_err(format!("failed to parse Chrome for Testing response: {e}")))?;

    let version = &cft.channels.stable.version;
    let platform = current_platform();

    let download = cft
      .channels
      .stable
      .downloads
      .chrome_headless_shell
      .iter()
      .find(|d| d.platform == platform)
      .ok_or_else(|| FerriError::unsupported(format!("no Chrome Headless Shell build for platform: {platform}")))?;

    let install_dir = self.cache_dir.join(format!("chromium-headless-shell-{version}"));
    let marker_file = install_dir.join(".downloaded");
    let executable = headless_shell_executable_path(&install_dir, &platform);

    if marker_file.exists() && executable.exists() {
      let path = executable.to_string_lossy().to_string();
      progress(InstallProgress::AlreadyInstalled {
        version: version.clone(),
        path: path.clone(),
      });
      return Ok(path);
    }

    if install_dir.exists() {
      let _ = tokio::fs::remove_dir_all(&install_dir).await;
    }

    let tmp_dir = self.cache_dir.join(".tmp");
    tokio::fs::create_dir_all(&tmp_dir).await?;

    let zip_path = tmp_dir.join(format!("chrome-headless-shell-{version}-{platform}.zip"));
    let mut last_error = String::new();

    for attempt in 1..=DOWNLOAD_RETRIES {
      progress(InstallProgress::Downloading {
        bytes_downloaded: 0,
        total_bytes: None,
      });

      match self.download_file(&download.url, &zip_path, &progress).await {
        Ok(()) => {
          last_error.clear();
          break;
        },
        Err(e) => {
          last_error = format!("attempt {attempt}/{DOWNLOAD_RETRIES}: {e}");
          let _ = tokio::fs::remove_file(&zip_path).await;
          if attempt == DOWNLOAD_RETRIES {
            return Err(backend_err(format!(
              "download failed after {DOWNLOAD_RETRIES} attempts: {last_error}"
            )));
          }
        },
      }
    }

    progress(InstallProgress::Extracting);

    tokio::fs::create_dir_all(&install_dir).await?;

    let install_dir_clone = install_dir.clone();
    let zip_path_clone = zip_path.clone();
    tokio::task::spawn_blocking(move || extract_zip(&zip_path_clone, &install_dir_clone))
      .await
      .map_err(|e| backend_err(format!("extract task failed: {e}")))??;

    let _ = tokio::fs::remove_file(&zip_path).await;

    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt;
      if executable.exists() {
        let _ = std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755));
      }
    }

    let path = executable.to_string_lossy().to_string();
    if !executable.exists() {
      return Err(backend_err(format!(
        "extraction completed but chrome-headless-shell executable not found at: {path}"
      )));
    }

    let _ = tokio::fs::write(&marker_file, version.as_bytes()).await;

    progress(InstallProgress::Complete {
      version: version.clone(),
      path: path.clone(),
    });

    Ok(path)
  }

  /// Return the path to an installed Chrome Headless Shell, or `None` if not installed.
  #[must_use]
  pub fn find_installed_headless_shell(&self) -> Option<String> {
    let entries = std::fs::read_dir(&self.cache_dir).ok()?;
    let mut candidates: Vec<_> = entries
      .filter_map(std::result::Result::ok)
      .filter(|e| e.file_name().to_string_lossy().starts_with("chromium-headless-shell-"))
      .collect();
    candidates.sort_by_key(|entry| std::cmp::Reverse(version_order(&entry.file_name().to_string_lossy())));

    let platform = current_platform();
    for entry in candidates {
      let marker = entry.path().join(".downloaded");
      let exe = headless_shell_executable_path(&entry.path(), &platform);
      if marker.exists() && exe.exists() {
        return Some(exe.to_string_lossy().to_string());
      }
    }
    None
  }

  /// Install the `ChromeDriver` built for the newest Chromium in the cache,
  /// from the Chrome for Testing CDN, and answer its path. A driver only
  /// starts the browser of its own version, so it follows the installed
  /// Chromium rather than the latest stable release; installing that
  /// Chromium first is what `chromium chromedriver` in one command does.
  /// Installs to `{cache}/chromedriver-{version}/`.
  ///
  /// # Errors
  ///
  /// Fails when no Chromium is installed, the download fails after all
  /// retries, or extraction leaves no executable.
  pub async fn install_chromedriver<F>(&self, progress: F) -> Result<String>
  where
    F: Fn(InstallProgress),
  {
    progress(InstallProgress::Resolving);
    let version = self.installed_chromium_version().ok_or_else(|| {
      FerriError::invalid_argument(
        "chromedriver",
        "no Chromium is installed to pair it with: run `ferridriver install chromium chromedriver`",
      )
    })?;
    let platform = current_platform();
    let install_dir = self.cache_dir.join(format!("chromedriver-{version}"));
    let marker_file = install_dir.join(".downloaded");
    let executable = chromedriver_executable_path(&install_dir, &platform);
    if marker_file.exists() && executable.exists() {
      let path = executable.to_string_lossy().to_string();
      progress(InstallProgress::AlreadyInstalled {
        version,
        path: path.clone(),
      });
      return Ok(path);
    }
    // Two installs of the same driver can run at once (test workers that
    // each provision it): each downloads and extracts beside the cache
    // under a name of its own, and the finished directory is renamed into
    // place, so neither ever sees, or removes, the other's half-done work.
    let tmp_dir = self.cache_dir.join(".tmp");
    tokio::fs::create_dir_all(&tmp_dir).await?;
    let own = format!(
      "chromedriver-{version}-{platform}-{}-{}",
      std::process::id(),
      CHROMEDRIVER_STAGED.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let zip_path = tmp_dir.join(format!("{own}.zip"));
    let staging = tmp_dir.join(&own);
    let url = format!("{CFT_DOWNLOADS_URL}/{version}/{platform}/chromedriver-{platform}.zip");
    for attempt in 1..=DOWNLOAD_RETRIES {
      progress(InstallProgress::Downloading {
        bytes_downloaded: 0,
        total_bytes: None,
      });
      match self.download_file(&url, &zip_path, &progress).await {
        Ok(()) => break,
        Err(e) => {
          let _ = tokio::fs::remove_file(&zip_path).await;
          if attempt == DOWNLOAD_RETRIES {
            return Err(backend_err(format!(
              "download failed after {DOWNLOAD_RETRIES} attempts: attempt {attempt}/{DOWNLOAD_RETRIES}: {e}"
            )));
          }
        },
      }
    }

    progress(InstallProgress::Extracting);
    let _ = tokio::fs::remove_dir_all(&staging).await;
    tokio::fs::create_dir_all(&staging).await?;
    let staging_clone = staging.clone();
    let zip_path_clone = zip_path.clone();
    let extracted = tokio::task::spawn_blocking(move || extract_zip(&zip_path_clone, &staging_clone))
      .await
      .map_err(|e| backend_err(format!("extract task failed: {e}")));
    let _ = tokio::fs::remove_file(&zip_path).await;
    if let Err(error) = extracted.and_then(|extracted| extracted) {
      let _ = tokio::fs::remove_dir_all(&staging).await;
      return Err(error);
    }
    let staged = chromedriver_executable_path(&staging, &platform);
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt;
      if staged.exists() {
        let _ = std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755));
      }
    }
    if !staged.exists() {
      let _ = tokio::fs::remove_dir_all(&staging).await;
      return Err(backend_err(format!(
        "extraction completed but chromedriver executable not found at: {}",
        staged.display()
      )));
    }
    tokio::fs::write(staging.join(".downloaded"), version.as_bytes()).await?;
    // A directory left without its marker by an install that died is
    // replaced; one another install just finished is kept.
    if install_dir.exists() && !marker_file.exists() {
      let _ = tokio::fs::remove_dir_all(&install_dir).await;
    }
    if tokio::fs::rename(&staging, &install_dir).await.is_err() {
      let _ = tokio::fs::remove_dir_all(&staging).await;
    }
    let path = executable.to_string_lossy().to_string();
    if !(marker_file.exists() && executable.exists()) {
      return Err(backend_err(format!("chromedriver was not installed at: {path}")));
    }
    progress(InstallProgress::Complete {
      version,
      path: path.clone(),
    });
    Ok(path)
  }

  /// The version of the newest Chromium installed in the cache.
  fn installed_chromium_version(&self) -> Option<String> {
    let chromium = PathBuf::from(self.find_installed_chromium()?);
    let platform = current_platform();
    self
      .cache_dir
      .read_dir()
      .ok()?
      .filter_map(std::result::Result::ok)
      .find(|entry| chrome_executable_path(&entry.path(), &platform) == chromium)
      .and_then(|entry| {
        CacheFamily::Chromium
          .version_of(&entry.file_name().to_string_lossy())
          .map(str::to_string)
      })
  }

  /// The `ChromeDriver` installed for the newest installed Chromium, if
  /// there is one.
  #[must_use]
  pub fn find_installed_chromedriver(&self) -> Option<String> {
    let version = self.installed_chromium_version()?;
    let install_dir = self.cache_dir.join(format!("chromedriver-{version}"));
    let executable = chromedriver_executable_path(&install_dir, &current_platform());
    (install_dir.join(".downloaded").exists() && executable.exists()).then(|| executable.to_string_lossy().to_string())
  }

  /// Install the latest stable Firefox.
  ///
  /// Downloads from Mozilla's official archive, extracts to the cache directory,
  /// and returns the absolute path to the firefox executable.
  ///
  /// Uses the same cache/marker/retry pattern as `install_chromium()`.
  /// Archive format: `.tar.bz2` on Linux, `.dmg` on macOS (mounted via hdiutil).
  ///
  /// # Errors
  ///
  /// Returns an error if the Mozilla API is unreachable, the download
  /// fails after all retries, extraction fails, or the platform is unsupported.
  pub async fn install_firefox<F>(&self, progress: F) -> Result<String>
  where
    F: Fn(InstallProgress),
  {
    progress(InstallProgress::Resolving);

    // Fetch the latest stable Firefox version from Mozilla's API
    let versions: rustc_hash::FxHashMap<String, String> = self
      .client()?
      .get(FIREFOX_VERSIONS_URL)
      .send()
      .await
      .map_err(|e| backend_err(format!("failed to fetch Firefox versions: {e}")))?
      .json()
      .await
      .map_err(|e| backend_err(format!("failed to parse Firefox versions: {e}")))?;

    let version = versions
      .get("LATEST_FIREFOX_VERSION")
      .ok_or_else(|| backend_err("Firefox versions response missing LATEST_FIREFOX_VERSION"))?
      .clone();

    let (platform_dir, archive_name, archive_ext) = firefox_archive_info(&version)?;

    // Build download URL:
    // https://archive.mozilla.org/pub/firefox/releases/{version}/{platform}/en-US/{archive}
    let download_url = format!("{FIREFOX_RELEASES_URL}/{version}/{platform_dir}/en-US/{archive_name}");

    // Check if already installed
    let install_dir = self.cache_dir.join(format!("firefox-{version}"));
    let marker_file = install_dir.join(".downloaded");
    let executable = firefox_executable_path(&install_dir);

    if marker_file.exists() && executable.exists() {
      let path = executable.to_string_lossy().to_string();
      progress(InstallProgress::AlreadyInstalled {
        version: version.clone(),
        path: path.clone(),
      });
      return Ok(path);
    }

    // Clean up partial install
    if install_dir.exists() {
      let _ = tokio::fs::remove_dir_all(&install_dir).await;
    }

    // Download with retries
    let tmp_dir = self.cache_dir.join(".tmp");
    tokio::fs::create_dir_all(&tmp_dir).await?;

    let archive_path = tmp_dir.join(format!("firefox-{version}{archive_ext}"));
    let mut last_error = String::new();

    for attempt in 1..=DOWNLOAD_RETRIES {
      progress(InstallProgress::Downloading {
        bytes_downloaded: 0,
        total_bytes: None,
      });

      match self.download_file(&download_url, &archive_path, &progress).await {
        Ok(()) => {
          last_error.clear();
          break;
        },
        Err(e) => {
          last_error = format!("attempt {attempt}/{DOWNLOAD_RETRIES}: {e}");
          let _ = tokio::fs::remove_file(&archive_path).await;
          if attempt == DOWNLOAD_RETRIES {
            return Err(backend_err(format!(
              "Firefox download failed after {DOWNLOAD_RETRIES} attempts: {last_error}"
            )));
          }
        },
      }
    }

    // Extract
    progress(InstallProgress::Extracting);

    tokio::fs::create_dir_all(&install_dir).await?;

    let install_dir_clone = install_dir.clone();
    let archive_path_clone = archive_path.clone();
    tokio::task::spawn_blocking(move || extract_firefox_archive(&archive_path_clone, &install_dir_clone))
      .await
      .map_err(|e| backend_err(format!("extract task failed: {e}")))??;

    // Clean up temp file
    let _ = tokio::fs::remove_file(&archive_path).await;

    // Set executable permissions on Unix
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt;
      if executable.exists() {
        let _ = std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755));
      }
    }

    let path = executable.to_string_lossy().to_string();
    if !executable.exists() {
      return Err(backend_err(format!(
        "extraction completed but firefox executable not found at: {path}"
      )));
    }

    // Write marker file
    let _ = tokio::fs::write(&marker_file, version.as_bytes()).await;

    progress(InstallProgress::Complete {
      version: version.clone(),
      path: path.clone(),
    });

    Ok(path)
  }
}

/// ferridriver's own browser cache: `FERRIDRIVER_BROWSERS_PATH`, or
/// `ferridriver` under the platform cache directory.
#[must_use]
pub fn default_cache_dir() -> PathBuf {
  std::env::var_os("FERRIDRIVER_BROWSERS_PATH").map_or_else(
    || {
      dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from(".cache"))
        .join("ferridriver")
    },
    PathBuf::from,
  )
}

impl Default for BrowserInstaller {
  fn default() -> Self {
    Self::new()
  }
}
