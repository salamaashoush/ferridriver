//! `ElementHandleJs`: QuickJS wrapper around `ferridriver::ElementHandle`.
//!
//! Phase-C surface covers lifecycle — `dispose`, `isDisposed`, `asJSHandle`
//! — enough to exercise the per-backend release paths from `run_script`.
//! Phase E bolts the ~25 Playwright DOM methods on top of this same class.

use ferridriver::ElementHandle;
use rquickjs::JsLifetime;
use rquickjs::class::Trace;

use crate::bindings::convert::FerriResultExt;

/// QuickJS-visible wrapper around a core [`ElementHandle`].
#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "ElementHandle")]
pub struct ElementHandleJs {
  #[qjs(skip_trace)]
  inner: ElementHandle,
}

impl ElementHandleJs {
  #[must_use]
  pub fn new(inner: ElementHandle) -> Self {
    Self { inner }
  }

  #[must_use]
  pub fn inner(&self) -> &ElementHandle {
    &self.inner
  }
}

#[rquickjs::methods]
impl ElementHandleJs {
  /// `true` once [`Self::dispose`] has run.
  #[qjs(get, rename = "isDisposed")]
  pub fn is_disposed(&self) -> bool {
    self.inner.is_disposed()
  }

  /// Release the underlying remote element. Idempotent.
  #[qjs(rename = "dispose")]
  pub async fn dispose(&self) -> rquickjs::Result<()> {
    self.inner.dispose().await.into_js()
  }

  /// Companion [`crate::bindings::js_handle::JSHandleJs`] sharing the
  /// same remote reference. Disposing either releases the remote and
  /// latches both into the disposed state.
  #[qjs(rename = "asJSHandle")]
  pub fn as_js_handle(&self) -> crate::bindings::js_handle::JSHandleJs {
    crate::bindings::js_handle::JSHandleJs::new(self.inner.as_js_handle().clone())
  }
}
