//! NAPI bindings for browser installation.

use napi::bindgen_prelude::*;
use napi_derive::napi;

use ferridriver::install::BrowserInstaller;

/// Print the per-method CDP RTT stats table to stderr.
///
/// Only emits when `FERRIDRIVER_RTT_STATS=1` was set at process start
/// (the global aggregator only ticks under that env). Bun and Node
/// process exits do not reliably trigger libc `atexit`, so the CLI
/// bridge calls this just before `process.exit` to make the stats
/// dump appear deterministically.
#[napi]
pub fn dump_rtt_stats() {
  ferridriver::backend::cdp::transport::dump_global_rtt_stats();
}

/// Install the latest stable Chromium browser.
/// Returns the path to the installed chrome executable.
#[napi]
pub async fn install_chromium() -> Result<String> {
  let installer = BrowserInstaller::new();
  installer.install_chromium(|_| {}).await.map_err(Error::from_reason)
}

/// Install system dependencies required for Chromium (Linux only).
/// This is equivalent to `ferridriver install --with-deps`.
/// Requires root/sudo on Linux. No-op on macOS/Windows.
#[napi]
pub async fn install_system_deps() -> Result<()> {
  let installer = BrowserInstaller::new();
  installer.install_system_deps(|_| {}).await.map_err(Error::from_reason)
}

/// Install Chromium with system dependencies (convenience: install + install-deps).
/// Returns the path to the installed chrome executable.
#[napi]
pub async fn install_chromium_with_deps() -> Result<String> {
  let installer = BrowserInstaller::new();
  installer
    .install_system_deps(|_| {})
    .await
    .map_err(Error::from_reason)?;
  installer.install_chromium(|_| {}).await.map_err(Error::from_reason)
}

/// Install the latest stable Chrome Headless Shell.
/// Returns the path to the installed chrome-headless-shell executable.
/// This is a lighter, purpose-built binary optimized for headless automation.
#[napi]
pub async fn install_chromium_headless_shell() -> Result<String> {
  let installer = BrowserInstaller::new();
  installer
    .install_chromium_headless_shell(|_| {})
    .await
    .map_err(Error::from_reason)
}

/// Find an installed Chromium in the ferridriver cache.
/// Returns the path to the executable or null if not found.
#[napi]
pub fn find_installed_chromium() -> Option<String> {
  BrowserInstaller::new().find_installed_chromium()
}

/// Find an installed Chrome Headless Shell in the ferridriver cache.
/// Returns the path to the executable or null if not found.
#[napi]
pub fn find_installed_headless_shell() -> Option<String> {
  BrowserInstaller::new().find_installed_headless_shell()
}

/// Install the latest stable Firefox.
/// Returns the path to the installed firefox executable.
#[napi]
pub async fn install_firefox() -> Result<String> {
  let installer = BrowserInstaller::new();
  installer.install_firefox(|_| {}).await.map_err(Error::from_reason)
}

/// Find an installed Firefox in the ferridriver cache.
/// Returns the path to the executable or null if not found.
#[napi]
pub fn find_installed_firefox() -> Option<String> {
  BrowserInstaller::new().find_installed_firefox()
}

/// Get the browser cache directory path.
#[napi]
pub fn get_browser_cache_dir() -> String {
  BrowserInstaller::new().cache_dir().to_string_lossy().to_string()
}
