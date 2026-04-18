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

/// Shape of a JS `{ x, y }` point passed as `position` in click-family
/// options. Deserialised out of the raw `ClickOptions` JS object.
#[derive(serde::Deserialize, Debug, Default, Clone, Copy)]
struct JsClickPosition {
  x: f64,
  y: f64,
}

impl From<JsClickPosition> for ferridriver::options::Point {
  fn from(p: JsClickPosition) -> Self {
    Self { x: p.x, y: p.y }
  }
}

/// Raw JS shape of Playwright's `ClickOptions` — deserialised via
/// `serde_from_js` and then lowered to
/// [`ferridriver::options::ClickOptions`] by [`parse_click_options`].
/// Strings (`button`, `modifiers`) are validated at lowering time so
/// typos surface as `FerriError::InvalidArgument` rather than silent
/// defaults.
#[derive(serde::Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
struct JsClickOptions {
  button: Option<String>,
  click_count: Option<u32>,
  delay: Option<u64>,
  force: Option<bool>,
  modifiers: Option<Vec<String>>,
  no_wait_after: Option<bool>,
  position: Option<JsClickPosition>,
  steps: Option<u32>,
  timeout: Option<u64>,
  trial: Option<bool>,
}

/// Raw JS shape of Playwright's `DispatchEventOptions` — single field.
#[derive(serde::Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
struct JsDispatchEventOptions {
  timeout: Option<u64>,
}

/// Parse Playwright's `DispatchEventOptions` JS bag into the core struct.
pub fn parse_dispatch_event_options<'js>(
  ctx: &Ctx<'js>,
  value: rquickjs::function::Opt<Value<'js>>,
) -> rquickjs::Result<Option<ferridriver::options::DispatchEventOptions>> {
  let raw = match value.0 {
    Some(v) if !v.is_undefined() && !v.is_null() => v,
    _ => return Ok(None),
  };
  let js: JsDispatchEventOptions = serde_from_js(ctx, raw)?;
  Ok(Some(ferridriver::options::DispatchEventOptions { timeout: js.timeout }))
}

/// Raw JS shape of Playwright's `FilePayload`.
#[derive(serde::Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct JsFilePayload {
  name: String,
  mime_type: String,
  /// JS `Buffer`/`Uint8Array`/array-of-numbers all deserialize to a
  /// `Vec<u8>` via serde_json::from_js. rquickjs `Buffer` types round
  /// through JSON as arrays of small numbers, which serde handles.
  buffer: Vec<u8>,
}

impl From<JsFilePayload> for ferridriver::options::FilePayload {
  fn from(p: JsFilePayload) -> Self {
    Self {
      name: p.name,
      mime_type: p.mime_type,
      buffer: p.buffer,
    }
  }
}

/// Raw JS shape of Playwright's `SetInputFilesOptions` — two fields.
#[derive(serde::Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
struct JsSetInputFilesOptions {
  no_wait_after: Option<bool>,
  timeout: Option<u64>,
}

/// Parse the polymorphic `files` arg for `setInputFiles`:
/// `string | string[] | FilePayload | FilePayload[]`.
pub fn parse_input_files<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> rquickjs::Result<ferridriver::options::InputFiles> {
  if value.is_string() {
    let s: String = value.get()?;
    return Ok(ferridriver::options::InputFiles::Paths(vec![s.into()]));
  }
  if value.is_array() {
    let arr: Vec<serde_json::Value> = serde_from_js(ctx, value)?;
    if arr.is_empty() {
      return Ok(ferridriver::options::InputFiles::Paths(Vec::new()));
    }
    if arr[0].is_string() {
      let mut paths = Vec::with_capacity(arr.len());
      for el in arr {
        let s = el.as_str().ok_or_else(|| {
          rquickjs::Error::new_from_js_message("ferridriver", "setInputFiles", "array elements must be strings")
        })?;
        paths.push(std::path::PathBuf::from(s));
      }
      return Ok(ferridriver::options::InputFiles::Paths(paths));
    }
    let mut payloads = Vec::with_capacity(arr.len());
    for el in arr {
      let p: JsFilePayload = serde_json::from_value(el).map_err(|e| {
        rquickjs::Error::new_from_js_message("ferridriver", "setInputFiles", format!("FilePayload parse: {e}"))
      })?;
      payloads.push(p.into());
    }
    return Ok(ferridriver::options::InputFiles::Payloads(payloads));
  }
  if value.is_object() {
    let p: JsFilePayload = serde_from_js(ctx, value)?;
    return Ok(ferridriver::options::InputFiles::Payloads(vec![p.into()]));
  }
  Err(rquickjs::Error::new_from_js_message(
    "ferridriver",
    "setInputFiles",
    "files must be string | string[] | FilePayload | FilePayload[]",
  ))
}

/// Parse Playwright's `SetInputFilesOptions` JS bag into the core struct.
pub fn parse_set_input_files_options<'js>(
  ctx: &Ctx<'js>,
  value: rquickjs::function::Opt<Value<'js>>,
) -> rquickjs::Result<Option<ferridriver::options::SetInputFilesOptions>> {
  let raw = match value.0 {
    Some(v) if !v.is_undefined() && !v.is_null() => v,
    _ => return Ok(None),
  };
  let js: JsSetInputFilesOptions = serde_from_js(ctx, raw)?;
  Ok(Some(ferridriver::options::SetInputFilesOptions {
    no_wait_after: js.no_wait_after,
    timeout: js.timeout,
  }))
}

/// Raw JS shape of a `selectOption` descriptor — mirrors Playwright's
/// `{ value?, label?, index? }`.
#[derive(serde::Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
struct JsSelectOptionValue {
  value: Option<String>,
  label: Option<String>,
  index: Option<u32>,
}

impl From<JsSelectOptionValue> for ferridriver::options::SelectOptionValue {
  fn from(v: JsSelectOptionValue) -> Self {
    Self {
      value: v.value,
      label: v.label,
      index: v.index,
    }
  }
}

/// Raw JS shape of Playwright's `SelectOptionOptions` — three fields.
#[derive(serde::Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
struct JsSelectOptionOptions {
  force: Option<bool>,
  no_wait_after: Option<bool>,
  timeout: Option<u64>,
}

/// Parse a polymorphic `selectOption` `values` JS argument:
/// `string | string[] | { value?, label?, index? } | Array<...>`.
pub fn parse_select_option_values<'js>(
  ctx: &Ctx<'js>,
  value: Value<'js>,
) -> rquickjs::Result<Vec<ferridriver::options::SelectOptionValue>> {
  if value.is_string() {
    let s: String = value.get()?;
    return Ok(vec![ferridriver::options::SelectOptionValue::by_value(s)]);
  }
  if value.is_array() {
    let arr: Vec<serde_json::Value> = serde_from_js(ctx, value)?;
    let mut out = Vec::new();
    for el in arr {
      match el {
        serde_json::Value::String(s) => out.push(ferridriver::options::SelectOptionValue::by_value(s)),
        serde_json::Value::Object(_) => {
          let desc: JsSelectOptionValue = serde_json::from_value(el).map_err(|e| {
            rquickjs::Error::new_from_js_message("ferridriver", "selectOption", format!("descriptor parse: {e}"))
          })?;
          out.push(desc.into());
        },
        _ => {
          return Err(rquickjs::Error::new_from_js_message(
            "ferridriver",
            "selectOption",
            "array entries must be string or { value?, label?, index? } object",
          ));
        },
      }
    }
    return Ok(out);
  }
  if value.is_object() {
    let desc: JsSelectOptionValue = serde_from_js(ctx, value)?;
    return Ok(vec![desc.into()]);
  }
  Err(rquickjs::Error::new_from_js_message(
    "ferridriver",
    "selectOption",
    "values must be string | string[] | object | object[]",
  ))
}

/// Parse Playwright's `SelectOptionOptions` JS bag into the core struct.
pub fn parse_select_option_options<'js>(
  ctx: &Ctx<'js>,
  value: rquickjs::function::Opt<Value<'js>>,
) -> rquickjs::Result<Option<ferridriver::options::SelectOptionOptions>> {
  let raw = match value.0 {
    Some(v) if !v.is_undefined() && !v.is_null() => v,
    _ => return Ok(None),
  };
  let js: JsSelectOptionOptions = serde_from_js(ctx, raw)?;
  Ok(Some(ferridriver::options::SelectOptionOptions {
    force: js.force,
    no_wait_after: js.no_wait_after,
    timeout: js.timeout,
  }))
}

/// Raw JS shape of Playwright's `FillOptions` — three fields.
#[derive(serde::Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
struct JsFillOptions {
  force: Option<bool>,
  no_wait_after: Option<bool>,
  timeout: Option<u64>,
}

/// Parse Playwright's `FillOptions` JS bag into the core struct.
pub fn parse_fill_options<'js>(
  ctx: &Ctx<'js>,
  value: rquickjs::function::Opt<Value<'js>>,
) -> rquickjs::Result<Option<ferridriver::options::FillOptions>> {
  let raw = match value.0 {
    Some(v) if !v.is_undefined() && !v.is_null() => v,
    _ => return Ok(None),
  };
  let js: JsFillOptions = serde_from_js(ctx, raw)?;
  Ok(Some(ferridriver::options::FillOptions {
    force: js.force,
    no_wait_after: js.no_wait_after,
    timeout: js.timeout,
  }))
}

/// Raw JS shape of Playwright's `PressOptions` / `TypeOptions` — same shape.
#[derive(serde::Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
struct JsPressOptions {
  delay: Option<u64>,
  no_wait_after: Option<bool>,
  timeout: Option<u64>,
}

/// Parse Playwright's `PressOptions` JS bag into the core struct.
pub fn parse_press_options<'js>(
  ctx: &Ctx<'js>,
  value: rquickjs::function::Opt<Value<'js>>,
) -> rquickjs::Result<Option<ferridriver::options::PressOptions>> {
  let raw = match value.0 {
    Some(v) if !v.is_undefined() && !v.is_null() => v,
    _ => return Ok(None),
  };
  let js: JsPressOptions = serde_from_js(ctx, raw)?;
  Ok(Some(ferridriver::options::PressOptions {
    delay: js.delay,
    no_wait_after: js.no_wait_after,
    timeout: js.timeout,
  }))
}

/// Parse Playwright's `TypeOptions` JS bag — same shape as `PressOptions`.
pub fn parse_type_options<'js>(
  ctx: &Ctx<'js>,
  value: rquickjs::function::Opt<Value<'js>>,
) -> rquickjs::Result<Option<ferridriver::options::TypeOptions>> {
  let raw = match value.0 {
    Some(v) if !v.is_undefined() && !v.is_null() => v,
    _ => return Ok(None),
  };
  let js: JsPressOptions = serde_from_js(ctx, raw)?;
  Ok(Some(ferridriver::options::TypeOptions {
    delay: js.delay,
    no_wait_after: js.no_wait_after,
    timeout: js.timeout,
  }))
}

/// Raw JS shape of Playwright's `CheckOptions` / `UncheckOptions` /
/// `SetCheckedOptions` — five fields (force, noWaitAfter, position,
/// timeout, trial).
#[derive(serde::Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
struct JsCheckOptions {
  force: Option<bool>,
  no_wait_after: Option<bool>,
  position: Option<JsClickPosition>,
  timeout: Option<u64>,
  trial: Option<bool>,
}

/// Parse Playwright's `CheckOptions` / `UncheckOptions` /
/// `SetCheckedOptions` JS bag into the core struct.
pub fn parse_check_options<'js>(
  ctx: &Ctx<'js>,
  value: rquickjs::function::Opt<Value<'js>>,
) -> rquickjs::Result<Option<ferridriver::options::CheckOptions>> {
  let raw = match value.0 {
    Some(v) if !v.is_undefined() && !v.is_null() => v,
    _ => return Ok(None),
  };
  let js: JsCheckOptions = serde_from_js(ctx, raw)?;
  Ok(Some(ferridriver::options::CheckOptions {
    force: js.force,
    no_wait_after: js.no_wait_after,
    position: js.position.map(Into::into),
    timeout: js.timeout,
    trial: js.trial,
  }))
}

/// Raw JS shape of Playwright's `HoverOptions` — `ClickOptions` minus
/// `button`, `click_count`, `delay`. Also used for `TapOptions` (same
/// shape per Playwright).
#[derive(serde::Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
struct JsHoverOptions {
  force: Option<bool>,
  modifiers: Option<Vec<String>>,
  no_wait_after: Option<bool>,
  position: Option<JsClickPosition>,
  steps: Option<u32>,
  timeout: Option<u64>,
  trial: Option<bool>,
}

fn hover_options_from_raw<'js>(
  ctx: &Ctx<'js>,
  value: rquickjs::function::Opt<Value<'js>>,
  label: &'static str,
) -> rquickjs::Result<Option<ferridriver::options::HoverOptions>> {
  let raw = match value.0 {
    Some(v) if !v.is_undefined() && !v.is_null() => v,
    _ => return Ok(None),
  };
  let js: JsHoverOptions = serde_from_js(ctx, raw)?;
  let mut modifiers = Vec::new();
  if let Some(list) = js.modifiers {
    for name in list {
      let m = ferridriver::options::Modifier::parse(&name).ok_or_else(|| {
        rquickjs::Error::new_from_js_message("ferridriver", label, format!("Unknown modifier: {name}"))
      })?;
      modifiers.push(m);
    }
  }
  Ok(Some(ferridriver::options::HoverOptions {
    force: js.force,
    modifiers,
    no_wait_after: js.no_wait_after,
    position: js.position.map(Into::into),
    steps: js.steps,
    timeout: js.timeout,
    trial: js.trial,
  }))
}

/// Parse Playwright's `HoverOptions` JS bag into the core struct.
pub fn parse_hover_options<'js>(
  ctx: &Ctx<'js>,
  value: rquickjs::function::Opt<Value<'js>>,
) -> rquickjs::Result<Option<ferridriver::options::HoverOptions>> {
  hover_options_from_raw(ctx, value, "hover")
}

/// Parse Playwright's `TapOptions` JS bag — same shape as `HoverOptions`.
pub fn parse_tap_options<'js>(
  ctx: &Ctx<'js>,
  value: rquickjs::function::Opt<Value<'js>>,
) -> rquickjs::Result<Option<ferridriver::options::TapOptions>> {
  hover_options_from_raw(ctx, value, "tap")
}

/// Raw JS shape of Playwright's `DblClickOptions` — same fields as
/// `ClickOptions` minus `click_count`. See
/// `/tmp/playwright/packages/playwright-core/types/types.d.ts:13116`.
#[derive(serde::Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
struct JsDblClickOptions {
  button: Option<String>,
  delay: Option<u64>,
  force: Option<bool>,
  modifiers: Option<Vec<String>>,
  no_wait_after: Option<bool>,
  position: Option<JsClickPosition>,
  steps: Option<u32>,
  timeout: Option<u64>,
  trial: Option<bool>,
}

/// Parse Playwright's `DblClickOptions` JS bag into the core struct.
pub fn parse_dblclick_options<'js>(
  ctx: &Ctx<'js>,
  value: rquickjs::function::Opt<Value<'js>>,
) -> rquickjs::Result<Option<ferridriver::options::DblClickOptions>> {
  let raw = match value.0 {
    Some(v) if !v.is_undefined() && !v.is_null() => v,
    _ => return Ok(None),
  };
  let js: JsDblClickOptions = serde_from_js(ctx, raw)?;
  let button = match js.button.as_deref() {
    None => None,
    Some(s) => Some(ferridriver::options::MouseButton::parse(s).ok_or_else(|| {
      rquickjs::Error::new_from_js_message("ferridriver", "dblclick", format!("Unknown mouse button: {s}"))
    })?),
  };
  let mut modifiers = Vec::new();
  if let Some(list) = js.modifiers {
    for name in list {
      let m = ferridriver::options::Modifier::parse(&name).ok_or_else(|| {
        rquickjs::Error::new_from_js_message("ferridriver", "dblclick", format!("Unknown modifier: {name}"))
      })?;
      modifiers.push(m);
    }
  }
  Ok(Some(ferridriver::options::DblClickOptions {
    button,
    delay: js.delay,
    force: js.force,
    modifiers,
    no_wait_after: js.no_wait_after,
    position: js.position.map(Into::into),
    steps: js.steps,
    timeout: js.timeout,
    trial: js.trial,
  }))
}

/// Parse Playwright's `ClickOptions` JS bag into the core struct.
/// Accepts `Opt<Value>` so callers pass `Opt(options)` verbatim; `None`,
/// `undefined`, or `null` → `Ok(None)`. Unknown `button` / `modifier`
/// strings raise a typed `rquickjs::Error` with the exact Playwright
/// message so JS-side assertions see `/Unknown (button|modifier)/` for
/// drift detection.
pub fn parse_click_options<'js>(
  ctx: &Ctx<'js>,
  value: rquickjs::function::Opt<Value<'js>>,
) -> rquickjs::Result<Option<ferridriver::options::ClickOptions>> {
  let raw = match value.0 {
    Some(v) if !v.is_undefined() && !v.is_null() => v,
    _ => return Ok(None),
  };
  let js: JsClickOptions = serde_from_js(ctx, raw)?;
  let button = match js.button.as_deref() {
    None => None,
    Some(s) => Some(ferridriver::options::MouseButton::parse(s).ok_or_else(|| {
      rquickjs::Error::new_from_js_message("ferridriver", "click", format!("Unknown mouse button: {s}"))
    })?),
  };
  let mut modifiers = Vec::new();
  if let Some(list) = js.modifiers {
    for name in list {
      let m = ferridriver::options::Modifier::parse(&name).ok_or_else(|| {
        rquickjs::Error::new_from_js_message("ferridriver", "click", format!("Unknown modifier: {name}"))
      })?;
      modifiers.push(m);
    }
  }
  Ok(Some(ferridriver::options::ClickOptions {
    button,
    click_count: js.click_count,
    delay: js.delay,
    force: js.force,
    modifiers,
    no_wait_after: js.no_wait_after,
    position: js.position.map(Into::into),
    steps: js.steps,
    timeout: js.timeout,
    trial: js.trial,
  }))
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
