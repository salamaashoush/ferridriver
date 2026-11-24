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

/// The locator a step built from a SELECTOR resolves through.
///
/// Steps name a selector, not a Locator, so they take their strictness
/// from the context's `strictSelectors` the same way `page.click(sel)`
/// does — false unless the context asked for it. Building a plain
/// `page.locator(sel)` would make every step strict instead, and a
/// feature saying `"p" should contain text ...` on a page with two
/// paragraphs would fail where Playwright passes.
pub(crate) fn selector_locator(
  page: &std::sync::Arc<ferridriver::Page>,
  selector: &str,
) -> ferridriver::locator::Locator {
  page.locator(selector).strict(page.strict_selectors())
}

/// Resolve relative URLs against the run's base URL (published by the
/// test runner when a `webServer` fixture is configured). Absolute `http(s)`
/// and `data:` URLs pass through untouched. Shared by navigation and API
/// request steps so both address the same fixture server.
pub(crate) fn resolve_url(url: &str) -> String {
  if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("data:") {
    return url.to_string();
  }
  if let Some(base) = ferridriver_test::config::run_base_url() {
    let base = base.trim_end_matches('/');
    let path = url.strip_prefix('/').unwrap_or(url);
    return format!("{base}/{path}");
  }
  url.to_string()
}

/// An `{int}` a step uses as a count or an index. Cucumber's `{int}`
/// matches negative numbers, which would otherwise wrap to a huge count.
pub(crate) fn non_negative(n: i64, what: &str) -> Result<usize, crate::step::StepError> {
  usize::try_from(n).map_err(|_| crate::step::StepError::from(format!("{what} must be zero or more, got {n}")))
}

/// An `{int}` a step uses as a pixel coordinate or a wheel delta.
pub(crate) fn pixels(n: i64) -> Result<f64, crate::step::StepError> {
  i32::try_from(n)
    .map(f64::from)
    .map_err(|_| crate::step::StepError::from(format!("{n} is out of range for a pixel value")))
}
