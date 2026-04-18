//! `JSHandle` — lifecycle object for an arbitrary JavaScript value in the page.
//!
//! Mirrors Playwright's `JSHandle` class
//! (`/tmp/playwright/packages/playwright-core/src/client/jsHandle.ts`). A handle
//! holds a backend-agnostic reference to a value that lives in the page (CDP
//! `Runtime.RemoteObjectId`, `BiDi` `sharedId`, or `WebKit` `window.__wr[id]`
//! index), plus the `Arc<Page>` the value was minted against. Callers can
//! pass the handle back into evaluate/eval-family calls or release the
//! underlying remote object via [`JSHandle::dispose`].
//!
//! ## Lifecycle contract
//!
//! - Every handle is created on exactly one page / execution context.
//! - `dispose()` is idempotent — first call releases, subsequent calls are
//!   no-ops.
//! - After dispose, any method that talks to the remote returns
//!   [`crate::error::FerriError::TargetClosed`] (Playwright raises
//!   `JavaScriptErrorInEvaluate` from the server for the same condition;
//!   we surface `TargetClosed` because the handle's target — the remote
//!   object — is gone).
//!
//! Not thread-local: handles are `Clone`, `Send`, and `Sync` so they can
//! flow through the `evaluate(fn, arg)` wire serialization just like any
//! other public type.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::backend::AnyPage;
use crate::element_handle::ElementHandle;
use crate::error::{FerriError, Result};
use crate::page::Page;
use crate::protocol::HandleId;

/// Backend-specific handle payload. Carries only the wire-level identifier;
/// the session/context/view is recovered from the owning `Page` at dispose /
/// evaluate time. Not public — callers interact via [`JSHandle`] and
/// [`ElementHandle`].
///
/// Each variant maps 1:1 onto the corresponding `protocol::HandleId` wire
/// variant — [`HandleRemote::to_handle_id`] converts one to the other at
/// the `evaluate(fn, arg)` serialization boundary.
#[derive(Debug, Clone)]
pub enum HandleRemote {
  /// CDP `Runtime.RemoteObjectId`. Released via `Runtime.releaseObject`.
  Cdp(Arc<str>),
  /// `BiDi` `SharedReference.sharedId` (plus optional `handle` field).
  /// Released via `script.disown`.
  Bidi { shared_id: String, handle: Option<String> },
  /// `WebKit` host IPC ref — the `ref_id` used to index `window.__wr`.
  /// Released via the new `Op::ReleaseRef` IPC op.
  WebKit(u64),
}

impl HandleRemote {
  /// Convert to the serialization-boundary [`HandleId`] form used by the
  /// protocol wire serializer. The two types exist separately so the
  /// internal `HandleRemote` can carry `Arc<str>` / owned strings
  /// optimized for local cloning, while `HandleId` stays serde-native for
  /// the wire path.
  #[must_use]
  pub fn to_handle_id(&self) -> HandleId {
    match self {
      Self::Cdp(obj) => HandleId::Cdp((**obj).to_string()),
      Self::Bidi { shared_id, handle } => HandleId::Bidi {
        shared_id: shared_id.clone(),
        handle: handle.clone(),
      },
      Self::WebKit(ref_id) => HandleId::WebKit(*ref_id),
    }
  }

  /// Inverse of [`Self::to_handle_id`]. Returns a [`HandleRemote`] ready
  /// to dispatch against an `AnyPage`. The conversion is lossless.
  #[must_use]
  pub fn from_handle_id(id: HandleId) -> Self {
    match id {
      HandleId::Cdp(obj) => Self::Cdp(Arc::from(obj)),
      HandleId::Bidi { shared_id, handle } => Self::Bidi { shared_id, handle },
      HandleId::WebKit(ref_id) => Self::WebKit(ref_id),
    }
  }
}

/// Handle to a JavaScript value living in a page.
///
/// Cheaply cloneable — every clone shares the same `disposed` flag so the
/// first `dispose()` wins. The remote object is released exactly once; later
/// calls through any clone return `Ok(())` without talking to the backend.
#[derive(Clone)]
pub struct JSHandle {
  page: Arc<Page>,
  remote: HandleRemote,
  disposed: Arc<AtomicBool>,
}

impl JSHandle {
  /// Construct a new handle. Internal — callers go through page factories
  /// like `Page::query_selector` (`ElementHandle`) or
  /// `Page::evaluate_handle` (`JSHandle`).
  pub(crate) fn new(page: Arc<Page>, remote: HandleRemote) -> Self {
    Self {
      page,
      remote,
      disposed: Arc::new(AtomicBool::new(false)),
    }
  }

  /// The owning page.
  #[must_use]
  pub fn page(&self) -> &Arc<Page> {
    &self.page
  }

  /// Raw backend reference. Internal — used by evaluate-family calls to
  /// thread the remote through the protocol.
  #[must_use]
  pub fn remote(&self) -> &HandleRemote {
    &self.remote
  }

  /// `true` once [`Self::dispose`] has run for any clone of this handle.
  #[must_use]
  pub fn is_disposed(&self) -> bool {
    self.disposed.load(Ordering::SeqCst)
  }

  /// Borrow the `AnyPage` for backend dispatch. `pub(crate)` because the
  /// public Page API doesn't expose `AnyPage`.
  pub(crate) fn any_page(&self) -> &AnyPage {
    self.page.inner()
  }

  /// Claim the disposed flag. Returns `true` on the first call per handle
  /// graph, `false` thereafter. Internal — used to short-circuit
  /// idempotent dispose.
  fn claim_dispose(&self) -> bool {
    self
      .disposed
      .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
      .is_ok()
  }

  /// Release the underlying remote object on the backend.
  ///
  /// - CDP: `Runtime.releaseObject { objectId }`.
  /// - `BiDi`: `script.disown { handles, target }`.
  /// - `WebKit`: `Op::ReleaseRef` over IPC — deletes the entry from the
  ///   host's `window.__wr` map.
  ///
  /// Idempotent — first call wins; later calls on any clone return
  /// `Ok(())` without a backend round-trip.
  ///
  /// # Errors
  ///
  /// Forwards the backend's dispose error if the protocol call fails.
  /// On a genuine failure the `disposed` flag is rolled back so the
  /// caller can retry; on success the flag is latched and every
  /// subsequent call short-circuits without a backend round-trip.
  pub async fn dispose(&self) -> Result<()> {
    if !self.claim_dispose() {
      return Ok(());
    }
    let result = self.any_page().release_handle(&self.remote).await;
    if result.is_err() {
      // Roll back the flag so the caller can retry the failed release.
      // Idempotence is preserved on success because the flag stays
      // latched; only failures un-latch.
      self.disposed.store(false, Ordering::SeqCst);
    }
    result
  }

  /// Return this handle as an `ElementHandle` if its remote object is a
  /// DOM element. Playwright's `JSHandle.asElement()` inspects an
  /// initializer field set by the server when the remote is known to
  /// be a DOM node; ferridriver's core `JSHandle` does not (yet) carry
  /// that marker, so this method always returns `None`.
  ///
  /// Element-typed handles are produced by
  /// [`ElementHandle::from_any_element`] and callers obtain them
  /// directly from [`crate::page::Page::query_selector`] /
  /// `Locator::element_handle`. Phase D extends this method to return
  /// `Some(ElementHandle)` when the underlying remote describes a DOM
  /// node — decided by the CDP `RemoteObject.subtype` /
  /// `BiDi` `RemoteValue::Node` / `WebKit` value-type round-trip that
  /// will ship with `evaluate_handle`.
  #[allow(clippy::unused_self, reason = "phase-C stub; phase-D wires remote-type inspection")]
  #[must_use]
  pub fn as_element(&self) -> Option<ElementHandle> {
    None
  }
}

impl std::fmt::Debug for JSHandle {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("JSHandle")
      .field("remote", &self.remote)
      .field("disposed", &self.is_disposed())
      .finish_non_exhaustive()
  }
}

/// Error raised when a caller tries to use a `JSHandle` / `ElementHandle`
/// whose underlying remote has been released.
///
/// Matches Playwright's message text — the server's
/// `JavaScriptErrorInEvaluate` carries `"JSHandle is disposed"` in the
/// same situation. Consumers that dispatch on error content can match
/// the substring without coupling to a dedicated `FerriError` variant.
pub(crate) fn disposed_error() -> FerriError {
  FerriError::TargetClosed {
    reason: Some("JSHandle is disposed".to_string()),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn handle_remote_roundtrips_through_handle_id() {
    let cases = [
      HandleRemote::Cdp(Arc::from("obj-42")),
      HandleRemote::Bidi {
        shared_id: "shared-42".into(),
        handle: Some("h-1".into()),
      },
      HandleRemote::Bidi {
        shared_id: "shared-43".into(),
        handle: None,
      },
      HandleRemote::WebKit(42),
    ];
    for original in cases {
      let id = original.to_handle_id();
      let back = HandleRemote::from_handle_id(id);
      // PartialEq not derived (Arc<str> comparison quirks), compare by
      // stringifying via Debug.
      assert_eq!(format!("{original:?}"), format!("{back:?}"));
    }
  }

  #[test]
  fn disposed_error_message_matches_playwright() {
    let e = disposed_error();
    assert!(e.to_string().contains("JSHandle is disposed"), "message drift: {e}");
    assert_eq!(e.name(), "TargetClosedError");
  }
}
