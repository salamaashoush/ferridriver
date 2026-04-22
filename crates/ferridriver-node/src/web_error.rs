//! NAPI binding for [`ferridriver::web_error::WebError`].
//!
//! Mirrors Playwright's client-side `WebError` from
//! `/tmp/playwright/packages/playwright-core/src/client/webError.ts`:
//! sync `page()` / `error()` getters, with `error()` returning a plain
//! object matching JS `Error`'s `{ name, message, stack }` shape.

use ferridriver::web_error::WebError as CoreWebError;
use napi_derive::napi;

/// Live web-error handle — observed via
/// `page.waitForEvent('pageerror')` / `page.on('pageerror', cb)`
/// (page-scoped) or `context.waitForEvent('weberror')` /
/// `context.on('weberror', cb)` (context-scoped).
#[napi]
pub struct WebError {
  pub(crate) inner: CoreWebError,
}

impl WebError {
  pub(crate) fn from_core(inner: CoreWebError) -> Self {
    Self { inner }
  }
}

/// JS `Error`-shaped payload. Matches Playwright's `WebError.error()`
/// return shape byte-for-byte.
#[napi(object)]
pub struct WebErrorPayload {
  pub name: String,
  pub message: String,
  pub stack: String,
}

#[napi]
impl WebError {
  /// Playwright: `webError.page(): Page | null`. Returns the page the
  /// error originated on, or `null` if the page has been dropped.
  #[napi(ts_return_type = "Page | null")]
  pub fn page(&self) -> Option<crate::page::Page> {
    self.inner.page().map(crate::page::Page::wrap)
  }

  /// Playwright: `webError.error(): Error`. Returns `{ name, message,
  /// stack }` — a plain object rather than a thrown JS `Error` so
  /// consumers can inspect the fields without `try/catch`.
  #[napi]
  pub fn error(&self) -> WebErrorPayload {
    let d = self.inner.error();
    WebErrorPayload {
      name: d.name.clone(),
      message: d.message.clone(),
      stack: d.stack.clone(),
    }
  }
}
