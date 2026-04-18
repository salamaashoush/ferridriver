//! `ElementHandle` — `JSHandle` specialisation for DOM elements.
//!
//! Mirrors Playwright's `ElementHandle extends JSHandle`
//! (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts`).
//! We use composition instead of inheritance: an `ElementHandle` wraps
//! both a [`JSHandle`] (for lifecycle + evaluate) and an `AnyElement`
//! (for DOM-specific actions that already have per-backend impls
//! threaded through [`crate::actions`]).
//!
//! Phase C's minimum viable surface covers lifecycle + `as_js_handle()`.
//! Phase E bolts the ~25 Playwright DOM action methods on top of this
//! same type.

use std::sync::Arc;

use crate::backend::AnyElement;
use crate::error::Result;
use crate::js_handle::{HandleRemote, JSHandle, disposed_error};
use crate::page::Page;

/// Handle to a DOM element living in a page.
///
/// Cloneable. Every clone shares the same `disposed` flag via the
/// underlying [`JSHandle`].
#[derive(Clone)]
pub struct ElementHandle {
  js_handle: JSHandle,
  /// Backend element captured at materialisation time. Phase-E action
  /// methods delegate through this to the existing per-backend
  /// `AnyElement::click` / `fill` / etc. helpers rather than
  /// re-resolving the DOM node from the `HandleRemote` on every call.
  /// Carried as `Arc` so clones of `ElementHandle` share the backend
  /// element cheaply.
  #[allow(dead_code)]
  element: Arc<AnyElement>,
}

impl ElementHandle {
  /// Construct an `ElementHandle` from an existing backend `AnyElement`.
  /// Called from `Page::query_selector` / `Locator::element_handle` /
  /// etc. once the backend has minted a remote reference.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend element has no addressable handle
  /// (should never happen for elements that came out of `find_element`
  /// / `evaluate_to_element`).
  pub(crate) async fn from_any_element(page: Arc<Page>, element: AnyElement) -> Result<Self> {
    let remote = crate::backend::element_handle_remote(&element).await?;
    let js_handle = JSHandle::new(page, remote);
    Ok(Self {
      js_handle,
      element: Arc::new(element),
    })
  }

  /// Underlying [`JSHandle`] — exposes lifecycle + evaluate (phase D).
  #[must_use]
  pub fn as_js_handle(&self) -> &JSHandle {
    &self.js_handle
  }

  /// Backend-specific remote reference.
  #[must_use]
  pub fn remote(&self) -> &HandleRemote {
    self.js_handle.remote()
  }

  /// Owning page.
  #[must_use]
  pub fn page(&self) -> &Arc<Page> {
    self.js_handle.page()
  }

  /// Borrow the backend `AnyElement`. Phase-E action methods use this to
  /// delegate to the per-backend element implementations; phase F's
  /// materialisation helpers use it to round-trip through locator /
  /// frame APIs.
  #[allow(dead_code)]
  pub(crate) fn any_element(&self) -> &AnyElement {
    &self.element
  }

  /// Whether the backing remote has been released.
  #[must_use]
  pub fn is_disposed(&self) -> bool {
    self.js_handle.is_disposed()
  }

  /// Release the remote object. See [`JSHandle::dispose`] for semantics.
  ///
  /// # Errors
  ///
  /// Forwards the backend's dispose error. Idempotent.
  pub async fn dispose(&self) -> Result<()> {
    self.js_handle.dispose().await
  }

  /// Short-circuit helper for phase-E action methods: returns
  /// [`crate::error::FerriError::TargetClosed`] with the Playwright
  /// `"JSHandle is disposed"` message when this handle has already
  /// been released.
  ///
  /// # Errors
  ///
  /// Returns the disposed-handle error if [`Self::is_disposed`].
  #[allow(dead_code)]
  pub(crate) fn ensure_live(&self) -> Result<()> {
    if self.is_disposed() {
      return Err(disposed_error());
    }
    Ok(())
  }
}

impl std::fmt::Debug for ElementHandle {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("ElementHandle")
      .field("remote", self.remote())
      .field("disposed", &self.is_disposed())
      .finish()
  }
}
