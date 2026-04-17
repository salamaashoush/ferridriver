//! Conversion helpers between ferridriver core types and `rquickjs` values.

use ferridriver::FerriError;
use rquickjs::{Ctx, Function, Object, Value};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Convert a [`FerriError`] into an `rquickjs::Error` suitable for throwing
/// out of a binding method.
///
/// The JS-visible `message` is the `Display` output of the core error, which
/// already matches Playwright's phrasing for the variants that have a
/// Playwright analogue (see `ferridriver::error`). The `from` / `to` labels
/// are static strings used by `rquickjs` for its own error rendering.
pub fn to_rq_error(err: &FerriError) -> rquickjs::Error {
  rquickjs::Error::new_from_js_message("ferridriver", err.name(), err.to_string())
}

/// Adapter: `Result<T, FerriError>` into `rquickjs::Result<T>`.
pub trait FerriResultExt<T> {
  fn into_js(self) -> rquickjs::Result<T>;
}

impl<T> FerriResultExt<T> for Result<T, FerriError> {
  fn into_js(self) -> rquickjs::Result<T> {
    self.map_err(|e| to_rq_error(&e))
  }
}

/// Convert any `serde::Serialize` value into a JS value by round-tripping
/// through `JSON.parse(JSON.stringify(...))`. Used for binding methods that
/// return complex Rust structures (cookies, storage state, JSON response
/// bodies) without writing per-type FFI.
pub fn serde_to_js<'js, T: Serialize>(ctx: &Ctx<'js>, value: &T) -> rquickjs::Result<Value<'js>> {
  let json = serde_json::to_string(value)
    .map_err(|e| rquickjs::Error::new_from_js_message("serde", "serialize", e.to_string()))?;
  let json_global: Object<'js> = ctx.globals().get("JSON")?;
  let parse: Function<'js> = json_global.get("parse")?;
  parse.call((json,))
}

/// Inverse of [`serde_to_js`] — accept a JS value and deserialize into a
/// Rust type via `JSON.stringify` → `serde_json::from_str`.
pub fn serde_from_js<'js, T: DeserializeOwned>(ctx: &Ctx<'js>, value: Value<'js>) -> rquickjs::Result<T> {
  let json_global: Object<'js> = ctx.globals().get("JSON")?;
  let stringify: Function<'js> = json_global.get("stringify")?;
  let json: String = stringify.call((value,))?;
  serde_json::from_str(&json).map_err(|e| rquickjs::Error::new_from_js_message("serde", "deserialize", e.to_string()))
}
