//! Conversion helpers between ferridriver core types and `rquickjs` values.

use ferridriver::FerriError;
use rquickjs::object::Property;
use rquickjs::{Ctx, Object, Value};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Lower a [`FerriError`] into a thrown JS exception whose `name`
/// mirrors the core error name (`"TimeoutError"`, `"TargetClosedError"`,
/// ...), so `e.name === 'TimeoutError'` holds, matching Playwright.
#[must_use]
pub fn ferri_throw(ctx: &Ctx<'_>, e: &FerriError) -> rquickjs::Error {
  throw_named(ctx, e.name(), e.to_string())
}

/// Throw a real JS `Error` with `name` set explicitly — for
/// binding-level failures that have no [`FerriError`] behind them
/// (wait-loop deadlines, channel teardowns, assertion failures) but
/// must still satisfy `e.name === 'TimeoutError'`-style checks exactly
/// like core errors thrown via [`ferri_throw`].
/// `Error::new_from_js_message` cannot do this: it surfaces in scripts
/// as a `TypeError` with a mangled "Error converting from js ..."
/// message and a fixed name.
pub use ferrijs::std::node::throw_named;

/// Throw a real `TypeError` carrying the whole message: the error for an
/// argument a binding cannot use.
///
/// `rquickjs::Error::new_from_js_message` is banned in `clippy.toml`:
/// rquickjs throws it through `QuickJS`'s 256-byte format buffer, so a longer
/// message was cut, and every one read "Error converting from js ...".
pub fn type_error(ctx: &Ctx<'_>, message: impl Into<String>) -> rquickjs::Error {
  let message = message.into();
  let built: rquickjs::Result<Value<'_>> = (|| {
    let ctor: rquickjs::function::Constructor<'_> = ctx.globals().get("TypeError")?;
    ctor.construct((message.as_str(),))
  })();
  match built {
    Ok(error) => ctx.throw(error),
    Err(_) => throw_named(ctx, "TypeError", message),
  }
}

/// A failure in the synchronous part of a promise-returning binding (its
/// arguments read, its wait armed), held for the promise: the promise
/// rejects with it, as an async method's does, rather than the call
/// throwing past a `.then` / `.catch` the caller chained onto it.
pub enum Deferred<'js> {
  Thrown(Value<'js>),
  Error(rquickjs::Error),
}

impl<'js> Deferred<'js> {
  /// The failure, taken off the context when it is a thrown exception.
  #[must_use]
  pub fn hold(ctx: &Ctx<'js>, error: rquickjs::Error) -> Self {
    if error.is_exception() {
      Self::Thrown(ctx.catch())
    } else {
      Self::Error(error)
    }
  }

  /// Throw it again, from the promise.
  #[must_use]
  pub fn raise(self, ctx: &Ctx<'js>) -> rquickjs::Error {
    match self {
      Self::Thrown(value) => ctx.throw(value),
      Self::Error(error) => error,
    }
  }
}

/// Run a binding's synchronous part now, holding a failure for its
/// promise (see [`Deferred`]).
///
/// # Errors
///
/// A [`Deferred`] holding whatever `arm` fails with.
pub fn arm_now<'js, T>(ctx: &Ctx<'js>, arm: impl FnOnce() -> rquickjs::Result<T>) -> Result<T, Deferred<'js>> {
  arm().map_err(|error| Deferred::hold(ctx, error))
}

/// [`throw_named`] with the `stack` the thrower decided on, rather than
/// the one the engine captured at the `Error` construction site.
///
/// A boxed `test.step` re-attributes its failure to the line that called
/// the step (`ferridriver_test::step::boxed_error_stack`), and that
/// stack is what the user catches and what every report prints.
#[must_use]
pub fn throw_named_with_stack(ctx: &Ctx<'_>, name: &str, message: String, stack: Option<String>) -> rquickjs::Error {
  let Some(stack) = stack else {
    return throw_named(ctx, name, message);
  };
  let built: rquickjs::Result<Value<'_>> = (|| {
    let ctor: rquickjs::function::Constructor<'_> = ctx.globals().get("Error")?;
    let err: Object<'_> = ctor.construct((message.as_str(),))?;
    err.set("name", name)?;
    err.set("stack", stack)?;
    Ok(err.into_value())
  })();
  match built {
    Ok(v) => ctx.throw(v),
    Err(_) => throw_named(ctx, name, message),
  }
}

/// Adapter: `Result<T, FerriError>` into `rquickjs::Result<T>` with a
/// properly-named thrown `Error` (see [`ferri_throw`]).
pub trait FerriResultCtxExt<T> {
  /// The value, or the error thrown into `ctx`.
  ///
  /// # Errors
  ///
  /// The core error, rethrown under its JS name (see [`ferri_throw`]).
  fn into_js_with(self, ctx: &Ctx<'_>) -> rquickjs::Result<T>;
}

impl<T> FerriResultCtxExt<T> for Result<T, FerriError> {
  fn into_js_with(self, ctx: &Ctx<'_>) -> rquickjs::Result<T> {
    self.map_err(|e| ferri_throw(ctx, &e))
  }
}

/// Convert a JS millisecond value (`f64`) into a `u64`, clamping
/// negatives to `0` and NaN to `u64::MAX`. The fractional part is
/// truncated: sub-millisecond precision is irrelevant for timeouts.
#[must_use]
pub fn ms_f64_to_u64(ms: f64) -> u64 {
  if ms.is_nan() { u64::MAX } else { f64_to_u64(ms) }
}

/// A JS number as a `u64`: the fraction truncated, a negative or NaN value
/// as `0`, a value past `u64::MAX` saturating.
#[expect(
  clippy::cast_possible_truncation,
  clippy::cast_sign_loss,
  reason = "a float-to-int `as` truncates toward zero, saturates at both bounds and maps NaN to 0, exactly the conversion wanted"
)]
#[must_use]
pub fn f64_to_u64(v: f64) -> u64 {
  v as u64
}

/// [`f64_to_u64`], saturating at `u32::MAX`.
#[must_use]
pub fn f64_to_u32(v: f64) -> u32 {
  u32::try_from(f64_to_u64(v)).unwrap_or(u32::MAX)
}

/// [`f64_to_u64`], saturating at `usize::MAX`.
#[must_use]
pub fn f64_to_usize(v: f64) -> usize {
  usize::try_from(f64_to_u64(v)).unwrap_or(usize::MAX)
}

/// A JS number as an `i64`: the fraction truncated, NaN as `0`, a value
/// past either bound saturating.
#[expect(
  clippy::cast_possible_truncation,
  reason = "a float-to-int `as` truncates toward zero, saturates at both bounds and maps NaN to 0, exactly the conversion wanted"
)]
#[must_use]
pub fn f64_to_i64(v: f64) -> i64 {
  v as i64
}

/// A count, id or millisecond value as the JS `number` it crosses as.
#[expect(
  clippy::cast_precision_loss,
  reason = "a JS number is an f64; a value past 2^53 rounds exactly as it would in JS"
)]
#[must_use]
pub fn u64_to_f64(v: u64) -> f64 {
  v as f64
}

/// [`u64_to_f64`] for a signed value.
#[expect(
  clippy::cast_precision_loss,
  reason = "a JS number is an f64; a value past 2^53 rounds exactly as it would in JS"
)]
#[must_use]
pub fn i64_to_f64(v: i64) -> f64 {
  v as f64
}

/// [`u64_to_f64`] for a `usize` count.
#[must_use]
pub fn usize_to_f64(v: usize) -> f64 {
  u64_to_f64(u64::try_from(v).unwrap_or(u64::MAX))
}

/// A duration in whole milliseconds as the JS `number` it crosses as.
#[expect(
  clippy::cast_precision_loss,
  reason = "a JS number is an f64; a value past 2^53 rounds exactly as it would in JS"
)]
#[must_use]
pub fn duration_ms_to_f64(d: std::time::Duration) -> f64 {
  d.as_millis() as f64
}

/// A duration in whole milliseconds, saturating at `u64::MAX`.
#[must_use]
pub fn duration_ms_to_u64(d: std::time::Duration) -> u64 {
  u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// Parse a `waitForEvent`-style trailing argument: a bare timeout
/// number (documented ferridriver extension) or Playwright's
/// `{ timeout }` options bag. Returns `None` when omitted so callers
/// apply their own default.
///
/// # Errors
///
/// A `TypeError` when the options bag is malformed.
pub fn parse_timeout_number_or_bag<'js>(
  ctx: &Ctx<'js>,
  options: rquickjs::function::Opt<rquickjs::Value<'js>>,
) -> rquickjs::Result<Option<u64>> {
  match options.0 {
    Some(v) if v.is_number() => Ok(v.as_number().map(ms_f64_to_u64)),
    Some(v) if v.is_object() => {
      #[derive(serde::Deserialize, Default)]
      #[serde(rename_all = "camelCase", default)]
      struct JsTimeoutBag {
        timeout: Option<f64>,
      }
      let parsed: JsTimeoutBag = serde_from_js(ctx, v)?;
      Ok(parsed.timeout.map(ms_f64_to_u64))
    },
    _ => Ok(None),
  }
}

/// Bytes as Playwright hands them: a Node `Buffer` over `buffer`'s memory,
/// no copy. A `Buffer` is a `Uint8Array` subclass, so `includes` finds a
/// byte sequence and `toString('base64')` encodes, as a Node spec expects;
/// a plain view would answer `includes(buffer)` with a quiet `false`. A
/// realm without `Buffer` gets the view.
///
/// # Errors
///
/// Whatever `Buffer.from` throws, or, in a realm without `Buffer`, a failure to make the `Uint8Array`
/// view.
pub fn node_buffer<'js>(ctx: &Ctx<'js>, buffer: rquickjs::ArrayBuffer<'js>) -> rquickjs::Result<Value<'js>> {
  let from = ctx
    .globals()
    .get::<_, Value<'js>>("Buffer")
    .ok()
    .and_then(Value::into_object)
    .and_then(|buffer| buffer.get::<_, Value<'js>>("from").ok())
    .and_then(Value::into_function);
  match from {
    Some(from) => from.call((buffer,)),
    None => Ok(rquickjs::TypedArray::<u8>::from_arraybuffer(buffer)?.into_value()),
  }
}

/// Convert any `serde::Serialize` value into a JS value via
/// `ferrijs-serde` — direct `T` -> `rquickjs::Value`, no JSON string
/// and no `serde_json::Value` middle allocation. Used for binding
/// returns (cookies, storage state, parsed JSON bodies).
///
/// # Errors
///
/// A `TypeError` (`cannot convert the value: ...`) when `value` does not serialize.
pub fn serde_to_js<'js, T: Serialize>(ctx: &Ctx<'js>, value: &T) -> rquickjs::Result<Value<'js>> {
  ferrijs_serde::to_value(ctx.clone(), value).map_err(|e| type_error(ctx, format!("cannot convert the value: {e}")))
}

/// Build an already-settled JS `Promise` synchronously on the current
/// `QuickJS` context — avoids allocating a Rust future and scheduling a
/// `ctx.spawn` poll for values that are already computed (e.g. buffered
/// `APIResponse` body accessors).
///
/// # Errors
///
/// Throws only when the engine cannot allocate the promise itself.
pub fn settled_promise<'js, T: rquickjs::IntoJs<'js>>(
  ctx: &Ctx<'js>,
  value: rquickjs::Result<T>,
) -> rquickjs::Result<rquickjs::Promise<'js>> {
  let (promise, resolve, reject) = rquickjs::Promise::new(ctx)?;
  match value {
    Ok(v) => {
      let (): () = resolve.call((v,))?;
    },
    Err(err) => {
      let reason = if err.is_exception() {
        ctx.catch()
      } else {
        rquickjs::String::from_str(ctx.clone(), &err.to_string())?.into_value()
      };
      let (): () = reject.call((reason,))?;
    },
  }
  Ok(promise)
}

/// Build a JS `Array<{ name, value }>` directly from any iterator of
/// name/value pairs in one pass — no intermediate `Vec` and no serde
/// dispatch. Used by `request`/`response`/`apiResponse` `headersArray()`.
///
/// # Errors
///
/// Throws when the engine cannot allocate the array or an entry object.
pub fn name_value_array_to_js<'js, I, K, V>(ctx: &Ctx<'js>, pairs: I) -> rquickjs::Result<Value<'js>>
where
  I: IntoIterator<Item = (K, V)>,
  K: AsRef<str>,
  V: AsRef<str>,
{
  let arr = rquickjs::Array::new(ctx.clone())?;
  for (i, (name, value)) in pairs.into_iter().enumerate() {
    let obj = Object::new(ctx.clone())?;
    obj.set("name", name.as_ref())?;
    obj.set("value", value.as_ref())?;
    arr.set(i, obj)?;
  }
  Ok(arr.into_value())
}

/// Inverse of [`serde_to_js`] — deserialize a JS value into a Rust type
/// via `ferrijs-serde` (direct `Value` -> `T`). Integral-float ->
/// integer coercion, `undefined`/function-property drop, Proxy and
/// cycle handling all hold (covered by the ferrijs-serde test suite),
/// so the option-bag call sites keep their prior semantics without our
/// own hand-rolled walker.
///
/// # Errors
///
/// A `TypeError` (`invalid argument: ...`) when `value` does not have the shape of `T`.
pub fn serde_from_js<'js, T: DeserializeOwned>(ctx: &Ctx<'js>, value: Value<'js>) -> rquickjs::Result<T> {
  ferrijs_serde::from_value(value).map_err(|e| type_error(ctx, format!("invalid argument: {e}")))
}

/// Deserialize an optional JS option bag straight into a core options
/// struct — `None` / `undefined` / `null` → `Ok(None)`. The core
/// `ferridriver::options` structs carry the wire shape themselves
/// (serde camelCase + defaults, enum lowering with Playwright error
/// text), so there is no binding-side mirror struct to drift: a field
/// added in core is automatically accepted from JS.
///
/// # Errors
///
/// As [`serde_from_js`], for a bag that is present.
pub fn parse_opt_bag<'js, T: DeserializeOwned>(
  ctx: &Ctx<'js>,
  value: rquickjs::function::Opt<Value<'js>>,
) -> rquickjs::Result<Option<T>> {
  match value.0 {
    Some(v) if !v.is_undefined() && !v.is_null() => Ok(Some(serde_from_js(ctx, v)?)),
    _ => Ok(None),
  }
}

/// Define `key` as an own data property (writable/enumerable/
/// configurable, like a normal JS literal field) on `obj`.
///
/// Untrusted input — page-controlled `evaluate` results, script args —
/// can contain a `__proto__` (or other accessor) key. `Object::set`
/// routes through `[[Set]]`, so such a key would invoke the
/// `__proto__` setter (retargeting the object's prototype) or any
/// inherited setter. `Object::prop` lowers to `JS_DefineProperty`,
/// which always creates an own data property and never triggers a
/// setter — the value lands exactly where a JSON consumer expects.
fn define_own<'js, V: rquickjs::IntoJs<'js>>(obj: &Object<'js>, key: &str, value: V) -> rquickjs::Result<()> {
  obj.prop(key, Property::from(value).writable().enumerable().configurable())
}

/// Build an `rquickjs::Value` from a `serde_json::Value`. Thin wrapper
/// over [`serde_to_js`], kept for the script-args call site in
/// `engine.rs`.
/// An absent value as Playwright spells it in TS: `T | null`.
///
/// rquickjs lowers a bare `Option::None` to `undefined`, which is a
/// different value in JS — `=== null` and `toBeNull()` both fail on it,
/// while every Playwright signature that can answer "nothing" says
/// `null`. A binding whose upstream declaration is `T | null` returns
/// `Null<T>` instead of `Option<T>`.
pub struct Null<T>(pub Option<T>);

impl<'js, T: rquickjs::IntoJs<'js>> rquickjs::IntoJs<'js> for Null<T> {
  fn into_js(self, ctx: &Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    match self.0 {
      Some(v) => v.into_js(ctx),
      None => Ok(Value::new_null(ctx.clone())),
    }
  }
}

pub(crate) fn json_to_js<'js>(ctx: &Ctx<'js>, v: &serde_json::Value) -> rquickjs::Result<Value<'js>> {
  // A transitive dep force-enables `serde_json/arbitrary_precision`
  // workspace-wide. Under that feature `serde_json::Value::Number`'s
  // `Serialize` emits a private one-key map, so routing through
  // `serde_to_js` (ferrijs-serde) would inject numbers into JS as
  // `{"$serde_json::private::Number": "..."}` objects. Walk the value
  // explicitly with the AP-safe `as_*` accessors instead.
  match v {
    serde_json::Value::Null => Ok(Value::new_null(ctx.clone())),
    serde_json::Value::Bool(b) => Ok(Value::new_bool(ctx.clone(), *b)),
    serde_json::Value::Number(n) => {
      let f = n.as_f64().unwrap_or(f64::NAN);
      if let Some(i) = f64_as_exact_i32(f) {
        Ok(Value::new_int(ctx.clone(), i))
      } else {
        Ok(Value::new_number(ctx.clone(), f))
      }
    },
    serde_json::Value::String(s) => Ok(rquickjs::String::from_str(ctx.clone(), s)?.into_value()),
    serde_json::Value::Array(items) => {
      let arr = rquickjs::Array::new(ctx.clone())?;
      for (i, item) in items.iter().enumerate() {
        arr.set(i, json_to_js(ctx, item)?)?;
      }
      Ok(arr.into_value())
    },
    serde_json::Value::Object(map) => {
      let obj = Object::new(ctx.clone())?;
      for (k, val) in map {
        define_own(&obj, k.as_str(), json_to_js(ctx, val)?)?;
      }
      Ok(obj.into_value())
    },
  }
}

// ── evaluate(fn, arg) wire bridge (Phase D) ───────────────────────────

mod evaluate;
mod options;

use evaluate::f64_as_exact_i32;
pub use evaluate::{
  ArgFunctions, evaluate_options, extract_page_function, quickjs_arg_to_serialized, quickjs_arg_with_functions,
  serialized_value_to_quickjs,
};
pub(crate) use evaluate::{array_buffer_bytes, typed_array_bytes};
pub(crate) use options::{check_context_paths, readable, writable};
pub use options::{
  init_script_from_js, init_script_source_from_js, parse_check_options, parse_click_options, parse_dblclick_options,
  parse_dispatch_event_options, parse_drop_options, parse_drop_payload, parse_fill_options, parse_hover_options,
  parse_input_files, parse_press_options, parse_select_option_options, parse_select_option_values,
  parse_set_input_files_options, parse_tap_options, parse_type_options,
};
