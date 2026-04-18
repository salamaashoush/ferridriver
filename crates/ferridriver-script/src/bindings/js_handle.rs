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

  /// Playwright: `jsHandle.asElement(): ElementHandle | null`
  /// (`/tmp/playwright/packages/playwright-core/src/client/jsHandle.ts:65`).
  /// Inspects the remote value and returns a fresh `ElementHandle`
  /// (sharing this handle's dispose flag) when the value is a DOM
  /// Node, otherwise `null`.
  #[qjs(rename = "asElement")]
  pub async fn as_element(&self) -> rquickjs::Result<Option<crate::bindings::element_handle::ElementHandleJs>> {
    let maybe = self.inner.as_element().await.into_js()?;
    Ok(maybe.map(crate::bindings::element_handle::ElementHandleJs::new))
  }

  /// Playwright: `jsHandle.jsonValue(): Promise<T>`. Projects the
  /// remote value to its JSON-like form. Rich types without a native
  /// JSON shape surface as `null`; use [`Self::jsonValueWire`] for
  /// the full isomorphic wire shape.
  #[qjs(rename = "jsonValue")]
  pub async fn json_value<'js>(&self, ctx: rquickjs::Ctx<'js>) -> rquickjs::Result<rquickjs::Value<'js>> {
    let v = self.inner.json_value().await.into_js()?;
    serialized_value_to_quickjs(&ctx, &v)
  }

  /// Raw isomorphic wire shape variant of [`Self::jsonValue`] — keeps
  /// rich types (`Date`, `RegExp`, `BigInt`, typed arrays, `NaN`,
  /// `Infinity`, `undefined`) intact as their tagged wire form.
  #[qjs(rename = "jsonValueWire")]
  pub async fn json_value_wire<'js>(&self, ctx: rquickjs::Ctx<'js>) -> rquickjs::Result<rquickjs::Value<'js>> {
    let v = self.inner.json_value().await.into_js()?;
    let wire = serde_json::to_value(&v)
      .map_err(|e| rquickjs::Error::new_from_js_message("jsonValueWire", "", &e.to_string()))?;
    json_value_to_quickjs(&ctx, &wire)
  }

  /// Playwright: `jsHandle.getProperty(propertyName): Promise<JSHandle>`.
  #[qjs(rename = "getProperty")]
  pub async fn get_property(&self, name: String) -> rquickjs::Result<JSHandleJs> {
    let h = self.inner.get_property(&name).await.into_js()?;
    Ok(JSHandleJs::new(h))
  }

  /// Playwright: `jsHandle.getProperties(): Promise<Map<string, JSHandle>>`.
  /// The QuickJS surface returns a plain object `{ [key]: JSHandle }`
  /// mirroring the NAPI `Record<string, JSHandle>` shape — ergonomic
  /// on the JS side without losing per-key handle identity.
  #[qjs(rename = "getProperties")]
  pub async fn get_properties<'js>(&self, ctx: rquickjs::Ctx<'js>) -> rquickjs::Result<rquickjs::Value<'js>> {
    let pairs = self.inner.get_properties().await.into_js()?;
    let obj = rquickjs::Object::new(ctx.clone())?;
    for (k, h) in pairs {
      let handle_js = rquickjs::Class::instance(ctx.clone(), JSHandleJs::new(h))?;
      obj.set(k, handle_js)?;
    }
    Ok(obj.into_value())
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
