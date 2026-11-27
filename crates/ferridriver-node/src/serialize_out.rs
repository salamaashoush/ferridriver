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

#[expect(unsafe_code, reason = "napi-rs declares ToNapiValue::to_napi_value unsafe")]
impl ToNapiValue for Evaluated {
  unsafe fn to_napi_value(raw_env: sys::napi_env, val: Self) -> Result<sys::napi_value> {
    let env = Env::from_raw(raw_env);
    let mut rehydrator = Rehydrator {
      env: &env,
      refs: FxHashMap::default(),
      global: None,
    };
    rehydrator.rehydrate(val.0)
  }
}

/// Extract `(fn_source, is_function_hint)` from an evaluate `pageFunction`
/// arg that can be a JS string or a JS function — matches Playwright's
/// `String(pageFunction)` + `typeof pageFunction === 'function'` check at
/// `client/page.ts:515` / `client/frame.ts:196`.
pub(crate) fn extract_fn_source(value: Unknown<'_>) -> Result<(String, Option<bool>)> {
  let is_fn = value.get_type()? == ValueType::Function;
  let s = value.coerce_to_string()?.into_utf8()?.into_owned()?;
  Ok((s, Some(is_fn)))
}

struct Rehydrator<'a> {
  env: &'a Env,
  refs: FxHashMap<u32, sys::napi_value>,
  global: Option<sys::napi_value>,
}

/// Runs only under [`Evaluated::to_napi_value`]: `env` is the live
/// environment of that call, and every `napi_value` here and in the helpers
/// below is a handle created in its scope.
#[expect(unsafe_code, reason = "builds JS values through napi's C API")]
impl Rehydrator<'_> {
  fn rehydrate(&mut self, value: SerializedValue) -> Result<sys::napi_value> {
    match value {
      SerializedValue::Bool(b) => {
        let mut out = ptr::null_mut();
        // SAFETY: `self.env` is the live environment of the conversion this runs
        // under; `out` is a writable out-parameter.
        let status = unsafe { sys::napi_get_boolean(self.env.raw(), b, &raw mut out) };
        check_status!(status, "get_boolean")?;
        Ok(out)
      },
      SerializedValue::Number(n) => create_double(self.env, n),
      SerializedValue::Str(s) => {
        let js = self.env.create_string(&s)?;
        Ok(js.raw())
      },
      SerializedValue::Special(SpecialValue::Null) => {
        let mut out = ptr::null_mut();
        // SAFETY: `self.env` is live; `out` is a writable out-parameter.
        let status = unsafe { sys::napi_get_null(self.env.raw(), &raw mut out) };
        check_status!(status, "get_null")?;
        Ok(out)
      },
      SerializedValue::Special(SpecialValue::Undefined) => {
        let mut out = ptr::null_mut();
        // SAFETY: `self.env` is live; `out` is a writable out-parameter.
        let status = unsafe { sys::napi_get_undefined(self.env.raw(), &raw mut out) };
        check_status!(status, "get_undefined")?;
        Ok(out)
      },
      SerializedValue::Special(SpecialValue::NaN) => create_double(self.env, f64::NAN),
      SerializedValue::Special(SpecialValue::Infinity) => create_double(self.env, f64::INFINITY),
      SerializedValue::Special(SpecialValue::NegInfinity) => create_double(self.env, f64::NEG_INFINITY),
      SerializedValue::Special(SpecialValue::NegZero) => create_double(self.env, -0.0),
      SerializedValue::Date(iso) => {
        let arg = create_str(self.env, &iso)?;
        self.call_constructor("Date", &[arg])
      },
      SerializedValue::Url(url) => {
        let arg = create_str(self.env, &url)?;
        self.call_constructor("URL", &[arg])
      },
      SerializedValue::BigInt(s) => {
        let arg = create_str(self.env, &s)?;
        self.call_global_fn("BigInt", &[arg])
      },
      SerializedValue::RegExp(RegExpValue { p, f }) => {
        let args = [create_str(self.env, &p)?, create_str(self.env, &f)?];
        self.call_constructor("RegExp", &args)
      },
      SerializedValue::Error(ErrorValue { m, n, s }) => {
        let msg = create_str(self.env, &m)?;
        let err = self.call_constructor("Error", &[msg])?;
        set_named_property(self.env, err, "name", create_str(self.env, &n)?)?;
        set_named_property(self.env, err, "stack", create_str(self.env, &s)?)?;
        Ok(err)
      },
      SerializedValue::TypedArray(ta) => self.rehydrate_typed_array(ta.k, ta.b),
      SerializedValue::ArrayBuffer(ab) => {
        let buf = ArrayBuffer::from_data(self.env, ab.b)?;
        Ok(buf.raw())
      },
      SerializedValue::Array { id, items } => {
        let mut arr = ptr::null_mut();
        // SAFETY: `self.env` is live; `arr` is a writable out-parameter.
        let status = unsafe { sys::napi_create_array_with_length(self.env.raw(), items.len(), &raw mut arr) };
        check_status!(status, "create_array")?;
        self.refs.insert(id, arr);
        for (i, item) in items.into_iter().enumerate() {
          let v = self.rehydrate(item)?;
          // SAFETY: `self.env` is live; `arr` was created above and `v` rehydrated, both in this scope.
          let status = unsafe { sys::napi_set_element(self.env.raw(), arr, u32::try_from(i).unwrap_or(u32::MAX), v) };
          check_status!(status, "array set_element")?;
        }
        Ok(arr)
      },
      SerializedValue::Object { id, entries } => {
        let mut obj = ptr::null_mut();
        // SAFETY: `self.env` is live; `obj` is a writable out-parameter.
        let status = unsafe { sys::napi_create_object(self.env.raw(), &raw mut obj) };
        check_status!(status, "create_object")?;
        self.refs.insert(id, obj);
        for entry in entries {
          let v = self.rehydrate(entry.v)?;
          set_named_property(self.env, obj, &entry.k, v)?;
        }
        Ok(obj)
      },
      SerializedValue::Reference(id) => self
        .refs
        .get(&id)
        .copied()
        .ok_or_else(|| Error::new(Status::InvalidArg, format!("wire back-reference to unknown id {id}"))),
      SerializedValue::Handle(_) => Err(Error::new(
        Status::InvalidArg,
        "rehydrate: bare Handle in return value — use evaluateHandle()".to_string(),
      )),
      SerializedValue::Function(name) => Err(Error::new(
        Status::InvalidArg,
        format!("rehydrate: function binding {name} in a return value"),
      )),
    }
  }

  fn global(&mut self) -> Result<sys::napi_value> {
    if let Some(global) = self.global {
      return Ok(global);
    }
    let mut global = ptr::null_mut();
    // SAFETY: `self.env` is live; `global` is a writable out-parameter.
    let status = unsafe { sys::napi_get_global(self.env.raw(), &raw mut global) };
    check_status!(status, "get_global")?;
    self.global = Some(global);
    Ok(global)
  }

  fn global_property(&mut self, name: &str) -> Result<sys::napi_value> {
    let global = self.global()?;
    let key = self.env.create_string(name)?;
    let mut prop = ptr::null_mut();
    // SAFETY: `self.env` is live; `global` and `key` are live in this scope; `prop` is a writable out-parameter.
    let status = unsafe { sys::napi_get_property(self.env.raw(), global, key.raw(), &raw mut prop) };
    check_status!(status, "get_global_property")?;
    Ok(prop)
  }

  fn call_constructor(&mut self, ctor_name: &str, args: &[sys::napi_value]) -> Result<sys::napi_value> {
    let ctor = self.global_property(ctor_name)?;
    let mut out = ptr::null_mut();
    // SAFETY: `self.env` is live; `ctor` was just read in this scope and `args` holds `args.len()`
    // handles from it; `out` is a writable out-parameter.
    let status = unsafe { sys::napi_new_instance(self.env.raw(), ctor, args.len(), args.as_ptr(), &raw mut out) };
    check_status!(status, "new_instance")?;
    Ok(out)
  }

  fn call_global_fn(&mut self, fn_name: &str, args: &[sys::napi_value]) -> Result<sys::napi_value> {
    let global = self.global()?;
    let func = self.global_property(fn_name)?;
    let mut out = ptr::null_mut();
    // SAFETY: `self.env` is live; `global` and `func` are live in this scope and `args`
    // holds `args.len()` handles from it; `out` is a writable out-parameter.
    let status =
      unsafe { sys::napi_call_function(self.env.raw(), global, func, args.len(), args.as_ptr(), &raw mut out) };
    check_status!(status, "call_function")?;
    Ok(out)
  }

  fn rehydrate_typed_array(&mut self, kind: TypedArrayKind, bytes: Vec<u8>) -> Result<sys::napi_value> {
    // Transfer the owned byte vector directly into an ArrayBuffer without
    // copying, then construct the typed-array view via `new <Kind>Array(buffer)`.
    let ab = ArrayBuffer::from_data(self.env, bytes)?;
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
    self.call_constructor(ctor_name, &[ab_raw])
  }
}

#[expect(unsafe_code, reason = "builds JS values through napi's C API")]
fn create_double(env: &Env, n: f64) -> Result<sys::napi_value> {
  let mut out = ptr::null_mut();
  // SAFETY: `env` is the live environment of the conversion this runs
  // under; `out` is a writable out-parameter.
  let status = unsafe { sys::napi_create_double(env.raw(), n, &raw mut out) };
  check_status!(status, "create_double")?;
  Ok(out)
}

fn create_str(env: &Env, s: &str) -> Result<sys::napi_value> {
  let js = env.create_string(s)?;
  Ok(js.raw())
}

#[expect(unsafe_code, reason = "builds JS values through napi's C API")]
fn set_named_property(env: &Env, obj: sys::napi_value, name: &str, value: sys::napi_value) -> Result<()> {
  let key = env.create_string(name)?;
  // SAFETY: `env` is the live environment of the conversion this runs
  // under; `obj` is an object rehydrated in this scope and `key` and
  // `value` are handles from it.
  let status = unsafe { sys::napi_set_property(env.raw(), obj, key.raw(), value) };
  check_status!(status, "set_property")?;
  Ok(())
}
