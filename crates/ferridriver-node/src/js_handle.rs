//! JSHandle class -- NAPI binding for `ferridriver::JSHandle`.
//!
//! Mirrors Playwright's `JSHandle<T>` interface
//! (`/tmp/playwright/packages/playwright-core/types/types.d.ts`). The phase-C
//! surface covers lifecycle — `dispose`, `isDisposed`, `asElement` — which
//! is enough to prove the per-backend `Runtime.releaseObject` /
//! `script.disown` / `Op::ReleaseRef` paths end-to-end. Phase D extends this
//! with `evaluate(fn, arg)`, `evaluateHandle`, `getProperties`,
//! `getProperty`, and `jsonValue`.

use crate::error::IntoNapi;
use napi::Result;
use napi_derive::napi;

/// Handle to a JavaScript value living in a page.
///
/// Created via `page.evaluateHandle(...)` (phase D) or surfaced
/// indirectly through [`crate::element_handle::ElementHandle::asJSHandle`].
/// Clones share the same underlying remote object — `dispose()` on any
/// clone releases the object and latches every sibling into the disposed
/// state.
#[napi]
pub struct JSHandle {
  inner: ferridriver::JSHandle,
}

impl JSHandle {
  pub(crate) fn wrap(inner: ferridriver::JSHandle) -> Self {
    Self { inner }
  }

  #[allow(dead_code)]
  pub(crate) fn inner_ref(&self) -> &ferridriver::JSHandle {
    &self.inner
  }
}

#[napi]
impl JSHandle {
  /// `true` once [`Self::dispose`] has run for this handle (or any clone
  /// sharing the same remote).
  #[napi(getter)]
  pub fn is_disposed(&self) -> bool {
    self.inner.is_disposed()
  }

  /// Release the underlying remote object. Playwright:
  /// `jsHandle.dispose(): Promise<void>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/jsHandle.ts`).
  ///
  /// Idempotent — calling dispose twice returns successfully the second
  /// time without a second backend round-trip. On protocol failure the
  /// disposed flag is rolled back so the caller can retry.
  #[napi]
  pub async fn dispose(&self) -> Result<()> {
    self.inner.dispose().await.into_napi()
  }

  /// Return this handle as an `ElementHandle` if its remote object is a
  /// DOM element, else `null`. Phase-C always returns `null` at the
  /// `JSHandle` layer; `ElementHandle` exposes its own
  /// [`crate::element_handle::ElementHandle::asJsHandle`] for the
  /// opposite direction. Playwright:
  /// `jsHandle.asElement(): ElementHandle | null`.
  #[napi]
  pub fn as_element(&self) -> Option<crate::element_handle::ElementHandle> {
    self.inner.as_element().map(crate::element_handle::ElementHandle::wrap)
  }
}
