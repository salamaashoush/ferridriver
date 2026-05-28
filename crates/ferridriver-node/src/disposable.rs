//! NAPI Disposable class — mirrors Playwright's `Disposable` / `DisposableStub`
//! (`client/disposable.ts`). Returned from `route()` and `addInitScript()`;
//! `dispose()` reverses the registration. `remove()` is an alias.

use crate::error::IntoNapi;
use napi::Result;
use napi_derive::napi;
use std::sync::Arc;

/// Reverses a single registration (route / init-script). Returned from
/// `page.route`, `page.addInitScript`, and their context equivalents.
#[napi]
pub struct Disposable {
  inner: Arc<ferridriver::Disposable>,
}

impl Disposable {
  pub(crate) fn wrap(inner: ferridriver::Disposable) -> Self {
    Self { inner: Arc::new(inner) }
  }
}

#[napi]
impl Disposable {
  /// Reverse the registration. Idempotent — repeat calls are no-ops.
  #[napi]
  pub async fn dispose(&self) -> Result<()> {
    let inner = Arc::clone(&self.inner);
    inner.dispose().await.into_napi()
  }

  /// Alias for [`Disposable::dispose`].
  #[napi]
  pub async fn remove(&self) -> Result<()> {
    let inner = Arc::clone(&self.inner);
    inner.dispose().await.into_napi()
  }
}
