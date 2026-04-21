//! NAPI binding for [`ferridriver::console_message::ConsoleMessage`].
//!
//! Mirrors Playwright's client-side `ConsoleMessage` from
//! `/tmp/playwright/packages/playwright-core/src/client/consoleMessage.ts`:
//! sync `url()`-style getters on `type()` / `text()` / `args()` /
//! `location()` / `page()` / `timestamp()`.

use ferridriver::console_message::ConsoleMessage as CoreConsoleMessage;
use napi_derive::napi;

/// Live console-message handle — observed via
/// `page.waitForEvent('console')` or `page.on('console', cb)`.
#[napi]
pub struct ConsoleMessage {
  pub(crate) inner: CoreConsoleMessage,
}

impl ConsoleMessage {
  pub(crate) fn from_core(inner: CoreConsoleMessage) -> Self {
    Self { inner }
  }
}

/// Source location of a `console.*` call. Matches Playwright's
/// `ConsoleMessageLocation` wire shape verbatim.
#[napi(object)]
pub struct ConsoleMessageLocation {
  pub url: String,
  pub line_number: u32,
  pub column_number: u32,
}

#[napi]
impl ConsoleMessage {
  /// Playwright: `consoleMessage.type(): string`. Returns `'log'` /
  /// `'info'` / `'warning'` / `'error'` / `'debug'` / `'dir'` / etc.
  #[napi(js_name = "type")]
  pub fn type_str(&self) -> String {
    self.inner.type_str().to_string()
  }

  /// Playwright: `consoleMessage.text(): string`.
  #[napi]
  pub fn text(&self) -> String {
    self.inner.text().to_string()
  }

  /// Playwright: `consoleMessage.args(): JSHandle[]`.
  #[napi]
  pub fn args(&self) -> Vec<crate::js_handle::JSHandle> {
    self
      .inner
      .args()
      .iter()
      .cloned()
      .map(crate::js_handle::JSHandle::wrap)
      .collect()
  }

  /// Playwright: `consoleMessage.location(): ConsoleMessageLocation`.
  #[napi]
  pub fn location(&self) -> ConsoleMessageLocation {
    let loc = self.inner.location();
    ConsoleMessageLocation {
      url: loc.url.clone(),
      line_number: loc.line_number,
      column_number: loc.column_number,
    }
  }

  /// Playwright: `consoleMessage.page(): Page | null`. Returns `null`
  /// if the owning page has been dropped.
  #[napi(ts_return_type = "Page | null")]
  pub fn page(&self) -> Option<crate::page::Page> {
    self.inner.page().map(crate::page::Page::wrap)
  }

  /// Playwright: `consoleMessage.timestamp(): number`. Milliseconds
  /// since epoch.
  #[napi]
  pub fn timestamp(&self) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
      self.inner.timestamp() as f64
    }
  }
}
