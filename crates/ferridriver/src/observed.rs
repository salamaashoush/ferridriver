//! Per-page console / page-error retention backing
//! `page.consoleMessages()` / `page.pageErrors()` (Playwright parity).
//!
//! Buffers live on the backend page (like the frame cache) so every
//! `crate::Page` wrapper minted over the same backend page sees one
//! history. The `seed_frame_cache` listener task pushes entries as
//! events arrive; a main-frame navigation records a watermark so the
//! default `since-navigation` filter can slice without copying history.

use crate::console_message::ConsoleMessage;
use crate::web_error::WebError;

/// Retention cap per buffer, matching Playwright's `ensureArrayLimit`
/// guard against unbounded growth on chatty pages.
const MAX_ENTRIES: usize = 200;

/// Filter for [`crate::Page::console_messages`] / [`crate::Page::page_errors`].
/// Playwright: `{ filter?: 'all' | 'since-navigation' }`, defaulting to
/// `since-navigation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ObservedFilter {
  /// Everything retained since page creation (up to the cap).
  All,
  /// Only entries recorded after the last main-frame navigation.
  #[default]
  SinceNavigation,
}

impl ObservedFilter {
  /// Parse the Playwright wire string. Unknown values fall back to the
  /// default (`since-navigation`), mirroring the server's
  /// `filter === 'all'` check.
  #[must_use]
  pub fn parse(s: Option<&str>) -> Self {
    match s {
      Some("all") => Self::All,
      _ => Self::SinceNavigation,
    }
  }
}

struct Buffer<T> {
  entries: Vec<T>,
  nav_mark: usize,
}

impl<T> Default for Buffer<T> {
  fn default() -> Self {
    Self {
      entries: Vec::new(),
      nav_mark: 0,
    }
  }
}

impl<T: Clone> Buffer<T> {
  fn push(&mut self, entry: T) {
    self.entries.push(entry);
    if self.entries.len() > MAX_ENTRIES {
      let overflow = self.entries.len() - MAX_ENTRIES;
      self.entries.drain(..overflow);
      self.nav_mark = self.nav_mark.saturating_sub(overflow);
    }
  }

  fn mark_navigation(&mut self) {
    self.nav_mark = self.entries.len();
  }

  fn snapshot(&self, filter: ObservedFilter) -> Vec<T> {
    match filter {
      ObservedFilter::All => self.entries.clone(),
      ObservedFilter::SinceNavigation => self.entries[self.nav_mark.min(self.entries.len())..].to_vec(),
    }
  }

  fn clear(&mut self) {
    self.entries.clear();
    self.nav_mark = 0;
  }
}

/// Console + page-error history for one backend page.
#[derive(Default)]
pub(crate) struct ObservedBuffers {
  console: Buffer<ConsoleMessage>,
  errors: Buffer<WebError>,
}

impl ObservedBuffers {
  pub(crate) fn push_console(&mut self, msg: ConsoleMessage) {
    self.console.push(msg);
  }

  pub(crate) fn push_error(&mut self, err: WebError) {
    self.errors.push(err);
  }

  pub(crate) fn mark_navigation(&mut self) {
    self.console.mark_navigation();
    self.errors.mark_navigation();
  }

  pub(crate) fn console_messages(&self, filter: ObservedFilter) -> Vec<ConsoleMessage> {
    self.console.snapshot(filter)
  }

  pub(crate) fn page_errors(&self, filter: ObservedFilter) -> Vec<WebError> {
    self.errors.snapshot(filter)
  }

  pub(crate) fn clear_console(&mut self) {
    self.console.clear();
  }

  pub(crate) fn clear_errors(&mut self) {
    self.errors.clear();
  }
}
