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
mod ios;
#[cfg(any(target_os = "linux", test))]
mod linux;

use std::path::{Path, PathBuf};

use futures::StreamExt;
use reqwest::Client;
use serde::Deserialize;
use tokio::io::AsyncWriteExt;

use crate::error::{FerriError, Result};

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

/// Playwright's `browsers.json` on main — single source of truth for the
/// `WebKit` build revision.
const PW_BROWSERS_JSON_URL: &str =
  "https://raw.githubusercontent.com/microsoft/playwright/main/packages/playwright-core/browsers.json";

/// Playwright CDN mirrors. Try in order; on connection failure, fall over.
const PW_CDN_MIRRORS: &[&str] = &[
  "https://cdn.playwright.dev/dbazure/download/playwright",
  "https://playwright.download.prss.microsoft.com/dbazure/download/playwright",
  "https://cdn.playwright.dev",
];

#[derive(Debug, Deserialize)]
struct PwBrowsersJson {
  browsers: Vec<PwBrowserEntry>,
}

#[derive(Debug, Deserialize)]
struct PwBrowserEntry {
  name: String,
  revision: String,
}

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
  Firefox,
  WebKit,
  AxeCore,
}

impl CacheFamily {
  /// The family `ferridriver install <target>` adds to, if it has one.
  /// Safari, Android and iOS are not versioned directories of the cache.
  #[must_use]
  pub fn of_target(target: &str) -> Option<Self> {
    Some(match target {
      "chromium" => Self::Chromium,
      "chromium-headless-shell" => Self::ChromiumHeadlessShell,
      "firefox" => Self::Firefox,
      "webkit" => Self::WebKit,
      "axe" => Self::AxeCore,
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
      Self::Firefox => "firefox-",
      Self::WebKit => "webkit-",
      Self::AxeCore => "axe-core-",
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
  client: Client,
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
    let cache_dir = if let Ok(p) = std::env::var("FERRIDRIVER_BROWSERS_PATH") {
      PathBuf::from(p)
    } else {
      dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from(".cache"))
        .join("ferridriver")
    };

    Self {
      cache_dir,
      client: Client::new(),
    }
  }

  /// Create an installer with a custom cache directory.
  #[must_use]
  pub fn with_cache_dir(cache_dir: PathBuf) -> Self {
    Self {
      cache_dir,
      client: Client::new(),
    }
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
      .client
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
      .client
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

    // Determine if we need sudo (getuid is always safe, just FFI-marked unsafe).
    #[allow(unsafe_code)]
    let uid = unsafe { libc::getuid() };
    let (cmd, args) = if uid == 0 {
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
      .client
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
    let versions: std::collections::HashMap<String, String> = self
      .client
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

  /// Install the Playwright `WebKit` binary.
  ///
  /// Reads the current `WebKit` revision from Playwright's `browsers.json`
  /// (latest main), downloads the matching archive from the Playwright
  /// CDN, and extracts it to `~/.cache/ferridriver/webkit/webkit-<rev>/`.
  ///
  /// The launcher search order picks this up automatically. Override
  /// with the `FERRIDRIVER_WEBKIT` environment variable when needed.
  ///
  /// # Errors
  ///
  /// Returns an error if Playwright's `browsers.json` is unreachable,
  /// the host platform has no published `WebKit` build, the download fails
  /// after all retries, or extraction fails.
  pub async fn install_webkit<F>(&self, progress: F) -> Result<String>
  where
    F: Fn(InstallProgress),
  {
    progress(InstallProgress::Resolving);

    let browsers: PwBrowsersJson = self
      .client
      .get(PW_BROWSERS_JSON_URL)
      .send()
      .await
      .map_err(|e| backend_err(format!("failed to fetch Playwright browsers.json: {e}")))?
      .json()
      .await
      .map_err(|e| backend_err(format!("failed to parse Playwright browsers.json: {e}")))?;

    let revision = browsers
      .browsers
      .into_iter()
      .find(|b| b.name == "webkit")
      .map(|b| b.revision)
      .ok_or_else(|| backend_err("Playwright browsers.json missing webkit entry"))?;

    let archive = webkit_archive_for_host()?;

    let webkit_root = self.cache_dir.join("webkit");
    let install_dir = webkit_root.join(format!("webkit-{revision}"));
    let marker_file = install_dir.join(".downloaded");
    let executable = install_dir.join("pw_run.sh");

    if marker_file.exists() && executable.exists() {
      let path = executable.to_string_lossy().to_string();
      progress(InstallProgress::AlreadyInstalled {
        version: revision.clone(),
        path: path.clone(),
      });
      return Ok(path);
    }

    if install_dir.exists() {
      let _ = tokio::fs::remove_dir_all(&install_dir).await;
    }

    let tmp_dir = self.cache_dir.join(".tmp");
    tokio::fs::create_dir_all(&tmp_dir).await?;
    let zip_path = tmp_dir.join(format!("webkit-{revision}.zip"));

    let mut last_error = String::new();
    'outer: for host in PW_CDN_MIRRORS {
      let url = format!("{host}/builds/webkit/{revision}/{archive}");
      for attempt in 1..=DOWNLOAD_RETRIES {
        progress(InstallProgress::Downloading {
          bytes_downloaded: 0,
          total_bytes: None,
        });
        match self.download_file(&url, &zip_path, &progress).await {
          Ok(()) => {
            last_error.clear();
            break 'outer;
          },
          Err(e) => {
            last_error = format!("{host} attempt {attempt}/{DOWNLOAD_RETRIES}: {e}");
            let _ = tokio::fs::remove_file(&zip_path).await;
          },
        }
      }
    }
    if !last_error.is_empty() {
      return Err(backend_err(format!(
        "WebKit download failed after exhausting mirrors: {last_error}"
      )));
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

    if !executable.exists() {
      return Err(backend_err(format!(
        "extraction completed but pw_run.sh not found at: {}",
        executable.display()
      )));
    }

    let _ = tokio::fs::write(&marker_file, revision.as_bytes()).await;

    let path = executable.to_string_lossy().to_string();
    progress(InstallProgress::Complete {
      version: revision,
      path: path.clone(),
    });
    Ok(path)
  }

  /// Download Playwright's Android driver: the two APKs `_android` installs
  /// on a device for its selector actions and its `input`. The same
  /// archive, from the same mirrors, that `playwright install android`
  /// fetches.
  ///
  /// # Errors
  ///
  /// Returns an error if the registry or every mirror is unreachable, or
  /// the archive does not hold both APKs.
  pub async fn install_android_driver<F>(&self, progress: F) -> Result<String>
  where
    F: Fn(InstallProgress),
  {
    progress(InstallProgress::Resolving);
    let browsers: PwBrowsersJson = self
      .client
      .get(PW_BROWSERS_JSON_URL)
      .send()
      .await
      .map_err(|e| backend_err(format!("failed to fetch Playwright browsers.json: {e}")))?
      .json()
      .await
      .map_err(|e| backend_err(format!("failed to parse Playwright browsers.json: {e}")))?;
    let revision = browsers
      .browsers
      .into_iter()
      .find(|b| b.name == "android")
      .map(|b| b.revision)
      .ok_or_else(|| backend_err("Playwright browsers.json missing android entry"))?;

    let install_dir = self.cache_dir.join(format!("android-driver-{revision}"));
    let marker_file = install_dir.join(".downloaded");
    let complete = |dir: &Path| ANDROID_DRIVER_APKS.iter().all(|apk| dir.join(apk).exists());
    if marker_file.exists() && complete(&install_dir) {
      let path = install_dir.to_string_lossy().to_string();
      progress(InstallProgress::AlreadyInstalled {
        version: revision,
        path: path.clone(),
      });
      return Ok(path);
    }
    if install_dir.exists() {
      let _ = tokio::fs::remove_dir_all(&install_dir).await;
    }

    let tmp_dir = self.cache_dir.join(".tmp");
    tokio::fs::create_dir_all(&tmp_dir).await?;
    let zip_path = tmp_dir.join(format!("android-driver-{revision}.zip"));
    let mut last_error = String::new();
    'outer: for host in PW_CDN_MIRRORS {
      let url = format!("{host}/builds/android/{revision}/android.zip");
      for attempt in 1..=DOWNLOAD_RETRIES {
        progress(InstallProgress::Downloading {
          bytes_downloaded: 0,
          total_bytes: None,
        });
        match self.download_file(&url, &zip_path, &progress).await {
          Ok(()) => {
            last_error.clear();
            break 'outer;
          },
          Err(e) => {
            last_error = format!("{host} attempt {attempt}/{DOWNLOAD_RETRIES}: {e}");
            let _ = tokio::fs::remove_file(&zip_path).await;
          },
        }
      }
    }
    if !last_error.is_empty() {
      return Err(backend_err(format!(
        "Android driver download failed after exhausting mirrors: {last_error}"
      )));
    }

    progress(InstallProgress::Extracting);
    tokio::fs::create_dir_all(&install_dir).await?;
    let install_dir_clone = install_dir.clone();
    let zip_path_clone = zip_path.clone();
    tokio::task::spawn_blocking(move || extract_zip(&zip_path_clone, &install_dir_clone))
      .await
      .map_err(|e| backend_err(format!("extract task failed: {e}")))??;
    let _ = tokio::fs::remove_file(&zip_path).await;
    if !complete(&install_dir) {
      return Err(backend_err(format!(
        "extraction completed but the driver APKs are not in {}",
        install_dir.display()
      )));
    }
    let _ = tokio::fs::write(&marker_file, revision.as_bytes()).await;

    let path = install_dir.to_string_lossy().to_string();
    progress(InstallProgress::Complete {
      version: revision,
      path: path.clone(),
    });
    Ok(path)
  }

  /// The directory of an installed Android driver, the newest when there
  /// are several, or `None` when [`Self::install_android_driver`] has not
  /// run.
  #[must_use]
  pub fn find_installed_android_driver(&self) -> Option<PathBuf> {
    let entries = std::fs::read_dir(&self.cache_dir).ok()?;
    let mut candidates: Vec<_> = entries
      .filter_map(std::result::Result::ok)
      .filter(|e| e.file_name().to_string_lossy().starts_with("android-driver-"))
      .collect();
    candidates.sort_by_key(|entry| std::cmp::Reverse(version_order(&entry.file_name().to_string_lossy())));
    candidates
      .into_iter()
      .map(|entry| entry.path())
      .find(|dir| dir.join(".downloaded").exists() && ANDROID_DRIVER_APKS.iter().all(|apk| dir.join(apk).exists()))
  }

  /// Return the path to an installed Firefox, or `None` if not installed.
  #[must_use]
  pub fn find_installed_firefox(&self) -> Option<String> {
    let entries = std::fs::read_dir(&self.cache_dir).ok()?;
    let mut candidates: Vec<_> = entries
      .filter_map(std::result::Result::ok)
      .filter(|e| e.file_name().to_string_lossy().starts_with("firefox-"))
      .collect();
    candidates.sort_by_key(|entry| std::cmp::Reverse(version_order(&entry.file_name().to_string_lossy())));

    for entry in candidates {
      let marker = entry.path().join(".downloaded");
      let exe = firefox_executable_path(&entry.path());
      if marker.exists() && exe.exists() {
        return Some(exe.to_string_lossy().to_string());
      }
    }
    None
  }
}

impl Default for BrowserInstaller {
  fn default() -> Self {
    Self::new()
  }
}

// ---------------------------------------------------------------------------
// Platform detection
// ---------------------------------------------------------------------------

/// Map the host to the Playwright `WebKit` archive name. Mirrors the
/// table in `packages/playwright-core/src/server/registry/index.ts`.
fn webkit_archive_for_host() -> Result<String> {
  let os = std::env::consts::OS;
  let arch = std::env::consts::ARCH;

  let archive = match (os, arch) {
    #[cfg(target_os = "linux")]
    ("linux", _) => {
      let os_release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
      let platform = linux::host_platform(&os_release, arch);
      return linux::webkit_archive(&platform)
        .map(str::to_string)
        .ok_or_else(|| FerriError::unsupported(format!("no Playwright WebKit build for {platform}")));
    },
    #[cfg(not(target_os = "linux"))]
    ("linux", "aarch64") => "webkit-ubuntu-24.04-arm64.zip",
    #[cfg(not(target_os = "linux"))]
    ("linux", _) => "webkit-ubuntu-24.04.zip",
    ("macos", "aarch64") => "webkit-mac-15-arm64.zip",
    ("macos", _) => "webkit-mac-15.zip",
    ("windows", _) => "webkit-win64.zip",
    _ => {
      return Err(FerriError::unsupported(format!(
        "no Playwright WebKit build for {os}/{arch}"
      )));
    },
  };
  Ok(archive.to_string())
}

fn current_platform() -> String {
  let os = std::env::consts::OS;
  let arch = std::env::consts::ARCH;

  match (os, arch) {
    ("linux", "x86_64") => "linux64".to_string(),
    ("linux", "aarch64") => "linux-arm64".to_string(),
    ("macos", "x86_64") => "mac-x64".to_string(),
    ("macos", "aarch64") => "mac-arm64".to_string(),
    ("windows", "x86_64") => "win64".to_string(),
    ("windows", "x86") => "win32".to_string(),
    _ => format!("{os}-{arch}"),
  }
}

fn chrome_executable_path(install_dir: &Path, platform: &str) -> PathBuf {
  match platform {
    "linux64" => install_dir.join("chrome-linux64/chrome"),
    "linux-arm64" => install_dir.join("chrome-linux-arm64/chrome"),
    "mac-x64" => {
      install_dir.join("chrome-mac-x64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing")
    },
    "mac-arm64" => {
      install_dir.join("chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing")
    },
    "win64" => install_dir.join("chrome-win64/chrome.exe"),
    "win32" => install_dir.join("chrome-win32/chrome.exe"),
    _ => install_dir.join("chrome"),
  }
}

fn headless_shell_executable_path(install_dir: &Path, platform: &str) -> PathBuf {
  match platform {
    "linux64" => install_dir.join("chrome-headless-shell-linux64/chrome-headless-shell"),
    "linux-arm64" => install_dir.join("chrome-headless-shell-linux-arm64/chrome-headless-shell"),
    "mac-x64" => install_dir.join("chrome-headless-shell-mac-x64/chrome-headless-shell"),
    "mac-arm64" => install_dir.join("chrome-headless-shell-mac-arm64/chrome-headless-shell"),
    "win64" => install_dir.join("chrome-headless-shell-win64/chrome-headless-shell.exe"),
    "win32" => install_dir.join("chrome-headless-shell-win32/chrome-headless-shell.exe"),
    _ => install_dir.join("chrome-headless-shell"),
  }
}

// ---------------------------------------------------------------------------
// Firefox platform helpers
// ---------------------------------------------------------------------------

/// Return (`platform_dir`, `archive_filename`, `archive_extension`) for Firefox downloads.
/// Matches Puppeteer's `archive()` and `platformName()` for stable channel.
fn firefox_archive_info(version: &str) -> Result<(String, String, String)> {
  let os = std::env::consts::OS;
  let arch = std::env::consts::ARCH;

  let major: u32 = version.split('.').next().and_then(|s| s.parse().ok()).unwrap_or(0);
  let tar_ext = if major >= 135 { "xz" } else { "bz2" };

  match (os, arch) {
    ("linux", "x86_64") => Ok((
      "linux-x86_64".into(),
      format!("firefox-{version}.tar.{tar_ext}"),
      format!(".tar.{tar_ext}"),
    )),
    ("linux", "aarch64") => Ok((
      "linux-aarch64".into(),
      format!("firefox-{version}.tar.{tar_ext}"),
      format!(".tar.{tar_ext}"),
    )),
    ("macos", "x86_64" | "aarch64") => Ok(("mac".into(), format!("Firefox {version}.dmg"), ".dmg".into())),
    ("windows", "x86_64") => Ok(("win64".into(), format!("Firefox Setup {version}.exe"), ".exe".into())),
    ("windows", "x86") => Ok(("win32".into(), format!("Firefox Setup {version}.exe"), ".exe".into())),
    _ => Err(FerriError::unsupported(format!(
      "unsupported platform for Firefox: {os}-{arch}"
    ))),
  }
}

/// Return the path to the Firefox executable inside the install directory.
/// Matches Puppeteer's `relativeExecutablePath()` for stable channel.
fn firefox_executable_path(install_dir: &Path) -> PathBuf {
  let os = std::env::consts::OS;
  match os {
    "macos" => install_dir.join("Firefox.app/Contents/MacOS/firefox"),
    "windows" => install_dir.join("core/firefox.exe"),
    // Linux: tar extracts to firefox/ subdirectory
    _ => install_dir.join("firefox/firefox"),
  }
}

/// Extract a Firefox archive (tar.bz2, tar.xz, or DMG).
fn extract_firefox_archive(archive_path: &Path, dest: &Path) -> Result<()> {
  let path_str = archive_path.to_string_lossy();

  if path_str.ends_with(".tar.bz2") {
    extract_tar_bz2(archive_path, dest)
  } else if path_str.ends_with(".tar.xz") {
    extract_tar_xz(archive_path, dest)
  } else if path_str.ends_with(".dmg") {
    extract_dmg(archive_path, dest)
  } else {
    Err(FerriError::unsupported(format!(
      "unsupported Firefox archive format: {path_str}"
    )))
  }
}

/// Extract a .tar.bz2 archive.
fn extract_tar_bz2(archive_path: &Path, dest: &Path) -> Result<()> {
  let file = std::fs::File::open(archive_path)?;
  let decoder = bzip2::read::BzDecoder::new(file);
  let mut archive = tar::Archive::new(decoder);
  archive.unpack(dest)?;
  Ok(())
}

/// Extract a .tar.xz archive.
fn extract_tar_xz(archive_path: &Path, dest: &Path) -> Result<()> {
  let file = std::fs::File::open(archive_path)?;
  let decoder = xz2::read::XzDecoder::new(file);
  let mut archive = tar::Archive::new(decoder);
  archive.unpack(dest)?;
  Ok(())
}

/// Extract a .dmg (macOS disk image) by mounting, copying, and unmounting.
/// Same approach as Puppeteer: hdiutil attach -> cp -R -> hdiutil detach.
fn extract_dmg(dmg_path: &Path, dest: &Path) -> Result<()> {
  // Mount the DMG
  let output = std::process::Command::new("hdiutil")
    .args(["attach", "-nobrowse", "-noautoopen"])
    .arg(dmg_path)
    .output()?;

  if !output.status.success() {
    return Err(backend_err(format!(
      "hdiutil attach failed: {}",
      String::from_utf8_lossy(&output.stderr)
    )));
  }

  let stdout = String::from_utf8_lossy(&output.stdout);
  let mount_path = stdout
    .lines()
    .filter_map(|line| {
      let parts: Vec<&str> = line.split('\t').collect();
      parts.last().map(|p| p.trim().to_string())
    })
    .find(|p| p.starts_with("/Volumes/"))
    .ok_or_else(|| backend_err("could not find volume mount path in hdiutil output"))?;

  // Find the .app directory inside the mount
  let result = (|| -> Result<()> {
    let entries = std::fs::read_dir(&mount_path)?;
    let app_name = entries
      .filter_map(std::result::Result::ok)
      .find(|e| e.file_name().to_string_lossy().ends_with(".app"))
      .ok_or_else(|| backend_err("no .app found in mounted DMG"))?;

    let source = std::path::Path::new(&mount_path).join(app_name.file_name());

    // cp -R to destination
    let status = std::process::Command::new("cp")
      .args(["-R"])
      .arg(&source)
      .arg(dest)
      .status()?;

    if !status.success() {
      return Err(backend_err("cp -R failed to copy Firefox.app"));
    }
    Ok(())
  })();

  // Always detach the DMG
  let _ = std::process::Command::new("hdiutil")
    .args(["detach", &mount_path, "-quiet"])
    .status();

  result
}

// ---------------------------------------------------------------------------
// Zip extraction (for Chrome)
// ---------------------------------------------------------------------------

/// What Playwright's Android driver archive holds: the driver and the
/// instrumentation target it runs in.
pub(crate) const ANDROID_DRIVER_APKS: [&str; 2] = ["android-driver.apk", "android-driver-target.apk"];

fn extract_zip(zip_path: &Path, dest: &Path) -> Result<()> {
  let file = std::fs::File::open(zip_path)?;
  let mut archive = zip::ZipArchive::new(file).map_err(|e| backend_err(format!("failed to read zip archive: {e}")))?;

  for i in 0..archive.len() {
    let mut entry = archive
      .by_index(i)
      .map_err(|e| backend_err(format!("failed to read zip entry {i}: {e}")))?;

    let name = entry.name().to_string();
    let out_path = dest.join(&name);

    if entry.is_dir() {
      std::fs::create_dir_all(&out_path)?;
      continue;
    }

    if let Some(parent) = out_path.parent() {
      std::fs::create_dir_all(parent)?;
    }

    // Symlink entries: file content IS the link target. Required for
    // Playwright WebKit bundles (libwebkitgtk-6.0.so.4 → ...so.4.17.1).
    // ZIP encodes symlinks as regular file entries with the unix mode
    // bits S_IFLNK (0o120000) set.
    #[cfg(unix)]
    {
      use std::io::Read;
      use std::os::unix::fs::PermissionsExt;
      if let Some(mode) = entry.unix_mode() {
        if mode & 0o170_000 == 0o120_000 {
          let mut target = String::new();
          entry.read_to_string(&mut target)?;
          // Replace any pre-existing entry — zip ordering is not stable.
          let _ = std::fs::remove_file(&out_path);
          std::os::unix::fs::symlink(&target, &out_path)?;
          continue;
        }
        let mut out_file = std::fs::File::create(&out_path)?;
        std::io::copy(&mut entry, &mut out_file)?;
        let _ = std::fs::set_permissions(&out_path, std::fs::Permissions::from_mode(mode));
        continue;
      }
    }

    let mut out_file = std::fs::File::create(&out_path)?;
    std::io::copy(&mut entry, &mut out_file)?;
  }

  Ok(())
}

#[cfg(test)]
mod removal_tests {
  use super::{BrowserInstaller, CacheFamily, version_order};

  /// A cache holding `dirs`, each a directory with a file in it.
  fn cache(dirs: &[&str]) -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("cache");
    for dir in dirs {
      std::fs::create_dir_all(root.path().join(dir)).expect("version");
      std::fs::write(root.path().join(dir).join(".downloaded"), "").expect("marker");
    }
    root
  }

  fn left(root: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root)
      .expect("cache")
      .filter_map(Result::ok)
      .map(|entry| entry.file_name().to_string_lossy().into_owned())
      .collect();
    names.sort();
    names
  }

  #[test]
  fn versions_sort_by_their_numbers() {
    let mut versions = vec!["155.0.8059.39", "99.0.1", "158.0b5", "158.0", "157.0.1", "2374"];
    versions.sort_by_key(|v| version_order(v));
    assert_eq!(
      versions,
      ["99.0.1", "155.0.8059.39", "157.0.1", "158.0", "158.0b5", "2374"]
    );
  }

  #[tokio::test]
  async fn only_older_versions_of_the_same_family_go() {
    let root = cache(&[
      "chromium-99.0.4844.51",
      "chromium-154.0.1.2",
      "chromium-155.0.8059.39",
      "chromium-156.0.1.1",
      "chromium-headless-shell-154.0.1.2",
      "chromium-headless-shell-155.0.8059.39",
      "chromedriver-154.0.1.2",
      "firefox-157.0.1",
      "firefox-158.0b5",
      "chromium-",
    ]);
    std::fs::write(root.path().join("chromium-1.0"), "a file, not a version").expect("file");
    let installer = BrowserInstaller::with_cache_dir(root.path().to_path_buf());

    let removed = installer
      .remove_older_versions(CacheFamily::Chromium, "155.0.8059.39")
      .await
      .expect("remove");
    let versions: Vec<&str> = removed.iter().map(|gone| gone.version.as_str()).collect();
    assert_eq!(versions, ["99.0.4844.51", "154.0.1.2"]);
    assert_eq!(removed[1].path, root.path().join("chromium-154.0.1.2"));
    assert_eq!(
      left(root.path()),
      [
        "chromedriver-154.0.1.2",
        "chromium-",
        "chromium-1.0",
        "chromium-155.0.8059.39",
        "chromium-156.0.1.1",
        "chromium-headless-shell-154.0.1.2",
        "chromium-headless-shell-155.0.8059.39",
        "firefox-157.0.1",
        "firefox-158.0b5",
      ]
    );

    // What a launch would pick over the installed one stays.
    let none = installer
      .remove_older_versions(CacheFamily::Firefox, "157.0.1")
      .await
      .expect("remove");
    assert_eq!(none, Vec::new());
    let shell = installer
      .remove_older_versions(CacheFamily::ChromiumHeadlessShell, "155.0.8059.39")
      .await
      .expect("remove");
    assert_eq!(shell.len(), 1);
    assert!(!root.path().join("chromium-headless-shell-154.0.1.2").exists());
  }

  #[tokio::test]
  async fn webkit_revisions_live_in_their_own_directory() {
    let root = cache(&["webkit/webkit-2300", "webkit/webkit-2374", "webkit-2000"]);
    let installer = BrowserInstaller::with_cache_dir(root.path().to_path_buf());
    let removed = installer
      .remove_older_versions(CacheFamily::WebKit, "2374")
      .await
      .expect("remove");
    assert_eq!(removed.len(), 1);
    assert_eq!(left(&root.path().join("webkit")), ["webkit-2374"]);
    // A directory of the same name outside the family's root is not its.
    assert!(root.path().join("webkit-2000").exists());

    let empty = BrowserInstaller::with_cache_dir(root.path().join("no-such-cache"));
    assert_eq!(
      empty
        .remove_older_versions(CacheFamily::AxeCore, "4.12.1")
        .await
        .expect("remove"),
      Vec::new()
    );
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn a_symlink_is_neither_followed_nor_removed() {
    let root = cache(&["firefox-157.0.1"]);
    let elsewhere = tempfile::tempdir().expect("elsewhere");
    std::fs::write(elsewhere.path().join("keep.txt"), "sashoush").expect("file");
    std::os::unix::fs::symlink(elsewhere.path(), root.path().join("firefox-100.0")).expect("link");
    let installer = BrowserInstaller::with_cache_dir(root.path().to_path_buf());
    let removed = installer
      .remove_older_versions(CacheFamily::Firefox, "157.0.1")
      .await
      .expect("remove");
    assert_eq!(removed, Vec::new());
    assert!(elsewhere.path().join("keep.txt").exists());
    assert!(root.path().join("firefox-100.0").exists());
  }
}
