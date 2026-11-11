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
mod playwright_cache;

pub use playwright_cache::{CachedBrowser, find_playwright_browser, playwright_cache_dir};

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

/// The latest released `playwright-core`. Its `browsers.json` names the
/// `WebKit` and Android driver builds that release ships, the way Chrome
/// for Testing and Mozilla name their current stable release. Main's names
/// builds no release has shipped yet.
const PW_LATEST_RELEASE_URL: &str = "https://registry.npmjs.org/playwright-core/latest";

#[derive(Debug, Deserialize)]
struct NpmRelease {
  version: String,
}

/// `browsers.json` as tagged for `release`.
fn released_browsers_json_url(release: &NpmRelease) -> Result<String> {
  let version = release.version.trim();
  let plain = !version.is_empty()
    && version.split('.').count() == 3
    && version
      .split('.')
      .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()));
  if !plain {
    return Err(backend_err(format!(
      "the latest playwright-core is '{version}', which is not a release version"
    )));
  }
  Ok(format!(
    "https://raw.githubusercontent.com/microsoft/playwright/v{version}/packages/playwright-core/browsers.json"
  ))
}

/// The build revision `browsers` names for `name`, taking the override
/// for `platform` when there is one.
fn browser_revision(browsers: PwBrowsersJson, name: &str, platform: &str) -> Result<String> {
  browsers
    .browsers
    .into_iter()
    .find(|b| b.name == name)
    .map(|mut b| b.revision_overrides.remove(platform).unwrap_or(b.revision))
    .ok_or_else(|| backend_err(format!("Playwright browsers.json missing {name} entry")))
}

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
#[serde(rename_all = "camelCase")]
struct PwBrowserEntry {
  name: String,
  revision: String,
  /// Host platforms pinned to an older build, such as `mac14-arm64`.
  #[serde(default)]
  revision_overrides: std::collections::BTreeMap<String, String>,
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
  ChromeDriver,
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
      "chromedriver" => Self::ChromeDriver,
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
      Self::ChromeDriver => "chromedriver-",
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
    Self {
      cache_dir: default_cache_dir(),
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

  async fn released_browsers_json(&self) -> Result<PwBrowsersJson> {
    let release: NpmRelease = self
      .client
      .get(PW_LATEST_RELEASE_URL)
      .send()
      .await
      .map_err(|e| backend_err(format!("failed to resolve the latest Playwright release: {e}")))?
      .json()
      .await
      .map_err(|e| backend_err(format!("failed to parse the latest Playwright release: {e}")))?;
    self
      .client
      .get(released_browsers_json_url(&release)?)
      .send()
      .await
      .map_err(|e| {
        backend_err(format!(
          "failed to fetch Playwright {} browsers.json: {e}",
          release.version
        ))
      })?
      .error_for_status()
      .map_err(|e| {
        backend_err(format!(
          "failed to fetch Playwright {} browsers.json: {e}",
          release.version
        ))
      })?
      .json()
      .await
      .map_err(|e| {
        backend_err(format!(
          "failed to parse Playwright {} browsers.json: {e}",
          release.version
        ))
      })
  }

  /// Install the Playwright `WebKit` binary.
  ///
  /// Reads the `WebKit` revision from the `browsers.json` of the latest
  /// released Playwright, downloads the matching archive from the
  /// Playwright CDN, and extracts it to
  /// `~/.cache/ferridriver/webkit/webkit-<rev>/`.
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

    let browsers = self.released_browsers_json().await?;

    let platform = webkit_platform_for_host()?;
    let archive = webkit_archive(&platform)
      .ok_or_else(|| FerriError::unsupported(format!("no Playwright WebKit build for {platform}")))?;
    let revision = browser_revision(browsers, "webkit", &platform)?;

    let webkit_root = self.cache_dir.join("webkit");
    let install_dir = webkit_root.join(format!("webkit-{revision}"));
    let marker_file = install_dir.join(".downloaded");
    let executable = install_dir.join("pw_run.sh");

    // The marker names the archive, so a build made for another OS release
    // under the same revision is replaced rather than kept.
    let installed = tokio::fs::read_to_string(&marker_file).await.unwrap_or_default();
    if installed == archive && executable.exists() {
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

    let _ = tokio::fs::write(&marker_file, archive.as_bytes()).await;

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
    let browsers = self.released_browsers_json().await?;
    let revision = browser_revision(browsers, "android", "")?;

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

  /// Return the path to an installed `WebKit`, or `None` if not installed.
  #[must_use]
  pub fn find_installed_webkit(&self) -> Option<String> {
    let root = CacheFamily::WebKit.root(&self.cache_dir);
    let entries = std::fs::read_dir(root).ok()?;
    let mut candidates: Vec<_> = entries
      .filter_map(std::result::Result::ok)
      .filter(|e| e.file_name().to_string_lossy().starts_with("webkit-"))
      .collect();
    candidates.sort_by_key(|entry| std::cmp::Reverse(version_order(&entry.file_name().to_string_lossy())));

    candidates.into_iter().find_map(|entry| {
      let exe = entry.path().join("pw_run.sh");
      (entry.path().join(".downloaded").exists() && exe.is_file()).then(|| exe.to_string_lossy().to_string())
    })
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

// ---------------------------------------------------------------------------
// Platform detection
// ---------------------------------------------------------------------------

/// The platform name the `WebKit` builds are published under for this host:
/// `mac26-arm64`, `ubuntu24.04-x64`, `win64`.
fn webkit_platform_for_host() -> Result<String> {
  let os = std::env::consts::OS;
  let arch = std::env::consts::ARCH;
  match os {
    #[cfg(target_os = "linux")]
    "linux" => {
      let os_release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
      Ok(linux::host_platform(&os_release, arch))
    },
    #[cfg(unix)]
    "macos" => {
      let uname = rustix::system::uname();
      Ok(mac_host_platform(&uname.release().to_string_lossy(), arch))
    },
    "windows" => Ok("win64".to_string()),
    _ => Err(FerriError::unsupported(format!(
      "no Playwright WebKit build for {os}/{arch}"
    ))),
  }
}

/// The newest macOS major a `WebKit` build is published for. A newer
/// Darwin, a beta say, takes this one's build.
#[cfg(any(unix, test))]
const LAST_STABLE_MACOS_MAJOR: u32 = 26;

/// The macOS platform name from the Darwin kernel release
/// (`uname -r`). Darwin 20 to 24 are macOS 11 to 15; Apple then jumped to
/// macOS 26 for Darwin 25.
#[cfg(any(unix, test))]
fn mac_host_platform(darwin_release: &str, arch: &str) -> String {
  let darwin = darwin_release
    .split('.')
    .next()
    .and_then(|major| major.parse::<u32>().ok())
    .unwrap_or(0);
  let version = match darwin {
    ..18 => "10.13".to_string(),
    18 => "10.14".to_string(),
    19 => "10.15".to_string(),
    20..25 => (darwin - 9).to_string(),
    _ => (darwin + 1).min(LAST_STABLE_MACOS_MAJOR).to_string(),
  };
  let suffix = if darwin >= 20 && arch == "aarch64" {
    "-arm64"
  } else {
    ""
  };
  format!("mac{version}{suffix}")
}

/// The `WebKit` archive built for `platform`. macOS before 14 has no
/// current build.
fn webkit_archive(platform: &str) -> Option<&'static str> {
  #[cfg(target_os = "linux")]
  if let Some(archive) = linux::webkit_archive(platform) {
    return Some(archive);
  }
  Some(match platform {
    "mac14" => "webkit-mac-14.zip",
    "mac14-arm64" => "webkit-mac-14-arm64.zip",
    "mac15" => "webkit-mac-15.zip",
    "mac15-arm64" => "webkit-mac-15-arm64.zip",
    "mac26" => "webkit-mac-26.zip",
    "mac26-arm64" => "webkit-mac-26-arm64.zip",
    "win64" => "webkit-win64.zip",
    _ => return None,
  })
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

fn chromedriver_executable_path(install_dir: &Path, platform: &str) -> PathBuf {
  let name = if platform.starts_with("win") {
    "chromedriver.exe"
  } else {
    "chromedriver"
  };
  install_dir.join(format!("chromedriver-{platform}")).join(name)
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

/// Whether a symlink at `link` (relative to the archive root) pointing at
/// `target` resolves inside the archive.
#[cfg(any(unix, test))]
fn link_stays_inside(link: &Path, target: &Path) -> bool {
  use std::path::Component;
  if target.is_absolute() {
    return false;
  }
  let mut depth: usize = link.parent().map_or(0, |parent| parent.components().count());
  for component in target.components() {
    match component {
      Component::Normal(_) => depth += 1,
      Component::CurDir => {},
      Component::ParentDir => match depth.checked_sub(1) {
        Some(up) => depth = up,
        None => return false,
      },
      Component::RootDir | Component::Prefix(_) => return false,
    }
  }
  true
}

fn extract_zip(zip_path: &Path, dest: &Path) -> Result<()> {
  let file = std::fs::File::open(zip_path)?;
  let mut archive = zip::ZipArchive::new(file).map_err(|e| backend_err(format!("failed to read zip archive: {e}")))?;

  for i in 0..archive.len() {
    let mut entry = archive
      .by_index(i)
      .map_err(|e| backend_err(format!("failed to read zip entry {i}: {e}")))?;

    // An entry named `/etc/x` or `../x` would land outside `dest`.
    let relative = entry
      .enclosed_name()
      .ok_or_else(|| backend_err(format!("zip entry {i} has a path outside the archive")))?;
    let out_path = dest.join(&relative);

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
          if !link_stays_inside(&relative, Path::new(&target)) {
            return Err(backend_err(format!(
              "zip entry {} links outside the archive: {target}",
              relative.display()
            )));
          }
          // Replace any pre-existing entry — zip ordering is not stable.
          let _ = std::fs::remove_file(&out_path);
          std::os::unix::fs::symlink(&target, &out_path)?;
          continue;
        }
        let mut out_file = std::fs::File::create(&out_path)?;
        std::io::copy(&mut entry, &mut out_file)?;
        // Permission bits only: setuid, setgid and sticky from an archive
        // are never wanted on a browser build.
        let _ = std::fs::set_permissions(&out_path, std::fs::Permissions::from_mode(mode & 0o777));
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

#[cfg(test)]
mod extraction_tests {
  use std::path::Path;

  use super::{extract_zip, link_stays_inside};

  fn write_zip(path: &Path, build: impl FnOnce(&mut zip::ZipWriter<std::fs::File>)) {
    let mut writer = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    build(&mut writer);
    writer.finish().unwrap();
  }

  fn stored() -> zip::write::SimpleFileOptions {
    zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)
  }

  #[test]
  fn symlinks_must_resolve_inside_the_archive() {
    assert!(link_stays_inside(Path::new("lib/libfoo.so"), Path::new("libfoo.so.4")));
    assert!(link_stays_inside(
      Path::new("lib/x/libfoo.so"),
      Path::new("../libfoo.so.4")
    ));
    assert!(!link_stays_inside(
      Path::new("lib/libfoo.so"),
      Path::new("../../etc/passwd")
    ));
    assert!(!link_stays_inside(Path::new("libfoo.so"), Path::new("/etc/passwd")));
  }

  #[test]
  fn extraction_refuses_entries_outside_the_destination() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("evil.zip");
    write_zip(&archive, |zip| {
      zip.start_file("../escaped", stored()).unwrap();
      std::io::Write::write_all(zip, b"x").unwrap();
    });
    let dest = root.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();
    assert!(extract_zip(&archive, &dest).is_err());
    assert!(!root.path().join("escaped").exists());
  }

  #[cfg(unix)]
  #[test]
  fn extraction_keeps_inner_links_and_drops_privileged_bits() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("build.zip");
    write_zip(&archive, |zip| {
      zip
        .start_file("lib/libfoo.so.4", stored().unix_permissions(0o4755))
        .unwrap();
      std::io::Write::write_all(zip, b"elf").unwrap();
      zip.add_symlink("lib/libfoo.so", "libfoo.so.4", stored()).unwrap();
    });
    let dest = root.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();
    extract_zip(&archive, &dest).unwrap();
    let mode = std::fs::metadata(dest.join("lib/libfoo.so.4"))
      .unwrap()
      .permissions()
      .mode();
    assert_eq!(mode & 0o7777, 0o755, "setuid survived: {mode:o}");
    assert_eq!(std::fs::read(dest.join("lib/libfoo.so")).unwrap(), b"elf");

    let escaping = root.path().join("escaping.zip");
    write_zip(&escaping, |zip| {
      zip.add_symlink("lib/libfoo.so", "../../outside", stored()).unwrap();
    });
    let dest = root.path().join("out2");
    std::fs::create_dir_all(&dest).unwrap();
    assert!(extract_zip(&escaping, &dest).is_err());
  }
}

#[cfg(test)]
mod webkit_platform_tests {
  use super::{
    NpmRelease, PwBrowsersJson, browser_revision, mac_host_platform, released_browsers_json_url, webkit_archive,
  };

  #[test]
  fn darwin_releases_map_to_mac_platforms() {
    for (release, arch, platform) in [
      ("17.7.0", "x86_64", "mac10.13"),
      ("18.0.0", "x86_64", "mac10.14"),
      ("19.6.0", "x86_64", "mac10.15"),
      ("20.6.0", "aarch64", "mac11-arm64"),
      ("23.6.0", "aarch64", "mac14-arm64"),
      ("24.1.0", "x86_64", "mac15"),
      ("25.0.0", "aarch64", "mac26-arm64"),
      ("27.2.0", "aarch64", "mac26-arm64"),
    ] {
      assert_eq!(mac_host_platform(release, arch), platform, "{release} {arch}");
    }
  }

  #[test]
  fn each_mac_release_takes_the_build_made_for_it() {
    assert_eq!(webkit_archive("mac14-arm64"), Some("webkit-mac-14-arm64.zip"));
    assert_eq!(webkit_archive("mac15"), Some("webkit-mac-15.zip"));
    assert_eq!(webkit_archive("mac26-arm64"), Some("webkit-mac-26-arm64.zip"));
    assert_eq!(webkit_archive("mac13-arm64"), None);
  }

  #[test]
  fn browser_builds_come_from_the_latest_release_tag_not_main() {
    let release: NpmRelease = serde_json::from_str(r#"{"name":"playwright-core","version":"1.65.0"}"#).unwrap();
    assert_eq!(
      released_browsers_json_url(&release).unwrap(),
      "https://raw.githubusercontent.com/microsoft/playwright/v1.65.0/packages/playwright-core/browsers.json"
    );
    for unreleased in ["1.66.0-alpha-2026-11-06", "1.65.0-beta-1793921730000", "main", ""] {
      let release = NpmRelease {
        version: unreleased.to_string(),
      };
      assert!(released_browsers_json_url(&release).is_err(), "{unreleased}");
    }
  }

  #[test]
  fn a_revision_is_the_platform_override_or_the_default() {
    let json = r#"{"browsers":[{"name":"webkit","revision":"2388","revisionOverrides":{"mac14-arm64":"2251"}},{"name":"android","revision":"1001"}]}"#;
    let parse = || serde_json::from_str::<PwBrowsersJson>(json).unwrap();
    assert_eq!(browser_revision(parse(), "webkit", "mac14-arm64").unwrap(), "2251");
    assert_eq!(browser_revision(parse(), "webkit", "mac26-arm64").unwrap(), "2388");
    assert_eq!(browser_revision(parse(), "android", "").unwrap(), "1001");
    assert!(browser_revision(parse(), "chromium", "mac26-arm64").is_err());
  }

  #[test]
  fn revision_overrides_are_read_per_platform() {
    let json = r#"{"browsers":[{"name":"webkit","revision":"2387","revisionOverrides":{"mac14-arm64":"2251"}}]}"#;
    let mut browsers: PwBrowsersJson = serde_json::from_str(json).unwrap();
    let mut webkit = browsers.browsers.remove(0);
    assert_eq!(webkit.revision_overrides.remove("mac14-arm64").as_deref(), Some("2251"));
    assert_eq!(webkit.revision, "2387");
  }
}
