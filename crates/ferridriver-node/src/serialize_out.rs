//! NAPI return-path: rehydrate an isomorphic [`SerializedValue`] into the
//! native JS value Playwright users expect (real `Date` / `RegExp` /
//! `BigInt` / `URL` / `Error` / typed arrays / `-0` / `NaN` / `±Infinity`).
//!
//! Mirrors Playwright's `parseSerializedValue` at
//! `/tmp/playwright/packages/playwright-core/src/protocol/serializers.ts:19`.

use std::ptr;

use ferridriver::protocol::{ErrorValue, RegExpValue, SerializedValue, SpecialValue, TypedArrayKind};
use napi::bindgen_prelude::*;
use napi::{Env, JsValue, sys};
use rustc_hash::FxHashMap;

/// Return-value wrapper for the NAPI evaluate / jsonValue surface. Holds
/// the raw [`SerializedValue`] produced by core; the napi-rs return-path
/// rehydrates it into native JS via [`Self::to_napi_value`] so callers
/// see real `Date`/`RegExp`/`BigInt`/etc. — matching Playwright exactly.
pub struct Evaluated(pub SerializedValue);

impl TypeName for Evaluated {
  fn type_name() -> &'static str {
    "unknown"
  }
  fn value_type() -> ValueType {
    ValueType::Unknown
  }
}

impl ToNapiValue for Evaluated {
  unsafe fn to_napi_value(raw_env: sys::napi_env, val: Self) -> Result<sys::napi_value> {
    let env = Env::from_raw(raw_env);
    let mut refs: FxHashMap<u32, sys::napi_value> = FxHashMap::default();
    rehydrate(&env, &val.0, &mut refs)
  }
}

/// Extract `(fn_source, is_function_hint)` from an evaluate `pageFunction`
/// arg that can be a JS string or a JS function — matches Playwright's
/// `String(pageFunction)` + `typeof pageFunction === 'function'` check at
/// `client/page.ts:515` / `client/frame.ts:196`.
pub fn extract_fn_source(value: Unknown<'_>) -> Result<(String, Option<bool>)> {
  let is_fn = value.get_type()? == ValueType::Function;
  let s = value.coerce_to_string()?.into_utf8()?.into_owned()?;
  Ok((s, Some(is_fn)))
}

fn rehydrate(
  env: &Env,
  value: &SerializedValue,
  refs: &mut FxHashMap<u32, sys::napi_value>,
) -> Result<sys::napi_value> {
  match value {
    SerializedValue::Bool(b) => {
      let mut out = ptr::null_mut();
      check_status!(
        unsafe { sys::napi_get_boolean(env.raw(), *b, &raw mut out) },
        "get_boolean"
      )?;
      Ok(out)
    },
    SerializedValue::Number(n) => create_double(env, *n),
    SerializedValue::Str(s) => {
      let js = env.create_string(s)?;
      Ok(js.raw())
    },
    SerializedValue::Special(SpecialValue::Null) => {
      let mut out = ptr::null_mut();
      check_status!(unsafe { sys::napi_get_null(env.raw(), &raw mut out) }, "get_null")?;
      Ok(out)
    },
    SerializedValue::Special(SpecialValue::Undefined) => {
      let mut out = ptr::null_mut();
      check_status!(
        unsafe { sys::napi_get_undefined(env.raw(), &raw mut out) },
        "get_undefined"
      )?;
      Ok(out)
    },
    SerializedValue::Special(SpecialValue::NaN) => create_double(env, f64::NAN),
    SerializedValue::Special(SpecialValue::Infinity) => create_double(env, f64::INFINITY),
    SerializedValue::Special(SpecialValue::NegInfinity) => create_double(env, f64::NEG_INFINITY),
    SerializedValue::Special(SpecialValue::NegZero) => create_double(env, -0.0),
    SerializedValue::Date(iso) => call_constructor(env, "Date", &[create_str(env, iso)?]),
    SerializedValue::Url(url) => call_constructor(env, "URL", &[create_str(env, url)?]),
    SerializedValue::BigInt(s) => call_global_fn(env, "BigInt", &[create_str(env, s)?]),
    SerializedValue::RegExp(RegExpValue { p, f }) => {
      call_constructor(env, "RegExp", &[create_str(env, p)?, create_str(env, f)?])
    },
    SerializedValue::Error(ErrorValue { m, n, s }) => {
      let err = call_constructor(env, "Error", &[create_str(env, m)?])?;
      set_named_property(env, err, "name", create_str(env, n)?)?;
      set_named_property(env, err, "stack", create_str(env, s)?)?;
      Ok(err)
    },
    SerializedValue::TypedArray(ta) => rehydrate_typed_array(env, ta.k, &ta.b),
    SerializedValue::ArrayBuffer(ab) => {
      let buf = ArrayBuffer::from_data(env, ab.b.clone())?;
      Ok(buf.raw())
    },
    SerializedValue::Array { id, items } => {
      let mut arr = ptr::null_mut();
      check_status!(
        unsafe { sys::napi_create_array_with_length(env.raw(), items.len(), &raw mut arr) },
        "create_array"
      )?;
      refs.insert(*id, arr);
      for (i, item) in items.iter().enumerate() {
        let v = rehydrate(env, item, refs)?;
        check_status!(
          unsafe { sys::napi_set_element(env.raw(), arr, u32::try_from(i).unwrap_or(u32::MAX), v) },
          "array set_element"
        )?;
      }
      Ok(arr)
    },
    SerializedValue::Object { id, entries } => {
      let mut obj = ptr::null_mut();
      check_status!(
        unsafe { sys::napi_create_object(env.raw(), &raw mut obj) },
        "create_object"
      )?;
      refs.insert(*id, obj);
      for entry in entries {
        let v = rehydrate(env, &entry.v, refs)?;
        set_named_property(env, obj, &entry.k, v)?;
      }
      Ok(obj)
    },
    SerializedValue::Reference(id) => refs
      .get(id)
      .copied()
      .ok_or_else(|| Error::new(Status::InvalidArg, format!("wire back-reference to unknown id {id}"))),
    SerializedValue::Handle(_) => Err(Error::new(
      Status::InvalidArg,
      "rehydrate: bare Handle in return value — use evaluateHandle()".to_string(),
    )),
  }
}

fn create_double(env: &Env, n: f64) -> Result<sys::napi_value> {
  let mut out = ptr::null_mut();
  check_status!(
    unsafe { sys::napi_create_double(env.raw(), n, &raw mut out) },
    "create_double"
  )?;
  Ok(out)
}

fn create_str(env: &Env, s: &str) -> Result<sys::napi_value> {
  let js = env.create_string(s)?;
  Ok(js.raw())
}

fn global_property(env: &Env, name: &str) -> Result<sys::napi_value> {
  let mut global = ptr::null_mut();
  check_status!(
    unsafe { sys::napi_get_global(env.raw(), &raw mut global) },
    "get_global"
  )?;
  let key = env.create_string(name)?;
  let mut prop = ptr::null_mut();
  check_status!(
    unsafe { sys::napi_get_property(env.raw(), global, key.raw(), &raw mut prop) },
    "get_global_property"
  )?;
  Ok(prop)
}

fn call_constructor(env: &Env, ctor_name: &str, args: &[sys::napi_value]) -> Result<sys::napi_value> {
  let ctor = global_property(env, ctor_name)?;
  let mut out = ptr::null_mut();
  check_status!(
    unsafe { sys::napi_new_instance(env.raw(), ctor, args.len(), args.as_ptr(), &raw mut out) },
    "new_instance"
  )?;
  Ok(out)
}

fn call_global_fn(env: &Env, fn_name: &str, args: &[sys::napi_value]) -> Result<sys::napi_value> {
  let func = global_property(env, fn_name)?;
  let mut global = ptr::null_mut();
  check_status!(
    unsafe { sys::napi_get_global(env.raw(), &raw mut global) },
    "get_global"
  )?;
  let mut out = ptr::null_mut();
  check_status!(
    unsafe { sys::napi_call_function(env.raw(), global, func, args.len(), args.as_ptr(), &raw mut out) },
    "call_function"
  )?;
  Ok(out)
}

fn set_named_property(env: &Env, obj: sys::napi_value, name: &str, value: sys::napi_value) -> Result<()> {
  let key = env.create_string(name)?;
  check_status!(
    unsafe { sys::napi_set_property(env.raw(), obj, key.raw(), value) },
    "set_property"
  )?;
  Ok(())
}

fn rehydrate_typed_array(env: &Env, kind: TypedArrayKind, bytes: &[u8]) -> Result<sys::napi_value> {
  // Create an ArrayBuffer holding the raw bytes, then construct the typed
  // array via `new <Kind>Array(buffer)` through the global constructor so
  // every variant shares one code path.
  let ab = ArrayBuffer::from_data(env, bytes.to_vec())?;
  let ab_raw = ab.raw();
  let ctor_name = match kind {
    TypedArrayKind::I8 => "Int8Array",
    TypedArrayKind::U8 => "Uint8Array",
    TypedArrayKind::U8Clamped => "Uint8ClampedArray",
    TypedArrayKind::I16 => "Int16Array",
    TypedArrayKind::U16 => "Uint16Array",
    TypedArrayKind::I32 => "Int32Array",
    TypedArrayKind::U32 => "Uint32Array",
    TypedArrayKind::F32 => "Float32Array",
    TypedArrayKind::F64 => "Float64Array",
    TypedArrayKind::BI64 => "BigInt64Array",
    TypedArrayKind::BUI64 => "BigUint64Array",
  };
  call_constructor(env, ctor_name, &[ab_raw])
}
