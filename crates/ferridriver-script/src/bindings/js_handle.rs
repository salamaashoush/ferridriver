//! `JSHandleJs`: QuickJS wrapper around `ferridriver::JSHandle`.
//!
//! Mirrors the NAPI surface in `crates/ferridriver-node/src/js_handle.rs`
//! and Playwright's `JSHandle` TS interface. Phase-C surface covers lifecycle
//! only — `dispose`, `isDisposed`, `asElement`. Phase D extends with
//! `evaluate`, `evaluateHandle`, `getProperties`, `getProperty`, `jsonValue`.

use ferridriver::JSHandle;
use rquickjs::JsLifetime;
use rquickjs::class::Trace;

use crate::bindings::convert::FerriResultExt;

/// QuickJS-visible wrapper around a core [`JSHandle`].
///
/// Held without `Arc` because [`JSHandle`] is itself `Clone` and shares
/// its dispose flag through an internal `Arc<AtomicBool>`.
#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "JSHandle")]
pub struct JSHandleJs {
  #[qjs(skip_trace)]
  inner: JSHandle,
}

impl JSHandleJs {
  #[must_use]
  pub fn new(inner: JSHandle) -> Self {
    Self { inner }
  }

  #[must_use]
  pub fn inner(&self) -> &JSHandle {
    &self.inner
  }
}

#[rquickjs::methods]
impl JSHandleJs {
  /// `true` once [`Self::dispose`] has run.
  #[qjs(get, rename = "isDisposed")]
  pub fn is_disposed(&self) -> bool {
    self.inner.is_disposed()
  }

  /// Release the underlying remote object. Playwright:
  /// `jsHandle.dispose(): Promise<void>`. Idempotent — calling twice
  /// short-circuits the second time.
  #[qjs(rename = "dispose")]
  pub async fn dispose(&self) -> rquickjs::Result<()> {
    self.inner.dispose().await.into_js()
  }

  /// Return this handle as an `ElementHandle` if its remote is a DOM
  /// element, else `null`. Phase-C always returns `null` at the
  /// `JSHandle` layer; obtain `ElementHandle` via `page.querySelector`.
  #[qjs(rename = "asElement")]
  pub fn as_element(&self) -> Option<crate::bindings::element_handle::ElementHandleJs> {
    self
      .inner
      .as_element()
      .map(crate::bindings::element_handle::ElementHandleJs::new)
  }
}
