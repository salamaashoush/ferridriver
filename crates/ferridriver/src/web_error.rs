//! `WebError` — live handle for page-side unhandled errors and unhandled promise rejections.
//!
//! Mirrors Playwright's client-side `WebError` from
//! `/tmp/playwright/packages/playwright-core/src/client/webError.ts` and
//! the server-side `addPageError` fan-out from
//! `/tmp/playwright/packages/playwright-core/src/server/page.ts:425`
//! (page-scoped `'pageerror'`) plus
//! `/tmp/playwright/packages/playwright-core/src/server/browserContext.ts:54`
//! (context-scoped `'weberror'`).
//!
//! Replaces the previous `PageEvent::PageError(String)` variant that
//! leaked a flat message string — Rule 3 violation. A live `WebError`
//! carries `{ name, message, stack }` matching JS `Error`'s shape plus a
//! weak back-reference to the owning page.
//!
//! Usage:
//!
//! ```ignore
//! page.on("pageerror", Arc::new(|event| {
//!     if let PageEvent::PageError(err) = event {
//!         eprintln!("[{}] {}\n{}", err.error().name, err.error().message, err.error().stack);
//!     }
//! }));
//! ```

use std::sync::Arc;

use crate::console_message::ConsoleMessageLocation;
use crate::page::Page;

/// JS `Error`-shaped payload: `name` (`'Error'`, `'TypeError'`, etc.),
/// `message`, and `stack`. Matches Playwright's `WebError.error(): Error`
/// return shape where `Error` is the native JS class.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ErrorDetails {
  pub name: String,
  pub message: String,
  pub stack: String,
}

impl ErrorDetails {
  /// Construct from `(name, message, stack)` triples captured directly
  /// off the protocol's exception payload.
  #[must_use]
  pub fn new(name: impl Into<String>, message: impl Into<String>, stack: impl Into<String>) -> Self {
    Self {
      name: name.into(),
      message: message.into(),
      stack: stack.into(),
    }
  }
}

/// Live web-error handle. Cheaply cloneable (Arc-based). Observed via
/// `page.on('pageerror', cb)` / `page.waitForEvent('pageerror')`
/// (page-scoped) and `context.on('weberror', cb)` /
/// `context.waitForEvent('weberror')` (context-scoped — any page in
/// the context).
#[derive(Clone)]
pub struct WebError {
  inner: Arc<WebErrorState>,
}

struct WebErrorState {
  error: ErrorDetails,
  /// Source location of the error, taken from the top stack frame.
  /// Playwright's `WebError.location()` returns `{ url, line, column }`,
  /// defaulting to `{ "", 0, 0 }` when no stack frame is available
  /// (`crProtocolHelper.ts::stackTraceToLocation`).
  location: ConsoleMessageLocation,
  /// Weak back-reference to the owning page. `WebError::page` upgrades
  /// it; returns `None` if the page has been dropped or the backend
  /// emitter pre-dates the outer `Arc<Page>` (matches Playwright's
  /// `createHandle(context, arg)` guard pattern from the console path).
  page: std::sync::Weak<Page>,
}

impl WebError {
  /// Build a `WebError` with a strong page back-reference. Called by
  /// backend listeners that hold the upgraded `Arc<Page>` at event
  /// build time.
  #[must_use]
  pub fn new(page: &Arc<Page>, error: ErrorDetails, location: ConsoleMessageLocation) -> Self {
    Self {
      inner: Arc::new(WebErrorState {
        error,
        location,
        page: Arc::downgrade(page),
      }),
    }
  }

  /// Build a `WebError` without a page back-reference. Used where the
  /// backend listener spawns before the outer `Arc<Page>` is populated
  /// (`CDP` / `BiDi` race window, `WebKit` pre-registration drain).
  /// `page()` returns `None`.
  #[must_use]
  pub fn new_detached(error: ErrorDetails, location: ConsoleMessageLocation) -> Self {
    Self {
      inner: Arc::new(WebErrorState {
        error,
        location,
        page: std::sync::Weak::new(),
      }),
    }
  }

  /// Owning page (weak). Playwright: `webError.page(): Page | null`.
  /// Returns `None` if the page has been dropped.
  #[must_use]
  pub fn page(&self) -> Option<Arc<Page>> {
    self.inner.page.upgrade()
  }

  /// JS `Error`-shaped payload. Playwright: `webError.error(): Error`.
  #[must_use]
  pub fn error(&self) -> &ErrorDetails {
    &self.inner.error
  }

  /// Source location of the error. Playwright:
  /// `webError.location(): { url, lineNumber, columnNumber }`.
  #[must_use]
  pub fn location(&self) -> &ConsoleMessageLocation {
    &self.inner.location
  }
}

impl std::fmt::Debug for WebError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("WebError")
      .field("name", &self.inner.error.name)
      .field("message", &self.inner.error.message)
      .field("stack_len", &self.inner.error.stack.len())
      .finish()
  }
}
