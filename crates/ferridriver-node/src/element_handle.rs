//! ElementHandle class -- NAPI binding for `ferridriver::ElementHandle`.
//!
//! Mirrors Playwright's `ElementHandle<T extends Node>` interface
//! (`/tmp/playwright/packages/playwright-core/types/types.d.ts`).
//!
//! The phase-C surface covers lifecycle (`dispose`, `isDisposed`,
//! `asJsHandle`) so the per-backend release paths can be exercised from
//! JS via `page.query_selector` + `handle.dispose()`. Phase E bolts the
//! Playwright DOM methods on top of this same class.

use crate::error::IntoNapi;
use napi::Result;
use napi_derive::napi;

/// Handle to a DOM element living in a page.
///
/// Created via `page.querySelector(selector)` — phase F adds
/// `page.querySelectorAll`, `locator.elementHandle`, and
/// `locator.elementHandles` as additional materialisation paths.
#[napi]
pub struct ElementHandle {
  inner: ferridriver::ElementHandle,
}

impl ElementHandle {
  pub(crate) fn wrap(inner: ferridriver::ElementHandle) -> Self {
    Self { inner }
  }

  #[allow(dead_code)]
  pub(crate) fn inner_ref(&self) -> &ferridriver::ElementHandle {
    &self.inner
  }
}

#[napi]
impl ElementHandle {
  /// `true` once [`Self::dispose`] has run for this handle (or any clone
  /// sharing the same remote). Playwright:
  /// `elementHandle.isDisposed` — exposed as a `boolean` getter here to
  /// match the JS convention.
  #[napi(getter)]
  pub fn is_disposed(&self) -> bool {
    self.inner.is_disposed()
  }

  /// Release the underlying remote element. See
  /// [`crate::js_handle::JSHandle::dispose`] for semantics.
  #[napi]
  pub async fn dispose(&self) -> Result<()> {
    self.inner.dispose().await.into_napi()
  }

  /// Return this handle as a general `JSHandle`. Playwright:
  /// `elementHandle` is-a `JSHandle`, so the cast is always infallible
  /// — we surface a companion `JSHandle` wrapping the same remote.
  /// The two handles share the same dispose flag: disposing either
  /// releases the remote.
  #[napi]
  pub fn as_js_handle(&self) -> crate::js_handle::JSHandle {
    crate::js_handle::JSHandle::wrap(self.inner.as_js_handle().clone())
  }

  /// Playwright: `elementHandle.evaluate(pageFunction, arg?)`.
  /// Delegates through the companion `JSHandle`.
  #[napi(ts_args_type = "fnSource: string, arg?: unknown")]
  pub async fn evaluate_with_arg(
    &self,
    fn_source: String,
    arg: Option<serde_json::Value>,
  ) -> Result<Option<serde_json::Value>> {
    let serialized = crate::page::build_serialized_argument(arg);
    let result = self
      .inner
      .as_js_handle()
      .evaluate_with_arg(&fn_source, serialized, Some(true))
      .await
      .into_napi()?;
    Ok(result.to_json_like())
  }

  /// Raw isomorphic wire shape variant of [`Self::evaluateWithArg`].
  #[napi(ts_args_type = "fnSource: string, arg?: unknown")]
  pub async fn evaluate_with_arg_wire(
    &self,
    fn_source: String,
    arg: Option<serde_json::Value>,
  ) -> Result<serde_json::Value> {
    let serialized = crate::page::build_serialized_argument(arg);
    let result = self
      .inner
      .as_js_handle()
      .evaluate_with_arg(&fn_source, serialized, Some(true))
      .await
      .into_napi()?;
    serde_json::to_value(&result).map_err(|e| napi::Error::from_reason(e.to_string()))
  }

  /// Playwright: `elementHandle.evaluateHandle(pageFunction, arg?)`.
  #[napi(ts_args_type = "fnSource: string, arg?: unknown")]
  pub async fn evaluate_handle_with_arg(
    &self,
    fn_source: String,
    arg: Option<serde_json::Value>,
  ) -> Result<crate::js_handle::JSHandle> {
    let serialized = crate::page::build_serialized_argument(arg);
    let handle = self
      .inner
      .as_js_handle()
      .evaluate_handle_with_arg(&fn_source, serialized, Some(true))
      .await
      .into_napi()?;
    Ok(crate::js_handle::JSHandle::wrap(handle))
  }
}
