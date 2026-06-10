//! NAPI binding for [`ferridriver::web_error::WebError`].
//!
//! Mirrors Playwright's client-side `WebError` from
//! `/tmp/playwright/packages/playwright-core/src/client/webError.ts` and
//! the public type declaration in
//! `/tmp/playwright/packages/playwright-core/types/types.d.ts:21658` —
//! `page(): null|Page` plus `error(): Error` (the **native JS `Error`**,
//! not a plain object). See [`JsErrorValue`] for the native-Error
//! construction path.

use ferridriver::web_error::WebError as CoreWebError;
use napi::bindgen_prelude::{
  FromNapiValue, Function as NapiFunction, JsObjectValue as _, JsValue as _, Object as NapiObject, ToNapiValue,
};
use napi::{Env, JsString, sys};
use napi_derive::napi;

/// Live web-error handle — observed via
/// `context.waitForEvent('weberror')` / `context.on('weberror', cb)`
/// (context-scoped). Playwright's `page.on('pageerror')` /
/// `page.waitForEvent('pageerror')` deliver a native JS `Error`
/// directly, not a `WebError` — the class exists specifically for the
/// `weberror` surface.
#[napi]
pub struct WebError {
  pub(crate) inner: CoreWebError,
}

impl WebError {
  pub(crate) fn from_core(inner: CoreWebError) -> Self {
    Self { inner }
  }
}

#[napi]
impl WebError {
  /// Playwright: `webError.page(): null | Page`. Returns the page the
  /// error originated on, or `null` if the page has been dropped.
  #[napi(ts_return_type = "Page | null")]
  pub fn page(&self) -> Option<crate::page::Page> {
    self.inner.page().map(crate::page::Page::wrap)
  }

  /// Playwright: `webError.error(): Error`. Returns a **native JS
  /// `Error`** instance (not a plain object) so `instanceof Error`
  /// holds and downstream tooling recognises it as a thrown value.
  /// Captures `name` / `message` / `stack` from the original
  /// exception payload.
  #[napi(ts_return_type = "Error")]
  pub fn error(&self) -> JsErrorValue {
    let d = self.inner.error();
    JsErrorValue {
      name: d.name.clone(),
      message: d.message.clone(),
      stack: d.stack.clone(),
    }
  }
}

/// Rust-side wrapper for a JS `Error`-shaped payload. Implements
/// [`ToNapiValue`] so returning one from a `#[napi]` method constructs
/// a **real** JS `Error` instance via the global `Error` constructor
/// (matches Playwright's `webError.error(): Error` contract — the
/// returned value satisfies `instanceof Error === true`, carries an
/// engine-computed `stack` by default, and is recognised by node's
/// `util.inspect` / error-serialisation paths).
///
/// Direct construction from Rust without needing an `Env` handle;
/// conversion runs inside the JS thread via napi-rs's
/// `ToNapiValue::to_napi_value` callback, which is passed the raw
/// `napi_env` there.
pub struct JsErrorValue {
  pub name: String,
  pub message: String,
  pub stack: String,
}

impl ToNapiValue for JsErrorValue {
  unsafe fn to_napi_value(raw_env: sys::napi_env, val: Self) -> napi::Result<sys::napi_value> {
    // SAFETY: `raw_env` is a valid `napi_env` handle provided by the
    // napi-rs runtime; it lives for the duration of the JS call.
    let env = Env::from_raw(raw_env);
    let global = env.get_global()?;
    // Look up the global `Error` constructor. `Function<String, ()>`
    // mirrors the runtime signature (`new Error(message: string)`);
    // we immediately discard the return slot type because we then
    // cast the constructed value through the raw pointer path.
    let error_ctor = global.get_named_property_unchecked::<NapiFunction<'_, JsString<'_>, ()>>("Error")?;
    let message_js = env.create_string(&val.message)?;
    let constructed = error_ctor.new_instance(message_js)?;
    // `new_instance` returns `Unknown<'a>`; convert back to a raw
    // napi_value to rebuild as an `Object` we can mutate.
    let err_raw = constructed.raw();
    let mut err_object: NapiObject<'_> = unsafe { NapiObject::from_napi_value(raw_env, err_raw)? };
    // Override `name` (defaults to `'Error'`) and carry the original
    // `stack` when one was observed. Leaving `stack` unset when empty
    // keeps the engine's own auto-generated stack — Playwright mostly
    // cares about the `name`/`message` round-trip plus `stack`
    // presence, not stack equality.
    err_object.set_named_property("name", val.name)?;
    if !val.stack.is_empty() {
      err_object.set_named_property("stack", val.stack)?;
    }
    unsafe { ToNapiValue::to_napi_value(raw_env, err_object) }
  }
}

impl JsErrorValue {
  /// Build from a core [`ferridriver::web_error::ErrorDetails`].
  #[must_use]
  pub fn from_details(d: &ferridriver::web_error::ErrorDetails) -> Self {
    Self {
      name: d.name.clone(),
      message: d.message.clone(),
      stack: d.stack.clone(),
    }
  }
}

/// Cross-thread dispatch arg for `context.on('weberror', cb)` /
/// `context.once`. Carries a live core [`CoreWebError`] across the
/// tokio-to-napi boundary; on conversion, wraps it in the NAPI
/// [`WebError`] class so callbacks receive the Playwright-shaped
/// class instance (Playwright:
/// `/tmp/playwright/packages/playwright-core/types/types.d.ts:8365`
/// `on(event: 'weberror', listener: (webError: WebError) => any)`).
pub struct WebErrorArg(pub CoreWebError);

impl ToNapiValue for WebErrorArg {
  unsafe fn to_napi_value(env: sys::napi_env, val: Self) -> napi::Result<sys::napi_value> {
    let wrapper = WebError::from_core(val.0);
    unsafe { WebError::to_napi_value(env, wrapper) }
  }
}
