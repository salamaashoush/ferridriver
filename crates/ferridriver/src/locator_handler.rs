//! Locator handler registry — Playwright's `addLocatorHandler` /
//! `removeLocatorHandler`.
//!
//! A handler watches a locator (typically an overlay/modal). Before every
//! actionability retry the registry checks each handler's locator for
//! visibility; when visible, the handler callback runs (it dismisses the
//! overlay), the registry waits for the locator to become hidden (unless
//! `no_wait_after`), then the original action continues.
//!
//! Unlike Playwright, ferridriver has no client/server split: the handler
//! callback runs in-process. The registry therefore stores the callback
//! directly rather than emitting a `locatorHandlerTriggered` event.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;

use crate::error::Result;
use crate::locator::Locator;

/// Callback invoked when a registered handler's locator becomes visible.
/// Receives a [`Locator`] bound to the handler's selector (matching
/// Playwright's `handler(locator)` signature).
pub type LocatorHandlerFn = Arc<dyn Fn(Locator) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> + Send + Sync>;

struct LocatorHandlerEntry {
  uid: u64,
  /// Selector used both for the visibility checkpoint and for building the
  /// [`Locator`] passed to the callback.
  selector: String,
  callback: LocatorHandlerFn,
  /// Remaining invocations. `None` means unlimited. `Some(0)` is removed.
  times: Option<u32>,
  /// When true, skip waiting for the locator to become hidden after the
  /// handler runs (Playwright `noWaitAfter`).
  no_wait_after: bool,
}

/// Registry of locator handlers stored on a [`crate::page::Page`].
///
/// `running` guards against re-entrancy: handlers must not fire from inside a
/// handler callback (Playwright's `_locatorHandlerRunningCounter`).
#[derive(Default)]
pub(crate) struct LocatorHandlerRegistry {
  inner: Mutex<RegistryState>,
}

#[derive(Default)]
struct RegistryState {
  handlers: Vec<LocatorHandlerEntry>,
  next_uid: u64,
  running: bool,
}

impl LocatorHandlerRegistry {
  /// Register a handler for `selector`. Returns the assigned uid. A `times`
  /// of `Some(0)` registers nothing and returns `None`.
  pub(crate) fn register(
    &self,
    selector: String,
    callback: LocatorHandlerFn,
    times: Option<u32>,
    no_wait_after: bool,
  ) -> Option<u64> {
    if times == Some(0) {
      return None;
    }
    let mut state = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    state.next_uid += 1;
    let uid = state.next_uid;
    state.handlers.push(LocatorHandlerEntry {
      uid,
      selector,
      callback,
      times,
      no_wait_after,
    });
    Some(uid)
  }

  /// Remove every handler whose selector equals `selector` (Playwright's
  /// `removeLocatorHandler(locator)` compares by locator equality, which for
  /// our purposes is selector-string equality).
  pub(crate) fn remove_by_selector(&self, selector: &str) {
    let mut state = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    state.handlers.retain(|h| h.selector != selector);
  }

  fn is_empty(&self) -> bool {
    self
      .inner
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .handlers
      .is_empty()
  }
}

/// Run the locator-handler checkpoint for `page` before an actionability
/// attempt. Mirrors Playwright's `_performLocatorHandlersCheckpoint`:
///
/// 1. Skip entirely if a handler is already running (re-entrancy guard).
/// 2. For each handler, if its locator is visible, invoke the callback.
/// 3. Decrement `times`; auto-remove when it hits zero.
/// 4. Unless `no_wait_after`, wait for the locator to become hidden.
///
/// Errors from individual handlers/visibility checks are swallowed so the
/// outer action retry loop continues — exactly as Playwright keeps polling.
pub(crate) async fn perform_checkpoint(page: &Arc<crate::page::Page>) {
  let registry = page.locator_handlers();
  if registry.is_empty() {
    return;
  }
  {
    let mut state = registry.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if state.running {
      return;
    }
    state.running = true;
  }

  // Snapshot the (uid, selector, callback, no_wait_after) tuples so we don't
  // hold the lock across awaits.
  let snapshot: Vec<(u64, String, LocatorHandlerFn, bool)> = {
    let state = registry.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    state
      .handlers
      .iter()
      .map(|h| (h.uid, h.selector.clone(), Arc::clone(&h.callback), h.no_wait_after))
      .collect()
  };

  for (uid, selector, callback, no_wait_after) in snapshot {
    let locator = page.locator(&selector, None);
    let visible = locator.is_visible().await.unwrap_or(false);
    if !visible {
      continue;
    }

    // Decrement before running so a callback that triggers another checkpoint
    // (guarded by `running`) sees the updated count once we return.
    let should_run = {
      let mut state = registry.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      match state.handlers.iter_mut().find(|h| h.uid == uid) {
        Some(entry) => match entry.times {
          Some(0) => false,
          Some(ref mut n) => {
            *n -= 1;
            true
          },
          None => true,
        },
        None => false,
      }
    };
    if !should_run {
      continue;
    }

    let _ = callback(locator.clone()).await;

    let remove = {
      let state = registry.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      state
        .handlers
        .iter()
        .find(|h| h.uid == uid)
        .is_some_and(|h| h.times == Some(0))
    };
    if remove {
      let mut state = registry.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      state.handlers.retain(|h| h.uid != uid);
    }

    if !no_wait_after {
      // Best-effort wait for the overlay to disappear so the original action
      // doesn't immediately re-trip the handler.
      let _ = wait_hidden(&locator).await;
    }
  }

  let mut state = registry.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
  state.running = false;
}

/// Poll until the locator is no longer visible, capped at 30s.
async fn wait_hidden(locator: &Locator) -> Result<()> {
  let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
  loop {
    if !locator.is_visible().await.unwrap_or(false) {
      return Ok(());
    }
    if std::time::Instant::now() >= deadline {
      return Ok(());
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
  }
}
