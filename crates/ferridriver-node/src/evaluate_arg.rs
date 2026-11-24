//! The `arg` of `evaluate` and its relatives, walked from the JS value the
//! way Playwright's `serializeValue` does: rich types cross as themselves,
//! a `JSHandle` anywhere inside becomes a handle reference, a repeated
//! object becomes a `{ref}`, and a function becomes a binding the page
//! calls back through, if the caller passed `exposeFunctions`.

use std::ptr;

use ferridriver::protocol::{
  ArgFunction, ArgPathStep, ErrorValue, HandleId, PropertyEntry, RegExpValue, SerializationContext, SerializedArgument,
  SerializedValue, SpecialValue, TypedArrayKind, TypedArrayValue,
};
use napi::bindgen_prelude::{ClassInstance, FromNapiValue, TypeName, Unknown, ValidateNapiValue};
use napi::{Env, JsValue as _, check_status, sys};

const MAX_DEPTH: u32 = 512;

/// An evaluate argument, serialized when it crosses into Rust. The
/// functions in it are already bound; whether the call may pass them is
/// the caller's `exposeFunctions`.
pub struct NapiEvaluateArg {
  arg: SerializedArgument,
  /// Playwright's error for the first function, for a call without
  /// `exposeFunctions`.
  unexpected: Option<String>,
}

impl NapiEvaluateArg {
  /// The argument for a call with or without `exposeFunctions`.
  pub fn into_argument(self, expose_functions: bool) -> napi::Result<SerializedArgument> {
    match self.unexpected {
      Some(message) if !expose_functions => Err(napi::Error::from_reason(message)),
      _ => Ok(self.arg),
    }
  }
}

impl TypeName for NapiEvaluateArg {
  fn type_name() -> &'static str {
    "unknown"
  }
  fn value_type() -> napi::ValueType {
    napi::ValueType::Unknown
  }
}

impl ValidateNapiValue for NapiEvaluateArg {}

#[expect(unsafe_code, reason = "napi-rs declares FromNapiValue::from_napi_value unsafe")]
impl FromNapiValue for NapiEvaluateArg {
  unsafe fn from_napi_value(env: sys::napi_env, value: sys::napi_value) -> napi::Result<Self> {
    // SAFETY: napi-rs hands this conversion the live `env` of the current
    // call and a `value` in its scope; the walk ends before this returns.
    let mut walk = unsafe { Walk::new(env) };
    let value = walk.serialize(value, 0)?;
    Ok(Self {
      arg: SerializedArgument {
        value,
        handles: walk.handles,
        functions: walk.functions,
      },
      unexpected: walk.unexpected,
    })
  }
}

/// One argument's walk. Every `napi_value` it is handed or reaches is a
/// handle in the scope of the call that built it, against `env`.
struct Walk {
  env: sys::napi_env,
  alloc: SerializationContext,
  handles: Vec<HandleId>,
  functions: Vec<ArgFunction>,
  unexpected: Option<String>,
  path: Vec<ArgPathStep>,
  visited: Vec<(sys::napi_value, u32)>,
}

#[expect(unsafe_code, reason = "the walk reads JS values through napi's C API")]
impl Walk {
  /// # Safety
  ///
  /// `env` is the live environment of the current JS call, and the walk
  /// is dropped before that call returns.
  unsafe fn new(env: sys::napi_env) -> Self {
    Self {
      env,
      alloc: SerializationContext::default(),
      handles: Vec::new(),
      functions: Vec::new(),
      unexpected: None,
      path: Vec::new(),
      visited: Vec::new(),
    }
  }

  fn serialize(&mut self, value: sys::napi_value, depth: u32) -> napi::Result<SerializedValue> {
    if depth > MAX_DEPTH {
      return Err(napi::Error::from_reason("argument too deeply nested"));
    }
    let mut kind = 0;
    // SAFETY: `self.env` is live and `value` is a handle in its scope (see
    // [`Walk`]); `kind` is a writable out-parameter.
    let status = unsafe { sys::napi_typeof(self.env, value, &raw mut kind) };
    check_status!(status)?;
    match kind {
      sys::ValueType::napi_undefined | sys::ValueType::napi_symbol => {
        Ok(SerializedValue::Special(SpecialValue::Undefined))
      },
      sys::ValueType::napi_null => Ok(SerializedValue::Special(SpecialValue::Null)),
      // SAFETY: a live handle (see [`Walk`]) that `napi_typeof` just
      // reported as a boolean.
      sys::ValueType::napi_boolean => Ok(SerializedValue::Bool(unsafe {
        bool::from_napi_value(self.env, value)?
      })),
      // SAFETY: a live handle (see [`Walk`]) that `napi_typeof` just
      // reported as a number.
      sys::ValueType::napi_number => Ok(SerializedValue::from_f64(unsafe {
        f64::from_napi_value(self.env, value)?
      })),
      // SAFETY: a live handle (see [`Walk`]) that `napi_typeof` just
      // reported as a string.
      sys::ValueType::napi_string => Ok(SerializedValue::Str(unsafe {
        String::from_napi_value(self.env, value)?
      })),
      sys::ValueType::napi_bigint => Ok(SerializedValue::BigInt(self.coerce_string(value)?)),
      sys::ValueType::napi_function => self.function(value),
      _ => self.object(value, depth),
    }
  }

  fn function(&mut self, value: sys::napi_value) -> napi::Result<SerializedValue> {
    if self.unexpected.is_none() {
      let display = self.coerce_string(value)?;
      self.unexpected = Some(ferridriver::protocol::unexpected_value_error(&self.path, &display).to_string());
    }
    // SAFETY: `self.env` is live and `value` is a handle in its scope (see
    // [`Walk`]); `Unknown` asks nothing more.
    let callback = unsafe { Unknown::from_raw_unchecked(self.env, value) };
    let binding = crate::exposed::binding(Env::from_raw(self.env), callback, false)?;
    let name = ArgFunction::fresh_name();
    self.functions.push(ArgFunction {
      name: name.clone(),
      binding,
    });
    Ok(SerializedValue::Function(name))
  }

  fn object(&mut self, value: sys::napi_value, depth: u32) -> napi::Result<SerializedValue> {
    if let Some(handle) = self.handle(value)? {
      return Ok(handle);
    }
    if let Some(rich) = self.rich(value)? {
      return Ok(rich);
    }
    for (seen, id) in &self.visited {
      let mut same = false;
      // SAFETY: both handles were reached by this walk, so are live in
      // `self.env`'s scope (see [`Walk`]); `same` is a writable out-parameter.
      let status = unsafe { sys::napi_strict_equals(self.env, *seen, value, &raw mut same) };
      check_status!(status)?;
      if same {
        return Ok(SerializedValue::Reference(*id));
      }
    }
    let id = self.alloc.alloc_id();
    self.visited.push((value, id));
    let mut is_array = false;
    // SAFETY: `self.env` is live and `value` is a handle in its scope (see
    // [`Walk`]); `is_array` is a writable out-parameter.
    let status = unsafe { sys::napi_is_array(self.env, value, &raw mut is_array) };
    check_status!(status)?;
    if is_array {
      let mut len = 0;
      // SAFETY: `value` is a live array handle (see [`Walk`]); `len` is a
      // writable out-parameter.
      let status = unsafe { sys::napi_get_array_length(self.env, value, &raw mut len) };
      check_status!(status)?;
      let mut items = Vec::with_capacity(len as usize);
      for index in 0..len {
        let mut item = ptr::null_mut();
        // SAFETY: `value` is a live array handle (see [`Walk`]); `item` is a
        // writable out-parameter.
        let status = unsafe { sys::napi_get_element(self.env, value, index, &raw mut item) };
        check_status!(status)?;
        self.path.push(ArgPathStep::Index(index as usize));
        // A hole reads as `undefined` and crosses as one, as Playwright's
        // `serializeValue` has it.
        let serialized = self.serialize(item, depth + 1)?;
        self.path.pop();
        items.push(serialized);
      }
      return Ok(SerializedValue::Array { id, items });
    }
    let mut keys = ptr::null_mut();
    // SAFETY: `self.env` is live and `value` is a handle in its scope (see
    // [`Walk`]); `keys` is a writable out-parameter.
    let status = unsafe {
      sys::napi_get_all_property_names(
        self.env,
        value,
        sys::KeyCollectionMode::own_only,
        sys::KeyFilter::enumerable | sys::KeyFilter::skip_symbols,
        sys::KeyConversion::numbers_to_strings,
        &raw mut keys,
      )
    };
    check_status!(status)?;
    let mut len = 0;
    // SAFETY: `keys` is the array napi just returned in `self.env`'s scope;
    // `len` is a writable out-parameter.
    let status = unsafe { sys::napi_get_array_length(self.env, keys, &raw mut len) };
    check_status!(status)?;
    let mut entries = Vec::with_capacity(len as usize);
    for index in 0..len {
      let mut key = ptr::null_mut();
      // SAFETY: `keys` is a live array handle in `self.env`'s scope; `key` is
      // a writable out-parameter.
      let status = unsafe { sys::napi_get_element(self.env, keys, index, &raw mut key) };
      check_status!(status)?;
      let mut item = ptr::null_mut();
      // SAFETY: `value` and `key` are live handles (see [`Walk`]); `item` is
      // a writable out-parameter.
      let status = unsafe { sys::napi_get_property(self.env, value, key, &raw mut item) };
      check_status!(status)?;
      // SAFETY: `key` is a live handle, a string because the key collection
      // above asked for `numbers_to_strings` and `skip_symbols`.
      let key = unsafe { String::from_napi_value(self.env, key)? };
      self.path.push(ArgPathStep::Key(key.clone()));
      let serialized = self.serialize(item, depth + 1)?;
      self.path.pop();
      entries.push(PropertyEntry { k: key, v: serialized });
    }
    Ok(SerializedValue::Object { id, entries })
  }

  fn handle(&mut self, value: sys::napi_value) -> napi::Result<Option<SerializedValue>> {
    // SAFETY: `value` is a live handle (see [`Walk`]); `ClassInstance`
    // checks the class it wraps before reading through it.
    let js_handle = unsafe { ClassInstance::<crate::js_handle::JSHandle>::from_napi_value(self.env, value) };
    let backing = if let Ok(handle) = js_handle {
      handle.inner_ref().backing().clone()
    } else if let Ok(handle) =
      // SAFETY: as above, for the `ElementHandle` class.
      unsafe { ClassInstance::<crate::element_handle::ElementHandle>::from_napi_value(self.env, value) }
    {
      handle.inner_ref().as_js_handle().backing().clone()
    } else {
      return Ok(None);
    };
    Ok(Some(match backing {
      ferridriver::js_handle::JSHandleBacking::Remote(remote) => {
        let index = u32::try_from(self.handles.len()).map_err(|_| napi::Error::from_reason("too many JS handles"))?;
        self.handles.push(remote.to_handle_id());
        SerializedValue::Handle(index)
      },
      ferridriver::js_handle::JSHandleBacking::Value(value) => value,
    }))
  }

  /// Upstream's rich types: an `Error`, `Date`, `URL`, `RegExp`, typed
  /// array or `ArrayBuffer` crosses as itself rather than as its keys.
  fn rich(&mut self, value: sys::napi_value) -> napi::Result<Option<SerializedValue>> {
    let mut yes = false;
    // SAFETY: `self.env` is live and `value` is a handle in its scope (see
    // [`Walk`]); `yes` is a writable out-parameter.
    let status = unsafe { sys::napi_is_error(self.env, value, &raw mut yes) };
    check_status!(status)?;
    if yes {
      return Ok(Some(SerializedValue::Error(ErrorValue {
        m: self.property_string(value, "message")?,
        n: self.property_string(value, "name")?,
        s: self.property_string(value, "stack")?,
      })));
    }
    // SAFETY: `self.env` is live and `value` is a handle in its scope (see
    // [`Walk`]); `yes` is a writable out-parameter.
    let status = unsafe { sys::napi_is_date(self.env, value, &raw mut yes) };
    check_status!(status)?;
    if yes {
      return Ok(Some(SerializedValue::Date(self.to_json(value)?)));
    }
    if self.instance_of(value, "URL")? {
      return Ok(Some(SerializedValue::Url(self.to_json(value)?)));
    }
    if self.instance_of(value, "RegExp")? {
      return Ok(Some(SerializedValue::RegExp(RegExpValue {
        p: self.property_string(value, "source")?,
        f: self.property_string(value, "flags")?,
      })));
    }
    // SAFETY: `self.env` is live and `value` is a handle in its scope (see
    // [`Walk`]); `yes` is a writable out-parameter.
    let status = unsafe { sys::napi_is_typedarray(self.env, value, &raw mut yes) };
    check_status!(status)?;
    if yes {
      let (mut kind, mut length, mut data, mut buffer, mut offset) = (0, 0, ptr::null_mut(), ptr::null_mut(), 0);
      // SAFETY: `value` is a live typed-array handle (see [`Walk`]); the five
      // out-parameters are writable locals of the types napi declares.
      let status = unsafe {
        sys::napi_get_typedarray_info(
          self.env,
          value,
          &raw mut kind,
          &raw mut length,
          &raw mut data,
          &raw mut buffer,
          &raw mut offset,
        )
      };
      check_status!(status)?;
      let Some(k) = typed_array_kind(kind) else {
        return Ok(None);
      };
      let bytes = length * k.bytes_per_element();
      let b = if bytes == 0 {
        Vec::new()
      } else {
        // SAFETY: napi reports `data` as the view's first element and
        // `length` elements of this kind behind it.
        unsafe { std::slice::from_raw_parts(data.cast::<u8>(), bytes) }.to_vec()
      };
      return Ok(Some(SerializedValue::TypedArray(TypedArrayValue { b, k })));
    }
    // SAFETY: `self.env` is live and `value` is a handle in its scope (see
    // [`Walk`]); `yes` is a writable out-parameter.
    let status = unsafe { sys::napi_is_arraybuffer(self.env, value, &raw mut yes) };
    check_status!(status)?;
    if yes {
      let (mut data, mut length) = (ptr::null_mut(), 0);
      // SAFETY: `value` is a live `ArrayBuffer` handle (see [`Walk`]); `data`
      // and `length` are writable out-parameters.
      let status = unsafe { sys::napi_get_arraybuffer_info(self.env, value, &raw mut data, &raw mut length) };
      check_status!(status)?;
      let b = if length == 0 {
        Vec::new()
      } else {
        // SAFETY: napi reports `length` bytes behind `data`.
        unsafe { std::slice::from_raw_parts(data.cast::<u8>(), length) }.to_vec()
      };
      return Ok(Some(SerializedValue::ArrayBuffer(
        ferridriver::protocol::ArrayBufferValue { b },
      )));
    }
    Ok(None)
  }

  fn is_undefined(&self, value: sys::napi_value) -> napi::Result<bool> {
    let mut kind = 0;
    // SAFETY: `self.env` is live and `value` is a handle in its scope (see
    // [`Walk`]); `kind` is a writable out-parameter.
    let status = unsafe { sys::napi_typeof(self.env, value, &raw mut kind) };
    check_status!(status)?;
    Ok(kind == sys::ValueType::napi_undefined)
  }

  fn coerce_string(&self, value: sys::napi_value) -> napi::Result<String> {
    let mut text = ptr::null_mut();
    // SAFETY: `self.env` is live and `value` is a handle in its scope (see
    // [`Walk`]); `text` is a writable out-parameter.
    let status = unsafe { sys::napi_coerce_to_string(self.env, value, &raw mut text) };
    check_status!(status)?;
    // SAFETY: `text` is the string napi just created in `self.env`'s scope.
    unsafe { String::from_napi_value(self.env, text) }
  }

  fn property(&self, object: sys::napi_value, name: &str) -> napi::Result<sys::napi_value> {
    let env = Env::from_raw(self.env);
    let key = env.create_string(name)?;
    let mut out = ptr::null_mut();
    // SAFETY: `object` is a live handle (see [`Walk`]) and `key` a string
    // just created in the same scope; `out` is a writable out-parameter.
    let status = unsafe { sys::napi_get_property(self.env, object, key.raw(), &raw mut out) };
    check_status!(status)?;
    Ok(out)
  }

  fn property_string(&self, object: sys::napi_value, name: &str) -> napi::Result<String> {
    let value = self.property(object, name)?;
    if self.is_undefined(value)? {
      return Ok(String::new());
    }
    self.coerce_string(value)
  }

  fn instance_of(&self, value: sys::napi_value, class: &str) -> napi::Result<bool> {
    let mut global = ptr::null_mut();
    // SAFETY: `self.env` is live (see [`Walk`]); `global` is a writable
    // out-parameter.
    let status = unsafe { sys::napi_get_global(self.env, &raw mut global) };
    check_status!(status)?;
    let constructor = self.property(global, class)?;
    let mut kind = 0;
    // SAFETY: `constructor` is the handle `property` just read in
    // `self.env`'s scope; `kind` is a writable out-parameter.
    let status = unsafe { sys::napi_typeof(self.env, constructor, &raw mut kind) };
    check_status!(status)?;
    if kind != sys::ValueType::napi_function {
      return Ok(false);
    }
    let mut yes = false;
    // SAFETY: `value` is a live handle (see [`Walk`]) and `constructor` a
    // function `napi_typeof` just confirmed; `yes` is a writable out-parameter.
    let status = unsafe { sys::napi_instanceof(self.env, value, constructor, &raw mut yes) };
    check_status!(status)?;
    Ok(yes)
  }

  fn to_json(&self, value: sys::napi_value) -> napi::Result<String> {
    let method = self.property(value, "toJSON")?;
    let mut out = ptr::null_mut();
    // SAFETY: `value` is a live handle (see [`Walk`]) and `method` the handle
    // just read from it; zero arguments pair with a null `argv`, and `out` is
    // a writable out-parameter. napi reports a non-callable `method` as a status.
    let status = unsafe { sys::napi_call_function(self.env, value, method, 0, ptr::null(), &raw mut out) };
    check_status!(status)?;
    let mut kind = 0;
    // SAFETY: `out` is the call's result in `self.env`'s scope; `kind` is a
    // writable out-parameter.
    let status = unsafe { sys::napi_typeof(self.env, out, &raw mut kind) };
    check_status!(status)?;
    if kind == sys::ValueType::napi_string {
      // SAFETY: `out` is live and `napi_typeof` just reported a string.
      unsafe { String::from_napi_value(self.env, out) }
    } else {
      Ok(String::new())
    }
  }
}

/// `JSON.stringify(value)` parsed back: the argument Playwright inlines
/// into a plain init script, functions and `undefined` dropped.
///
/// # Safety
///
/// `env` is the live environment of the current JS call and `value` a
/// handle in its scope.
#[expect(unsafe_code, reason = "reads a JS value through napi's C API")]
pub(crate) unsafe fn json_stringify(env: sys::napi_env, value: sys::napi_value) -> napi::Result<serde_json::Value> {
  // SAFETY: the caller's contract is `Walk::new`'s, and the walk ends here.
  let walk = unsafe { Walk::new(env) };
  let mut global = ptr::null_mut();
  // SAFETY: `env` is live per this function's contract; `global` is a
  // writable out-parameter.
  let status = unsafe { sys::napi_get_global(env, &raw mut global) };
  check_status!(status)?;
  let json = walk.property(global, "JSON")?;
  let stringify = walk.property(json, "stringify")?;
  let mut out = ptr::null_mut();
  // SAFETY: `json` and `stringify` were just read from the global object
  // in `env`'s scope and `value` is live per this function's contract; one
  // argument pairs with the one-element `argv` at `&value`, and `out` is a
  // writable out-parameter.
  let status = unsafe { sys::napi_call_function(env, json, stringify, 1, &raw const value, &raw mut out) };
  check_status!(status)?;
  if walk.is_undefined(out)? {
    return Ok(serde_json::Value::Null);
  }
  // SAFETY: `out` is the call's result in `env`'s scope; `JSON.stringify`
  // returns a string whenever it does not return `undefined`, handled above.
  let text = unsafe { String::from_napi_value(env, out)? };
  serde_json::from_str(&text).map_err(|e| napi::Error::from_reason(e.to_string()))
}

fn typed_array_kind(kind: sys::napi_typedarray_type) -> Option<TypedArrayKind> {
  Some(match kind {
    sys::TypedarrayType::int8_array => TypedArrayKind::I8,
    sys::TypedarrayType::uint8_array => TypedArrayKind::U8,
    sys::TypedarrayType::uint8_clamped_array => TypedArrayKind::U8Clamped,
    sys::TypedarrayType::int16_array => TypedArrayKind::I16,
    sys::TypedarrayType::uint16_array => TypedArrayKind::U16,
    sys::TypedarrayType::int32_array => TypedArrayKind::I32,
    sys::TypedarrayType::uint32_array => TypedArrayKind::U32,
    sys::TypedarrayType::float32_array => TypedArrayKind::F32,
    sys::TypedarrayType::float64_array => TypedArrayKind::F64,
    sys::TypedarrayType::bigint64_array => TypedArrayKind::BI64,
    sys::TypedarrayType::biguint64_array => TypedArrayKind::BUI64,
    _ => return None,
  })
}
