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
//! use ferridriver::install::{BrowserInstaller, InstallProgress};
//!
//! let installer = BrowserInstaller::new();
//! let path = installer.install_chromium(|p| { /* handle progress */ }).await?;
//!
//! // Install system deps on Linux (requires sudo)
//! installer.install_system_deps().await?;
//! ```

use std::path::{Path, PathBuf};

use futures::StreamExt;
use reqwest::Client;
use serde::Deserialize;
use tokio::io::AsyncWriteExt;

// ---------------------------------------------------------------------------
// Chrome for Testing API types
// ---------------------------------------------------------------------------

const CFT_VERSIONS_URL: &str =
  "https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions-with-downloads.json";

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

// ---------------------------------------------------------------------------
// Installer
// ---------------------------------------------------------------------------

/// Browser installer that downloads Chrome for Testing from Google's CDN.
pub struct BrowserInstaller {
  cache_dir: PathBuf,
  client: Client,
}

impl BrowserInstaller {
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
  pub async fn install_chromium<F>(&self, progress: F) -> Result<String, String>
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
      .map_err(|e| format!("failed to fetch Chrome for Testing versions: {e}"))?
      .json()
      .await
      .map_err(|e| format!("failed to parse Chrome for Testing response: {e}"))?;

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
      .ok_or_else(|| format!("no Chrome for Testing build for platform: {platform}"))?;

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
    tokio::fs::create_dir_all(&tmp_dir)
      .await
      .map_err(|e| format!("failed to create temp dir: {e}"))?;

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
            return Err(format!(
              "download failed after {DOWNLOAD_RETRIES} attempts: {last_error}"
            ));
          }
        },
      }
    }

    // Extract
    progress(InstallProgress::Extracting);

    tokio::fs::create_dir_all(&install_dir)
      .await
      .map_err(|e| format!("failed to create install dir: {e}"))?;

    let install_dir_clone = install_dir.clone();
    let zip_path_clone = zip_path.clone();
    tokio::task::spawn_blocking(move || extract_zip(&zip_path_clone, &install_dir_clone))
      .await
      .map_err(|e| format!("extract task failed: {e}"))?
      .map_err(|e| format!("extraction failed: {e}"))?;

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
      return Err(format!(
        "extraction completed but chrome executable not found at: {path}"
      ));
    }

    // Write marker file (matches Playwright's .downloaded marker)
    let _ = tokio::fs::write(&marker_file, version.as_bytes()).await;

    progress(InstallProgress::Complete {
      version: version.clone(),
      path: path.clone(),
    });

    Ok(path)
  }

  /// Download a file with streaming progress.
  async fn download_file<F>(&self, url: &str, dest: &Path, progress: &F) -> Result<(), String>
  where
    F: Fn(InstallProgress),
  {
    let response = self
      .client
      .get(url)
      .send()
      .await
      .map_err(|e| format!("request failed: {e}"))?;

    if !response.status().is_success() {
      return Err(format!("HTTP {}: {url}", response.status()));
    }

    let total_bytes = response.content_length();
    let mut bytes_downloaded: u64 = 0;

    let mut file = tokio::fs::File::create(dest)
      .await
      .map_err(|e| format!("failed to create file: {e}"))?;

    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
      let chunk = chunk.map_err(|e| format!("download error: {e}"))?;
      file.write_all(&chunk).await.map_err(|e| format!("write error: {e}"))?;
      bytes_downloaded += chunk.len() as u64;
      progress(InstallProgress::Downloading {
        bytes_downloaded,
        total_bytes,
      });
    }
    file.flush().await.map_err(|e| format!("flush error: {e}"))?;

    Ok(())
  }

  /// Install system-level dependencies required to run Chromium on Linux.
  ///
  /// Detects the Linux distribution from `/etc/os-release` and installs
  /// the appropriate packages via `apt-get`. Requires root/sudo.
  ///
  /// This is equivalent to `npx playwright install-deps chromium`.
  /// On macOS and Windows this is a no-op (no system deps needed).
  ///
  /// # Errors
  ///
  /// Returns an error if the Linux distribution is unsupported or `apt-get`/`pacman` fails.
  #[allow(clippy::unused_async)] // async needed on linux cfg, not on macOS/Windows
  pub async fn install_system_deps<F>(&self, progress: F) -> Result<(), String>
  where
    F: Fn(InstallProgress),
  {
    #[cfg(not(target_os = "linux"))]
    {
      let _ = progress;
      Ok(())
    }

    #[cfg(target_os = "linux")]
    {
      let distro = detect_linux_distro();
      let (pkg_manager, packages) = system_packages_for_distro(&distro);

      if packages.is_empty() {
        return Err(format!(
          "unsupported Linux distribution: {distro}. Cannot determine required packages."
        ));
      }

      progress(InstallProgress::InstallingDeps { distro: distro.clone() });

      // Build the install command based on the package manager
      let commands = match pkg_manager {
        PackageManager::Apt => format!(
          "apt-get update && apt-get install -y --no-install-recommends {}",
          packages.join(" ")
        ),
        PackageManager::Pacman => format!("pacman -Sy --noconfirm --needed {}", packages.join(" ")),
      };

      // Determine if we need sudo
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
        .await
        .map_err(|e| format!("failed to run apt-get: {e}"))?;

      if !status.success() {
        return Err(format!("apt-get exited with code: {}", status.code().unwrap_or(-1)));
      }

      progress(InstallProgress::DepsInstalled);
      Ok(())
    }
  }

  /// Return the path to an installed chromium, or `None` if not installed.
  #[must_use]
  pub fn find_installed_chromium(&self) -> Option<String> {
    let entries = std::fs::read_dir(&self.cache_dir).ok()?;
    let mut candidates: Vec<_> = entries
      .filter_map(std::result::Result::ok)
      .filter(|e| e.file_name().to_string_lossy().starts_with("chromium-"))
      .collect();
    // Sort by name descending (newest version first)
    candidates.sort_by_key(|b| std::cmp::Reverse(b.file_name()));

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
}

impl Default for BrowserInstaller {
  fn default() -> Self {
    Self::new()
  }
}

// ---------------------------------------------------------------------------
// Platform detection
// ---------------------------------------------------------------------------

fn current_platform() -> String {
  let os = std::env::consts::OS;
  let arch = std::env::consts::ARCH;

  match (os, arch) {
    // CfT doesn't have arm64 linux yet, use linux64 for both
    ("linux", "x86_64" | "aarch64") => "linux64".to_string(),
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

// ---------------------------------------------------------------------------
// Linux distro detection (ported from Playwright's hostPlatform.ts)
// ---------------------------------------------------------------------------

/// Detect Linux distribution from /etc/os-release.
/// Returns a Playwright-compatible platform string like "ubuntu24.04-x64".
#[cfg(target_os = "linux")]
fn detect_linux_distro() -> String {
  let arch_suffix = match std::env::consts::ARCH {
    "x86_64" => "-x64",
    "aarch64" => "-arm64",
    _ => "-x64",
  };

  let (id, version) = read_os_release().unwrap_or_default();

  match id.as_str() {
    "ubuntu" | "pop" | "neon" | "tuxedo" => {
      let major: u32 = version.split('.').next().and_then(|s| s.parse().ok()).unwrap_or(24);
      if major < 20 {
        format!("ubuntu20.04{arch_suffix}")
      } else if major < 22 {
        format!("ubuntu20.04{arch_suffix}")
      } else if major < 24 {
        format!("ubuntu22.04{arch_suffix}")
      } else {
        format!("ubuntu24.04{arch_suffix}")
      }
    },
    "linuxmint" => {
      let major: u32 = version.split('.').next().and_then(|s| s.parse().ok()).unwrap_or(22);
      if major <= 20 {
        format!("ubuntu20.04{arch_suffix}")
      } else if major == 21 {
        format!("ubuntu22.04{arch_suffix}")
      } else {
        format!("ubuntu24.04{arch_suffix}")
      }
    },
    "debian" | "raspbian" => match version.as_str() {
      "11" => format!("debian11{arch_suffix}"),
      "12" => format!("debian12{arch_suffix}"),
      _ => format!("debian13{arch_suffix}"),
    },
    // Arch Linux and derivatives
    "arch" | "manjaro" | "endeavouros" | "garuda" | "artix" | "cachyos" => {
      format!("arch{arch_suffix}")
    },
    // Default to ubuntu24.04 for unknown distros (same as Playwright)
    _ => format!("ubuntu24.04{arch_suffix}"),
  }
}

/// Read /etc/os-release and return (ID, VERSION_ID).
#[cfg(target_os = "linux")]
fn read_os_release() -> Option<(String, String)> {
  let content = std::fs::read_to_string("/etc/os-release").ok()?;
  let mut id = String::new();
  let mut version = String::new();
  for line in content.lines() {
    if let Some(val) = line.strip_prefix("ID=") {
      id = val.trim_matches('"').to_lowercase();
    } else if let Some(val) = line.strip_prefix("VERSION_ID=") {
      version = val.trim_matches('"').to_string();
    }
  }
  Some((id, version))
}

// ---------------------------------------------------------------------------
// System dependency lists (ported from Playwright's nativeDeps.ts + Arch)
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
enum PackageManager {
  Apt,
  Pacman,
}

/// Return the package manager and deduplicated list of packages needed
/// to run Chromium on the given distro.
///
/// These lists are Docker-verified against Chrome for Testing on bare Ubuntu 24.04.
/// Only packages whose shared libraries Chrome actually links against are included,
/// plus fonts for text rendering and headed mode X11 libs for developers.
#[cfg(target_os = "linux")]
fn system_packages_for_distro(distro: &str) -> (PackageManager, Vec<&'static str>) {
  if distro.starts_with("arch") {
    return (PackageManager::Pacman, arch_chromium_packages());
  }

  // Chrome for Testing runtime deps (verified via ldd on bare Ubuntu 24.04 Docker).
  // These are the actual shared libraries Chrome links against at the binary level.
  // X11 composite/damage/fixes/randr ARE required even for headless (linked, not dlopen'd).
  let chromium: &[&str] = match distro {
    d if d.starts_with("ubuntu20.04") => &[
      // Core runtime
      "libasound2",
      "libatk-bridge2.0-0",
      "libatk1.0-0",
      "libatspi2.0-0",
      "libcairo2",
      "libcups2",
      "libdbus-1-3",
      "libdrm2",
      "libgbm1",
      "libglib2.0-0",
      "libnspr4",
      "libnss3",
      "libpango-1.0-0",
      // X11 (required by the binary even in headless)
      "libxcb1",
      "libxcomposite1",
      "libxdamage1",
      "libxfixes3",
      "libxrandr2",
      "libxkbcommon0",
      // Fonts (minimum for text rendering)
      "fonts-liberation",
      // Headed mode (X11 display)
      "libx11-6",
      "libxext6",
      "libwayland-client0",
      // Emoji
      "fonts-noto-color-emoji",
    ],
    d if d.starts_with("ubuntu22.04") | d.starts_with("debian11") | d.starts_with("debian12") => &[
      "libasound2",
      "libatk-bridge2.0-0",
      "libatk1.0-0",
      "libatspi2.0-0",
      "libcairo2",
      "libcups2",
      "libdbus-1-3",
      "libdrm2",
      "libgbm1",
      "libglib2.0-0",
      "libnspr4",
      "libnss3",
      "libpango-1.0-0",
      "libxcb1",
      "libxcomposite1",
      "libxdamage1",
      "libxfixes3",
      "libxrandr2",
      "libxkbcommon0",
      "fonts-liberation",
      "libx11-6",
      "libxext6",
      "libwayland-client0",
      "fonts-noto-color-emoji",
    ],
    // ubuntu24.04, debian13, and fallback (t64 ABI transition packages)
    _ => &[
      "libasound2t64",
      "libatk-bridge2.0-0t64",
      "libatk1.0-0t64",
      "libatspi2.0-0t64",
      "libcairo2",
      "libcups2t64",
      "libdbus-1-3",
      "libdrm2",
      "libgbm1",
      "libglib2.0-0t64",
      "libnspr4",
      "libnss3",
      "libpango-1.0-0",
      "libxcb1",
      "libxcomposite1",
      "libxdamage1",
      "libxfixes3",
      "libxrandr2",
      "libxkbcommon0",
      "fonts-liberation",
      "libx11-6",
      "libxext6",
      "libwayland-client0",
      "fonts-noto-color-emoji",
    ],
  };

  (PackageManager::Apt, chromium.to_vec())
}

/// Arch Linux (and derivatives) packages for Chromium.
/// Uses pacman package names. Verified against the same shared library
/// requirements as the apt packages above.
#[cfg(target_os = "linux")]
fn arch_chromium_packages() -> Vec<&'static str> {
  vec![
    // Core runtime (matches ldd requirements)
    "alsa-lib",     // libasound.so.2
    "at-spi2-core", // libatk-bridge, libatk, libatspi
    "cairo",        // libcairo.so.2
    "libcups",      // libcups.so.2
    "dbus",         // libdbus-1.so.3
    "libdrm",       // libdrm.so.2
    "mesa",         // libgbm.so.1
    "glib2",        // libglib, libgio, libgobject
    "nspr",         // libnspr4.so
    "nss",          // libnss3.so
    "pango",        // libpango-1.0.so.0
    // X11 (required by Chrome binary even in headless)
    "libxcb",        // libxcb.so.1
    "libxcomposite", // libXcomposite.so.1
    "libxdamage",    // libXdamage.so.1
    "libxfixes",     // libXfixes.so.3
    "libxrandr",     // libXrandr.so.2
    "libxkbcommon",  // libxkbcommon.so.0
    // Headed mode
    "libx11",  // libX11.so.6
    "libxext", // libXext.so.6
    "wayland", // libwayland-client.so.0
    // Fonts
    "ttf-liberation",   // basic web fonts
    "noto-fonts-emoji", // emoji rendering
    "fontconfig",       // font discovery
    "freetype2",        // font rendering
  ]
}

// ---------------------------------------------------------------------------
// Zip extraction
// ---------------------------------------------------------------------------

fn extract_zip(zip_path: &Path, dest: &Path) -> Result<(), String> {
  let file = std::fs::File::open(zip_path).map_err(|e| format!("failed to open zip: {e}"))?;
  let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("failed to read zip archive: {e}"))?;

  for i in 0..archive.len() {
    let mut entry = archive
      .by_index(i)
      .map_err(|e| format!("failed to read zip entry {i}: {e}"))?;

    let name = entry.name().to_string();
    let out_path = dest.join(&name);

    if entry.is_dir() {
      std::fs::create_dir_all(&out_path).map_err(|e| format!("failed to create dir {}: {e}", out_path.display()))?;
    } else {
      if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("failed to create parent dir: {e}"))?;
      }
      let mut out_file =
        std::fs::File::create(&out_path).map_err(|e| format!("failed to create file {}: {e}", out_path.display()))?;
      std::io::copy(&mut entry, &mut out_file).map_err(|e| format!("failed to write {}: {e}", out_path.display()))?;

      // Preserve executable permissions on Unix
      #[cfg(unix)]
      {
        use std::os::unix::fs::PermissionsExt;
        if let Some(mode) = entry.unix_mode() {
          let _ = std::fs::set_permissions(&out_path, std::fs::Permissions::from_mode(mode));
        }
      }
    }
  }

  Ok(())
}
