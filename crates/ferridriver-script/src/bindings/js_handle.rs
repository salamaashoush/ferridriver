//! `JSHandleJs`: QuickJS wrapper around `ferridriver::JSHandle`.
//!
//! Mirrors the NAPI surface in `crates/ferridriver-node/src/js_handle.rs`
//! and Playwright's `JSHandle` TS interface. Phase-C surface covers lifecycle
//! only — `dispose`, `isDisposed`, `asElement`. Phase D extends with
//! `evaluate`, `evaluateHandle`, `getProperties`, `getProperty`, `jsonValue`.

use ferridriver::JSHandle;
use rquickjs::JsLifetime;
use rquickjs::class::Trace;

use crate::bindings::convert::{
  FerriResultExt, json_value_to_quickjs, quickjs_arg_to_serialized, serialized_value_to_quickjs,
};

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

  /// Playwright: `jsHandle.evaluate(fn, arg?)`. The handle's remote
  /// object is passed as the first argument to `fn`. See
  /// `ferridriver::JSHandle::evaluate_with_arg` for the phase-D MVP
  /// semantic.
  #[qjs(rename = "evaluateWithArg")]
  pub async fn evaluate_with_arg<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    fn_source: String,
    arg: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    let serialized = quickjs_arg_to_serialized(&ctx, arg.0)?;
    let result = self
      .inner
      .evaluate_with_arg(&fn_source, serialized, Some(true))
      .await
      .into_js()?;
    serialized_value_to_quickjs(&ctx, &result)
  }

  /// Raw isomorphic wire shape variant of [`Self::evaluateWithArg`].
  #[qjs(rename = "evaluateWithArgWire")]
  pub async fn evaluate_with_arg_wire<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    fn_source: String,
    arg: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    let serialized = quickjs_arg_to_serialized(&ctx, arg.0)?;
    let result = self
      .inner
      .evaluate_with_arg(&fn_source, serialized, Some(true))
      .await
      .into_js()?;
    let wire = serde_json::to_value(&result)
      .map_err(|e| rquickjs::Error::new_from_js_message("evaluateWithArgWire", "", &e.to_string()))?;
    json_value_to_quickjs(&ctx, &wire)
  }

  /// Playwright: `jsHandle.evaluateHandle(fn, arg?)`.
  #[qjs(rename = "evaluateHandleWithArg")]
  pub async fn evaluate_handle_with_arg<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    fn_source: String,
    arg: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<JSHandleJs> {
    let serialized = quickjs_arg_to_serialized(&ctx, arg.0)?;
    let handle = self
      .inner
      .evaluate_handle_with_arg(&fn_source, serialized, Some(true))
      .await
      .into_js()?;
    Ok(JSHandleJs::new(handle))
  }
}
