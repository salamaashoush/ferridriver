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

  /// Playwright: `jsHandle.asElement(): ElementHandle | null`
  /// (`/tmp/playwright/packages/playwright-core/src/client/jsHandle.ts:65`).
  /// Inspects the remote value; returns a fresh `ElementHandle`
  /// (sharing this handle's dispose flag) when the value is a DOM
  /// Node, otherwise `null`.
  #[napi]
  pub async fn as_element(&self) -> Result<Option<crate::element_handle::ElementHandle>> {
    let maybe = self.inner.as_element().await.into_napi()?;
    Ok(maybe.map(crate::element_handle::ElementHandle::wrap))
  }

  /// Playwright: `jsHandle.jsonValue(): Promise<T>`. Projects the
  /// remote value to its JSON-like form. Rich types that have no
  /// native JSON shape (`Date`, `RegExp`, `BigInt`, typed arrays,
  /// `NaN`/`Infinity`) surface as `null`; use
  /// [`Self::jsonValueWire`] for the full isomorphic wire shape.
  #[napi]
  pub async fn json_value(&self) -> Result<Option<serde_json::Value>> {
    let v = self.inner.json_value().await.into_napi()?;
    Ok(v.to_json_like())
  }

  /// Raw isomorphic wire shape of [`Self::jsonValue`] — keeps rich
  /// types (`Date`, `RegExp`, `BigInt`, typed arrays, `NaN`,
  /// `Infinity`, `undefined`) intact as their tagged wire form.
  #[napi]
  pub async fn json_value_wire(&self) -> Result<serde_json::Value> {
    let v = self.inner.json_value().await.into_napi()?;
    serde_json::to_value(&v).map_err(|e| napi::Error::from_reason(e.to_string()))
  }

  /// Playwright: `jsHandle.getProperty(propertyName): Promise<JSHandle>`.
  #[napi]
  pub async fn get_property(&self, name: String) -> Result<JSHandle> {
    let h = self.inner.get_property(&name).await.into_napi()?;
    Ok(JSHandle::wrap(h))
  }

  /// Playwright: `jsHandle.getProperties(): Promise<Map<string, JSHandle>>`.
  /// NAPI returns a plain object `{ [key]: JSHandle }` — keeps the
  /// shape ergonomic on the JS side while preserving the per-key
  /// handle identity.
  #[napi(ts_return_type = "Record<string, JSHandle>")]
  pub async fn get_properties(&self) -> Result<std::collections::HashMap<String, JSHandle>> {
    let pairs = self.inner.get_properties().await.into_napi()?;
    Ok(pairs.into_iter().map(|(k, h)| (k, JSHandle::wrap(h))).collect())
  }

  /// Playwright: `jsHandle.evaluate(pageFunction, arg?)`. Runs
  /// `fnSource` with `this` bound to this handle's remote object.
  /// Rich-type return values that have no native JSON form surface
  /// as `null`; use [`Self::evaluateWithArgWire`] for the raw
  /// isomorphic wire shape.
  #[napi(ts_args_type = "fnSource: string, arg?: unknown")]
  pub async fn evaluate_with_arg(
    &self,
    fn_source: String,
    arg: Option<crate::types::NapiEvaluateArg>,
  ) -> Result<Option<serde_json::Value>> {
    let serialized = crate::page::build_serialized_argument(arg);
    let result = self
      .inner
      .evaluate_with_arg(&fn_source, serialized, Some(true))
      .await
      .into_napi()?;
    Ok(result.to_json_like())
  }

  /// Phase-D escape hatch: raw isomorphic wire shape.
  #[napi(ts_args_type = "fnSource: string, arg?: unknown")]
  pub async fn evaluate_with_arg_wire(
    &self,
    fn_source: String,
    arg: Option<crate::types::NapiEvaluateArg>,
  ) -> Result<serde_json::Value> {
    let serialized = crate::page::build_serialized_argument(arg);
    let result = self
      .inner
      .evaluate_with_arg(&fn_source, serialized, Some(true))
      .await
      .into_napi()?;
    serde_json::to_value(&result).map_err(|e| napi::Error::from_reason(e.to_string()))
  }

  /// Playwright: `jsHandle.evaluateHandle(pageFunction, arg?)`.
  #[napi(ts_args_type = "fnSource: string, arg?: unknown")]
  pub async fn evaluate_handle_with_arg(
    &self,
    fn_source: String,
    arg: Option<crate::types::NapiEvaluateArg>,
  ) -> Result<JSHandle> {
    let serialized = crate::page::build_serialized_argument(arg);
    let handle = self
      .inner
      .evaluate_handle_with_arg(&fn_source, serialized, Some(true))
      .await
      .into_napi()?;
    Ok(JSHandle::wrap(handle))
  }
}
