//! `QuickJS` option-bag and payload parsers for locator, page, and element actions.

use rquickjs::{Ctx, Function, Value};

use super::{parse_opt_bag, serde_from_js, type_error};

/// Parse Playwright's `DispatchEventOptions` JS bag into the core struct.
///
/// # Errors
///
/// As [`parse_opt_bag`].
pub fn parse_dispatch_event_options<'js>(
  ctx: &Ctx<'js>,
  value: rquickjs::function::Opt<Value<'js>>,
) -> rquickjs::Result<Option<ferridriver::options::DispatchEventOptions>> {
  parse_opt_bag(ctx, value)
}

/// Refuse a file the realm's `read` grant does not cover. These bindings
/// are a trusted extension of the realm, so every one that opens a file a
/// script named makes the check the standard library's `fs` makes.
pub(crate) fn readable(ctx: &Ctx<'_>, path: &std::path::Path) -> rquickjs::Result<()> {
  ferrijs_std::permissions::check_read(ctx, path)
}

/// [`readable`] for a file or directory a binding writes to.
pub(crate) fn writable(ctx: &Ctx<'_>, path: &std::path::Path) -> rquickjs::Result<()> {
  ferrijs_std::permissions::check_write(ctx, path)
}

/// The files and directories a context's options read and write.
pub(crate) fn check_context_paths(
  ctx: &Ctx<'_>,
  options: &ferridriver::options::BrowserContextOptions,
) -> rquickjs::Result<()> {
  if let Some(video) = &options.record_video {
    writable(ctx, &video.dir)?;
  }
  if let Some(har) = &options.record_har {
    writable(ctx, &har.path)?;
  }
  if let Some(ferridriver::options::StorageStateInput::Path(path)) = &options.storage_state {
    readable(ctx, path)?;
  }
  Ok(())
}

/// Parse the polymorphic `files` arg for `setInputFiles`:
/// `string | string[] | FilePayload | FilePayload[]`.
///
/// # Errors
///
/// A `TypeError` for a value of none of those shapes or a malformed `FilePayload`, or a
/// `PermissionDeniedError` for a path outside the realm's read grant.
pub fn parse_input_files<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> rquickjs::Result<ferridriver::options::InputFiles> {
  if value.is_string() {
    let s: String = value.get()?;
    readable(ctx, std::path::Path::new(&s))?;
    return Ok(ferridriver::options::InputFiles::Paths(vec![s.into()]));
  }
  if let Some(arr) = value.as_array() {
    let len = arr.len();
    if len == 0 {
      return Ok(ferridriver::options::InputFiles::Paths(Vec::new()));
    }
    // Probe the first element directly on the JS value (no
    // serde_json::Value middle-hop): all-strings -> paths, else
    // FilePayload objects.
    let first: Value<'js> = arr.get(0)?;
    if first.is_string() {
      let mut paths = Vec::with_capacity(len);
      for idx in 0..len {
        let el: Value<'js> = arr.get(idx)?;
        let s: String = el.into_string().map_or_else(
          || Err(type_error(ctx, "setInputFiles: array elements must be strings")),
          |s| s.to_string(),
        )?;
        readable(ctx, std::path::Path::new(&s))?;
        paths.push(std::path::PathBuf::from(s));
      }
      return Ok(ferridriver::options::InputFiles::Paths(paths));
    }
    let mut payloads = Vec::with_capacity(len);
    for idx in 0..len {
      let el: Value<'js> = arr.get(idx)?;
      payloads.push(parse_file_payload(ctx, el)?);
    }
    return Ok(ferridriver::options::InputFiles::Payloads(payloads));
  }
  if value.is_object() {
    return Ok(ferridriver::options::InputFiles::Payloads(vec![parse_file_payload(
      ctx, value,
    )?]));
  }
  Err(type_error(
    ctx,
    "setInputFiles: files must be string | string[] | FilePayload | FilePayload[]",
  ))
}

/// Parse one `FilePayload`. Playwright's `buffer` is a Node `Buffer`,
/// which here is [`ferrijs::std::node::buffer::BufferJs`]
/// class — and it is neither a `Uint8Array` subclass nor serde-visible
/// as a sequence, so `Buffer.from(...)` has to go through the shared
/// byte extractor rather than serde.
fn parse_file_payload<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> rquickjs::Result<ferridriver::options::FilePayload> {
  if let Some(obj) = value.as_object() {
    let buffer: Value<'js> = obj.get("buffer")?;
    if !buffer.is_undefined() && !buffer.is_null() {
      return Ok(ferridriver::options::FilePayload {
        name: obj.get("name")?,
        mime_type: obj.get("mimeType")?,
        buffer: ferrijs::std::node::bytes::value_to_bytes(ctx, &buffer, None)?,
      });
    }
  }
  serde_from_js(ctx, value)
}

/// Parse Playwright's `SetInputFilesOptions` JS bag into the core struct.
///
/// # Errors
///
/// As [`parse_opt_bag`].
pub fn parse_set_input_files_options<'js>(
  ctx: &Ctx<'js>,
  value: rquickjs::function::Opt<Value<'js>>,
) -> rquickjs::Result<Option<ferridriver::options::SetInputFilesOptions>> {
  parse_opt_bag(ctx, value)
}

/// Parse Playwright's `DropOptions` JS bag into the core struct.
///
/// # Errors
///
/// As [`parse_opt_bag`].
pub fn parse_drop_options<'js>(
  ctx: &Ctx<'js>,
  value: rquickjs::function::Opt<Value<'js>>,
) -> rquickjs::Result<Option<ferridriver::options::DropOptions>> {
  parse_opt_bag(ctx, value)
}

/// Parse Playwright's native `DropPayload`
/// (`{ files?: string | string[] | FilePayload | FilePayload[], data?: { [mimeType: string]: string } }`)
/// into the core struct. Both fields are optional.
///
/// # Errors
///
/// A `TypeError` for a payload that is not an object or a malformed `files` or `data`, or a
/// `PermissionDeniedError` for a file path outside the realm's read grant.
pub fn parse_drop_payload<'js>(
  ctx: &Ctx<'js>,
  value: Value<'js>,
) -> rquickjs::Result<ferridriver::options::DropPayload> {
  let obj = value
    .into_object()
    .ok_or_else(|| type_error(ctx, "drop: payload must be an object"))?;

  let files = match obj.get::<_, Value<'js>>("files") {
    Ok(v) if !v.is_undefined() && !v.is_null() => Some(parse_input_files(ctx, v)?),
    _ => None,
  };

  let data = match obj.get::<_, Value<'js>>("data") {
    Ok(v) if !v.is_undefined() && !v.is_null() => {
      let map: std::collections::BTreeMap<String, String> = serde_from_js(ctx, v)?;
      map.into_iter().collect()
    },
    _ => Vec::new(),
  };

  Ok(ferridriver::options::DropPayload { files, data })
}

/// Parse a polymorphic `selectOption` `values` JS argument:
/// `string | string[] | { value?, label?, index? } | Array<...>`.
///
/// # Errors
///
/// A `TypeError` for a value of none of those shapes or a malformed descriptor.
pub fn parse_select_option_values<'js>(
  ctx: &Ctx<'js>,
  value: Value<'js>,
) -> rquickjs::Result<Vec<ferridriver::options::SelectOptionValue>> {
  if value.is_string() {
    let s: String = value.get()?;
    return Ok(vec![ferridriver::options::SelectOptionValue::by_value_or_label(s)]);
  }
  if let Some(arr) = value.as_array() {
    let len = arr.len();
    let mut out = Vec::with_capacity(len);
    for idx in 0..len {
      let el: Value<'js> = arr.get(idx)?;
      if el.is_string() {
        let s: String = el.get()?;
        out.push(ferridriver::options::SelectOptionValue::by_value_or_label(s));
      } else if el.is_object() {
        // Direct ferrijs-serde (no serde_json::Value middle-hop).
        let desc: ferridriver::options::SelectOptionValue = serde_from_js(ctx, el)?;
        out.push(desc);
      } else {
        return Err(type_error(
          ctx,
          "selectOption: array entries must be string or { value?, label?, index? } object",
        ));
      }
    }
    return Ok(out);
  }
  if value.is_object() {
    let desc: ferridriver::options::SelectOptionValue = serde_from_js(ctx, value)?;
    return Ok(vec![desc]);
  }
  Err(type_error(
    ctx,
    "selectOption: values must be string | string[] | object | object[]",
  ))
}

/// Parse Playwright's `SelectOptionOptions` JS bag into the core struct.
///
/// # Errors
///
/// As [`parse_opt_bag`].
pub fn parse_select_option_options<'js>(
  ctx: &Ctx<'js>,
  value: rquickjs::function::Opt<Value<'js>>,
) -> rquickjs::Result<Option<ferridriver::options::SelectOptionOptions>> {
  parse_opt_bag(ctx, value)
}

/// Parse Playwright's `FillOptions` JS bag into the core struct.
///
/// # Errors
///
/// As [`parse_opt_bag`].
pub fn parse_fill_options<'js>(
  ctx: &Ctx<'js>,
  value: rquickjs::function::Opt<Value<'js>>,
) -> rquickjs::Result<Option<ferridriver::options::FillOptions>> {
  parse_opt_bag(ctx, value)
}

/// Parse Playwright's `PressOptions` JS bag into the core struct.
///
/// # Errors
///
/// As [`parse_opt_bag`].
pub fn parse_press_options<'js>(
  ctx: &Ctx<'js>,
  value: rquickjs::function::Opt<Value<'js>>,
) -> rquickjs::Result<Option<ferridriver::options::PressOptions>> {
  parse_opt_bag(ctx, value)
}

/// Parse Playwright's `TypeOptions` JS bag — same shape as `PressOptions`.
///
/// # Errors
///
/// As [`parse_opt_bag`].
pub fn parse_type_options<'js>(
  ctx: &Ctx<'js>,
  value: rquickjs::function::Opt<Value<'js>>,
) -> rquickjs::Result<Option<ferridriver::options::TypeOptions>> {
  parse_opt_bag(ctx, value)
}

/// Parse Playwright's `CheckOptions` / `UncheckOptions` /
/// `SetCheckedOptions` JS bag into the core struct.
///
/// # Errors
///
/// As [`parse_opt_bag`].
pub fn parse_check_options<'js>(
  ctx: &Ctx<'js>,
  value: rquickjs::function::Opt<Value<'js>>,
) -> rquickjs::Result<Option<ferridriver::options::CheckOptions>> {
  parse_opt_bag(ctx, value)
}

/// Parse Playwright's `HoverOptions` JS bag into the core struct.
///
/// # Errors
///
/// As [`parse_opt_bag`].
pub fn parse_hover_options<'js>(
  ctx: &Ctx<'js>,
  value: rquickjs::function::Opt<Value<'js>>,
) -> rquickjs::Result<Option<ferridriver::options::HoverOptions>> {
  parse_opt_bag(ctx, value)
}

/// Parse Playwright's `TapOptions` JS bag into the core struct.
///
/// # Errors
///
/// As [`parse_opt_bag`].
pub fn parse_tap_options<'js>(
  ctx: &Ctx<'js>,
  value: rquickjs::function::Opt<Value<'js>>,
) -> rquickjs::Result<Option<ferridriver::options::TapOptions>> {
  parse_opt_bag(ctx, value)
}

/// Parse Playwright's `DblClickOptions` JS bag into the core struct.
///
/// # Errors
///
/// As [`parse_opt_bag`].
pub fn parse_dblclick_options<'js>(
  ctx: &Ctx<'js>,
  value: rquickjs::function::Opt<Value<'js>>,
) -> rquickjs::Result<Option<ferridriver::options::DblClickOptions>> {
  parse_opt_bag(ctx, value)
}

/// Parse Playwright's `ClickOptions` JS bag into the core struct.
/// Accepts `Opt<Value>` so callers pass `Opt(options)` verbatim; `None`,
/// `undefined`, or `null` → `Ok(None)`. Unknown `button` / `modifier`
/// strings raise a typed `rquickjs::Error` with the exact Playwright
/// message so JS-side assertions see `/Unknown (button|modifier)/` for
/// drift detection.
///
/// # Errors
///
/// As [`parse_opt_bag`].
pub fn parse_click_options<'js>(
  ctx: &Ctx<'js>,
  value: rquickjs::function::Opt<Value<'js>>,
) -> rquickjs::Result<Option<ferridriver::options::ClickOptions>> {
  parse_opt_bag(ctx, value)
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
///
/// # Errors
///
/// A `TypeError` for an `arg` that does not convert to JSON, and as
/// [`init_script_source_from_js`].
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
  Ok((init_script_source_from_js(ctx, script)?, arg_json))
}

/// The `script` of `addInitScript(script, ...)`: a function, source text,
/// or `{ path?, content? }`.
///
/// # Errors
///
/// A `TypeError` for a `script` of none of those shapes or an object with neither `content` nor
/// `path`, or a `PermissionDeniedError` for a `path` outside the realm's read grant.
pub fn init_script_source_from_js<'js>(
  ctx: &Ctx<'js>,
  script: Value<'js>,
) -> rquickjs::Result<ferridriver::options::InitScriptSource> {
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
      .ok_or_else(|| type_error(ctx, "addInitScript: expected object"))?;
    if let Ok(content) = obj.get::<_, String>("content") {
      ferridriver::options::InitScriptSource::Content(content)
    } else if let Ok(path) = obj.get::<_, String>("path") {
      readable(ctx, std::path::Path::new(&path))?;
      ferridriver::options::InitScriptSource::Path(path.into())
    } else {
      return Err(type_error(
        ctx,
        "addInitScript: Either path or content property must be present",
      ));
    }
  } else {
    return Err(type_error(
      ctx,
      "addInitScript: script must be Function | string | { path?, content? }",
    ));
  };
  Ok(init)
}
