//! QuickJS binding for `ferridriver::Disposable` — mirrors Playwright's
//! `Disposable` / `DisposableStub` (`client/disposable.ts`). Returned from
//! `page.route` / `page.addInitScript` (and context equivalents); `dispose()`
//! reverses the registration, `remove()` is an alias.

use crate::bindings::convert::FerriResultExt;
use rquickjs::{JsLifetime, class::Trace};
use std::sync::Arc;

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "Disposable")]
pub struct DisposableJs {
  #[qjs(skip_trace)]
  inner: Arc<ferridriver::Disposable>,
}

impl DisposableJs {
  #[must_use]
  pub fn new(inner: ferridriver::Disposable) -> Self {
    Self { inner: Arc::new(inner) }
  }
}

#[rquickjs::methods]
impl DisposableJs {
  /// Reverse the registration. Idempotent — repeat calls are no-ops.
  #[qjs(rename = "dispose")]
  pub async fn dispose(&self) -> rquickjs::Result<()> {
    let inner = Arc::clone(&self.inner);
    inner.dispose().await.into_js()
  }

  /// Alias for `dispose()`.
  #[qjs(rename = "remove")]
  pub async fn remove(&self) -> rquickjs::Result<()> {
    let inner = Arc::clone(&self.inner);
    inner.dispose().await.into_js()
  }
}
