//! Built-in step definitions for browser automation.
//!
//! All steps use cucumber expressions and operate on `BrowserWorld`.
//! They are registered via `#[given]`, `#[when]`, `#[then]` proc macros.

pub mod api;
pub mod assertion;
pub mod cookie;
pub mod dialog;
pub mod emulation;
pub mod file;
pub mod frame;
pub mod interaction;
pub mod javascript;
pub mod keyboard;
pub mod mouse;
pub mod navigation;
pub mod network;
pub mod screenshot;
pub mod storage;
pub mod variable;
pub mod wait;
pub mod window;

/// Resolve relative URLs against `FERRIDRIVER_BASE_URL` (set by the test
/// runner when a `webServer` fixture is configured). Absolute `http(s)`
/// and `data:` URLs pass through untouched. Shared by navigation and API
/// request steps so both address the same fixture server.
pub(crate) fn resolve_url(url: &str) -> String {
  if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("data:") {
    return url.to_string();
  }
  if let Ok(base) = std::env::var("FERRIDRIVER_BASE_URL") {
    let base = base.trim_end_matches('/');
    let path = url.strip_prefix('/').unwrap_or(url);
    return format!("{base}/{path}");
  }
  url.to_string()
}
