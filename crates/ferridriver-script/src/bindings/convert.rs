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

/// Lower an `addInitScript`-style JS argument into
/// [`ferridriver::options::InitScriptSource`] plus an optional JSON arg.
/// Mirrors Playwright's
/// `Function | string | { path?: string, content?: string }` union from
/// `/tmp/playwright/packages/playwright-core/src/client/page.ts:520` — all
/// semantic lowering (function body via `.toString()`, path/content
/// precedence, `null`-vs-`undefined` preservation for `arg`) happens here
/// synchronously so the async binding method can immediately hand owned,
/// `Send`-safe values to Rust core.
///
/// Returns an error for non-matching `script` shapes or for a missing
/// `{ path, content }` entry. The (source|path|content) + arg rejection is
/// left to [`ferridriver::options::evaluation_script`] so both binding
/// layers share the exact error text Playwright ships.
pub fn init_script_from_js<'js>(
  ctx: &Ctx<'js>,
  script: Value<'js>,
  arg: Option<Value<'js>>,
) -> rquickjs::Result<(ferridriver::options::InitScriptSource, Option<serde_json::Value>)> {
  let arg_json = match arg {
    None => None,
    Some(v) if v.is_undefined() => None,
    Some(v) if v.is_null() => Some(serde_json::Value::Null),
    Some(v) => Some(serde_from_js::<serde_json::Value>(ctx, v)?),
  };

  let init = if script.is_function() {
    // `String(fn)` invokes `Function.prototype.toString` — the same
    // primitive Playwright's client uses via `fun.toString()`.
    let string_global: Function<'js> = ctx.globals().get("String")?;
    let body: String = string_global.call((script,))?;
    ferridriver::options::InitScriptSource::Function { body }
  } else if script.is_string() {
    let s: String = script.get()?;
    ferridriver::options::InitScriptSource::Source(s)
  } else if script.is_object() {
    let obj = script
      .as_object()
      .ok_or_else(|| rquickjs::Error::new_from_js_message("ferridriver", "addInitScript", "expected object"))?;
    if let Ok(content) = obj.get::<_, String>("content") {
      ferridriver::options::InitScriptSource::Content(content)
    } else if let Ok(path) = obj.get::<_, String>("path") {
      ferridriver::options::InitScriptSource::Path(path.into())
    } else {
      return Err(rquickjs::Error::new_from_js_message(
        "ferridriver",
        "addInitScript",
        "Either path or content property must be present",
      ));
    }
  } else {
    return Err(rquickjs::Error::new_from_js_message(
      "ferridriver",
      "addInitScript",
      "script must be Function | string | { path?, content? }",
    ));
  };

  Ok((init, arg_json))
}
