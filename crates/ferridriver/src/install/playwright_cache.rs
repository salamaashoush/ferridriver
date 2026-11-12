//! Browsers another installer already put on the machine: Playwright's
//! browser cache, read by the same rules its installer writes it.
//!
//! ferridriver's own cache always wins over this one. Playwright replaces
//! its builds whenever a project on the machine updates it, so a launch that
//! preferred them would change browser with whatever was installed last.

use std::path::{Path, PathBuf};

/// A browser family as Playwright's cache names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachedBrowser {
  Chromium,
  ChromiumHeadlessShell,
  Firefox,
  WebKit,
  /// Playwright's own ffmpeg, which records video.
  Ffmpeg,
}

impl CachedBrowser {
  fn directory_prefix(self) -> &'static str {
    match self {
      Self::Chromium => "chromium-",
      Self::ChromiumHeadlessShell => "chromium_headless_shell-",
      Self::Firefox => "firefox-",
      Self::WebKit => "webkit-",
      Self::Ffmpeg => "ffmpeg-",
    }
  }

  /// Where the executable sits inside one build directory, for this host.
  /// Chromium also lists the layout older Chromium builds shipped in, which
  /// caches written before Chrome for Testing still hold.
  fn executables(self) -> &'static [&'static str] {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    let table: &[&str] = match self {
      Self::Chromium => &[
        "chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
        "chrome-mac/Chromium.app/Contents/MacOS/Chromium",
      ],
      Self::ChromiumHeadlessShell => &["chrome-headless-shell-mac-arm64/chrome-headless-shell"],
      Self::Firefox => &["firefox/Nightly.app/Contents/MacOS/firefox"],
      Self::WebKit => &["pw_run.sh"],
      Self::Ffmpeg => &["ffmpeg-mac"],
    };
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    let table: &[&str] = match self {
      Self::Chromium => &[
        "chrome-mac-x64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
        "chrome-mac/Chromium.app/Contents/MacOS/Chromium",
      ],
      Self::ChromiumHeadlessShell => &["chrome-headless-shell-mac-x64/chrome-headless-shell"],
      Self::Firefox => &["firefox/Nightly.app/Contents/MacOS/firefox"],
      Self::WebKit => &["pw_run.sh"],
      Self::Ffmpeg => &["ffmpeg-mac"],
    };
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    let table: &[&str] = match self {
      Self::Chromium => &["chrome-linux64/chrome", "chrome-linux/chrome"],
      Self::ChromiumHeadlessShell => &[
        "chrome-headless-shell-linux64/chrome-headless-shell",
        "chrome-linux/headless_shell",
      ],
      Self::Firefox => &["firefox/firefox"],
      Self::WebKit => &["pw_run.sh"],
      Self::Ffmpeg => &["ffmpeg-linux"],
    };
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    let table: &[&str] = match self {
      Self::Chromium => &["chrome-linux-arm64/chrome", "chrome-linux/chrome"],
      Self::ChromiumHeadlessShell => &[
        "chrome-headless-shell-linux-arm64/chrome-headless-shell",
        "chrome-linux/headless_shell",
      ],
      Self::Firefox => &["firefox/firefox"],
      Self::WebKit => &["pw_run.sh"],
      Self::Ffmpeg => &["ffmpeg-linux"],
    };
    #[cfg(target_os = "windows")]
    let table: &[&str] = match self {
      Self::Chromium => &["chrome-win64/chrome.exe", "chrome-win/chrome.exe"],
      Self::ChromiumHeadlessShell => &["chrome-headless-shell-win64/chrome-headless-shell.exe"],
      Self::Firefox => &["firefox/firefox.exe"],
      Self::WebKit => &["Playwright.exe"],
      Self::Ffmpeg => &["ffmpeg-win64.exe"],
    };
    #[cfg(not(any(
      all(target_os = "macos", any(target_arch = "aarch64", target_arch = "x86_64")),
      all(target_os = "linux", any(target_arch = "aarch64", target_arch = "x86_64")),
      target_os = "windows"
    )))]
    let table: &[&str] = {
      let _ = self;
      &[]
    };
    table
  }
}

/// Playwright's browser cache: `PLAYWRIGHT_BROWSERS_PATH` when it names a
/// directory, otherwise `ms-playwright` under the platform cache directory.
/// `PLAYWRIGHT_BROWSERS_PATH=0` keeps browsers inside an npm package, which
/// has no location outside that package, so it yields `None`.
#[must_use]
pub fn playwright_cache_dir() -> Option<PathBuf> {
  match std::env::var_os("PLAYWRIGHT_BROWSERS_PATH") {
    Some(value) if value == "0" => None,
    Some(value) if !value.is_empty() => {
      let path = PathBuf::from(value);
      if path.is_absolute() {
        Some(path)
      } else {
        std::env::current_dir().ok().map(|cwd| cwd.join(path))
      }
    },
    _ => dirs::cache_dir().map(|cache| cache.join("ms-playwright")),
  }
}

/// The newest completely installed build of `browser` in Playwright's cache.
#[must_use]
pub fn find_playwright_browser(browser: CachedBrowser) -> Option<PathBuf> {
  newest_build_in(&playwright_cache_dir()?, browser)
}

/// The newest build of `browser` under `cache`. A build counts once its
/// installer has written `INSTALLATION_COMPLETE`; a directory without it is
/// a download that was interrupted or is still running.
pub(crate) fn newest_build_in(cache: &Path, browser: CachedBrowser) -> Option<PathBuf> {
  let prefix = browser.directory_prefix();
  let mut builds: Vec<(u64, PathBuf)> = std::fs::read_dir(cache)
    .ok()?
    .filter_map(std::result::Result::ok)
    .filter_map(|entry| {
      let name = entry.file_name();
      let revision = name.to_str()?.strip_prefix(prefix)?.parse::<u64>().ok()?;
      Some((revision, entry.path()))
    })
    .filter(|(_, dir)| dir.join("INSTALLATION_COMPLETE").is_file())
    .collect();
  builds.sort_by_key(|(revision, _)| std::cmp::Reverse(*revision));
  builds.into_iter().find_map(|(_, dir)| {
    browser
      .executables()
      .iter()
      .map(|relative| dir.join(relative))
      .find(|exe| exe.is_file())
  })
}

#[cfg(test)]
mod tests {
  use super::{CachedBrowser, newest_build_in};
  use std::path::Path;

  fn build(cache: &Path, dir: &str, relative: &str, complete: bool) {
    let exe = cache.join(dir).join(relative);
    std::fs::create_dir_all(exe.parent().expect("parent")).expect("mkdir");
    std::fs::write(&exe, b"x").expect("write");
    if complete {
      std::fs::write(cache.join(dir).join("INSTALLATION_COMPLETE"), b"").expect("marker");
    }
  }

  fn chromium() -> &'static str {
    CachedBrowser::Chromium.executables()[0]
  }

  #[test]
  fn the_highest_revision_wins_by_number_not_by_name() {
    let cache = tempfile::tempdir().expect("tempdir");
    build(cache.path(), "chromium-999", chromium(), true);
    build(cache.path(), "chromium-1248", chromium(), true);

    let found = newest_build_in(cache.path(), CachedBrowser::Chromium).expect("a build");
    assert!(
      found.starts_with(cache.path().join("chromium-1248")),
      "got {}",
      found.display()
    );
  }

  #[test]
  fn an_unfinished_download_is_not_a_build() {
    let cache = tempfile::tempdir().expect("tempdir");
    build(cache.path(), "chromium-1200", chromium(), true);
    build(cache.path(), "chromium-1248", chromium(), false);

    let found = newest_build_in(cache.path(), CachedBrowser::Chromium).expect("a build");
    assert!(
      found.starts_with(cache.path().join("chromium-1200")),
      "got {}",
      found.display()
    );
  }

  #[test]
  fn the_headless_shell_is_not_a_headed_chromium() {
    let cache = tempfile::tempdir().expect("tempdir");
    let shell = CachedBrowser::ChromiumHeadlessShell.executables()[0];
    build(cache.path(), "chromium_headless_shell-1248", shell, true);
    build(cache.path(), "chromium-headless-shell-1300", shell, true);

    assert!(newest_build_in(cache.path(), CachedBrowser::Chromium).is_none());
    let found = newest_build_in(cache.path(), CachedBrowser::ChromiumHeadlessShell).expect("the shell");
    assert!(found.ends_with(shell), "got {}", found.display());
  }

  #[test]
  fn firefox_is_found_where_its_installer_unpacks_it() {
    let cache = tempfile::tempdir().expect("tempdir");
    let firefox = CachedBrowser::Firefox.executables()[0];
    build(cache.path(), "firefox-1555", firefox, true);

    let found = newest_build_in(cache.path(), CachedBrowser::Firefox).expect("firefox");
    assert!(found.ends_with(firefox), "got {}", found.display());
  }
}
