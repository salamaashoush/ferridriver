//! Host platform detection and browser executable layout helpers.

use std::path::{Path, PathBuf};

use ferridriver_error::{FerriError, Result};

#[cfg(target_os = "linux")]
use crate::linux;
use crate::playwright_cache;

// ---------------------------------------------------------------------------
// Platform detection
// ---------------------------------------------------------------------------

/// The platform name the `WebKit` builds are published under for this host:
/// `mac26-arm64`, `ubuntu24.04-x64`, `win64`.
/// One build on the Playwright CDN: `builds/<name>/<revision>/<archive>`,
/// unpacked into `install_dir`, with `executable` the file that proves it.
pub(crate) struct PlaywrightBuild {
  pub(crate) name: &'static str,
  pub(crate) label: &'static str,
  pub(crate) revision: String,
  pub(crate) archive: &'static str,
  pub(crate) install_dir: PathBuf,
  pub(crate) executable: PathBuf,
  pub(crate) marker: BuildMarker,
}

/// How a build's directory says the build is completely installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BuildMarker {
  /// ferridriver's own cache: `.downloaded`, holding the archive's name, so
  /// a build made for another OS release under the same revision is
  /// replaced rather than kept.
  Archive,
  /// Playwright's cache: an empty `INSTALLATION_COMPLETE`, as
  /// `playwright install` writes it.
  PlaywrightComplete,
}

impl BuildMarker {
  pub(crate) fn file_name(self) -> &'static str {
    match self {
      Self::Archive => ".downloaded",
      Self::PlaywrightComplete => playwright_cache::INSTALLATION_COMPLETE,
    }
  }

  pub(crate) async fn says_installed(self, marker_file: &Path, archive: &str) -> bool {
    match self {
      Self::Archive => tokio::fs::read_to_string(marker_file).await.unwrap_or_default() == archive,
      Self::PlaywrightComplete => tokio::fs::metadata(marker_file)
        .await
        .is_ok_and(|found| found.is_file()),
    }
  }

  pub(crate) async fn write(self, marker_file: &Path, archive: &str) -> Result<()> {
    match self {
      Self::Archive => Ok(tokio::fs::write(marker_file, archive.as_bytes()).await?),
      Self::PlaywrightComplete => Ok(tokio::fs::write(marker_file, b"").await?),
    }
  }
}

/// Playwright's ffmpeg archive for `platform`, and the executable in it
/// (`registry/index.ts`). There is none before macOS 14 or on the Linux
/// releases Playwright no longer builds for; Windows on ARM runs the x64
/// build, as it does Chromium and `WebKit`.
pub(crate) fn ffmpeg_build(platform: &str) -> Option<(&'static str, &'static str)> {
  #[cfg(target_os = "linux")]
  if let Some(archive) = linux::ffmpeg_archive(platform) {
    return Some((archive, "ffmpeg-linux"));
  }
  Some(match platform {
    "mac14" | "mac15" | "mac26" => ("ffmpeg-mac.zip", "ffmpeg-mac"),
    "mac14-arm64" | "mac15-arm64" | "mac26-arm64" => ("ffmpeg-mac-arm64.zip", "ffmpeg-mac"),
    "win64" => ("ffmpeg-win64.zip", "ffmpeg-win64.exe"),
    _ => return None,
  })
}

/// What Playwright launches inside a `WebKit` build: a shell wrapper on
/// Linux and macOS, the browser itself on Windows.
pub(crate) const WEBKIT_EXECUTABLE: &str = if cfg!(windows) { "Playwright.exe" } else { "pw_run.sh" };

pub(crate) fn webkit_platform_for_host() -> Result<String> {
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
pub(crate) fn mac_host_platform(darwin_release: &str, arch: &str) -> String {
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
pub(crate) fn webkit_archive(platform: &str) -> Option<&'static str> {
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

pub(crate) fn current_platform() -> String {
  let os = std::env::consts::OS;
  let arch = std::env::consts::ARCH;

  match (os, arch) {
    ("linux", "x86_64") => "linux64".to_string(),
    ("linux", "aarch64") => "linux-arm64".to_string(),
    ("macos", "x86_64") => "mac-x64".to_string(),
    ("macos", "aarch64") => "mac-arm64".to_string(),
    // Chrome for Testing publishes no ARM64 Windows build; Playwright runs
    // the x64 one there under Windows' emulation, and so does this.
    ("windows", "x86_64" | "aarch64") => "win64".to_string(),
    ("windows", "x86") => "win32".to_string(),
    _ => format!("{os}-{arch}"),
  }
}

pub(crate) fn chrome_executable_path(install_dir: &Path, platform: &str) -> PathBuf {
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

pub(crate) fn chromedriver_executable_path(install_dir: &Path, platform: &str) -> PathBuf {
  let name = if platform.starts_with("win") {
    "chromedriver.exe"
  } else {
    "chromedriver"
  };
  install_dir.join(format!("chromedriver-{platform}")).join(name)
}

pub(crate) fn headless_shell_executable_path(install_dir: &Path, platform: &str) -> PathBuf {
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
