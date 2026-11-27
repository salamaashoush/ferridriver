//! `QuickJS` <-> `SerializedValue` / `SerializedArgument` marshaling for `evaluate` and `evaluateHandle`.

use rquickjs::{Ctx, Function, Object, Value};

use super::{define_own, f64_to_i64, ms_f64_to_u64, throw_named, type_error};

/// Lower a `QuickJS` JS argument into a
/// [`ferridriver::protocol::SerializedArgument`] ready for
/// `page.evaluate(fn, arg)` / `page.evaluateHandle(fn, arg)` etc.
///
/// Covers JSON-expressible values (primitives, plain arrays, plain
/// objects) plus top-level `JSHandle` / `ElementHandle` class
/// instances. `undefined` / absent maps to the utility script's
/// `{v: "undefined"}` sentinel; `null` maps to `{v: "null"}`.
///
/// Class-instance detection: a `JSHandleJs` or `ElementHandleJs`
/// value is emitted as `SerializedValue::Handle(index)` with its
/// backend [`ferridriver::protocol::HandleId`] pushed into the shared
/// handle table. Detection is recursive, so handles nested inside
/// object / array user args keep their page-side identity.
///
/// # Errors
///
/// A `TypeError` for an argument nested too deeply or cyclic, an `Error` naming where a function
/// sat in it, or whatever reading one of its properties throws.
pub fn quickjs_arg_to_serialized<'js>(
  ctx: &Ctx<'js>,
  value: Option<Value<'js>>,
) -> rquickjs::Result<ferridriver::protocol::SerializedArgument> {
  quickjs_arg_with_functions(ctx, value, ArgFunctions::Reject)
}

/// What a function inside an argument becomes.
#[derive(Clone, Copy)]
pub enum ArgFunctions {
  /// Playwright without `exposeFunctions`: an error naming where it sat.
  Reject,
  /// With `exposeFunctions`: a binding the page calls back through,
  /// released when the page with this key closes, if one owns it.
  Expose { page_key: Option<usize> },
}

impl ArgFunctions {
  /// The mode an `{ exposeFunctions?: boolean }` option bag selects.
  ///
  /// # Errors
  ///
  /// When reading `exposeFunctions` throws or its value is not a boolean.
  pub fn from_options(options: Option<&Value<'_>>, page_key: Option<usize>) -> rquickjs::Result<Self> {
    let expose = match options.and_then(Value::as_object) {
      Some(bag) => bag.get::<_, Option<bool>>("exposeFunctions")?.unwrap_or(false),
      None => false,
    };
    Ok(if expose {
      Self::Expose { page_key }
    } else {
      Self::Reject
    })
  }
}

/// The `{ timeout?: number }` of an evaluate option bag (Playwright
/// `locator.evaluate(pageFunction, arg?, options?)`).
///
/// # Errors
///
/// When reading `timeout` throws or its value is not a number.
pub fn evaluate_options(
  options: Option<&Value<'_>>,
) -> rquickjs::Result<Option<ferridriver::options::EvaluateOptions>> {
  let Some(bag) = options.and_then(Value::as_object) else {
    return Ok(None);
  };
  Ok(Some(ferridriver::options::EvaluateOptions {
    timeout: bag.get::<_, Option<f64>>("timeout")?.map(ms_f64_to_u64),
  }))
}

/// [`quickjs_arg_to_serialized`] with the functions in the argument
/// handled as `functions` says.
///
/// # Errors
///
/// As [`quickjs_arg_to_serialized`], except that under [`ArgFunctions::Expose`] a function becomes a
/// binding rather than an error.
pub fn quickjs_arg_with_functions<'js>(
  ctx: &Ctx<'js>,
  value: Option<Value<'js>>,
  functions: ArgFunctions,
) -> rquickjs::Result<ferridriver::protocol::SerializedArgument> {
  use ferridriver::protocol::{SerializedArgument, SerializedValue, SpecialValue};

  let v = match value {
    Some(v) if !v.is_undefined() => v,
    _ => {
      return Ok(SerializedArgument {
        value: SerializedValue::Special(SpecialValue::Undefined),
        handles: Vec::new(),
        functions: Vec::new(),
      });
    },
  };

  if v.is_null() {
    return Ok(SerializedArgument {
      value: SerializedValue::Special(SpecialValue::Null),
      handles: Vec::new(),
      functions: Vec::new(),
    });
  }

  // Direct JS -> SerializedValue. The old path went
  // JS -> serde_json::Value -> SerializedValue, allocating the whole
  // argument tree twice on every `page.evaluate(fn, arg)` /
  // `locator.evaluate`. Walk once, retaining numeric wire sentinels.
  let mut walk = ArgWalk {
    ctx,
    globals: ctx.globals(),
    object_proto: CachedGlobal::Unchecked,
    is_view_fn: CachedGlobal::Unchecked,
    alloc: ferridriver::protocol::SerializationContext::default(),
    handles: Vec::new(),
    mode: functions,
    functions: Vec::new(),
    path: Vec::new(),
    visited: Vec::new(),
  };
  let value = walk.serialize(&v, 0)?;
  Ok(SerializedArgument {
    value,
    handles: walk.handles,
    functions: walk.functions,
  })
}

/// Recursion cap. A cyclic / pathologically deep argument previously
/// errored out of `serde_from_js` (serde can't represent a cycle); keep
/// that behaviour with an explicit bound instead of overflowing.
const MAX_ARG_DEPTH: u32 = 512;

enum CachedGlobal<T> {
  Unchecked,
  Missing,
  Present(T),
}

struct ArgWalk<'a, 'js> {
  ctx: &'a Ctx<'js>,
  globals: Object<'js>,
  /// Cached `Object.prototype` for O(1) plain-object fast path in `rich_object`.
  object_proto: CachedGlobal<Object<'js>>,
  /// Cached `ArrayBuffer.isView` to guard the 11-entry `TYPED_ARRAYS` scan.
  is_view_fn: CachedGlobal<Function<'js>>,
  alloc: ferridriver::protocol::SerializationContext,
  handles: Vec<ferridriver::protocol::HandleId>,
  mode: ArgFunctions,
  functions: Vec<ferridriver::protocol::ArgFunction>,
  path: Vec<ferridriver::protocol::ArgPathStep>,
  /// Arrays and objects already written, by id: a second visit is a
  /// `{ref}`, which is what keeps a cycle from recursing forever.
  visited: Vec<(Value<'js>, u32)>,
}

impl<'js> ArgWalk<'_, 'js> {
  fn object_proto(&mut self) -> rquickjs::Result<Option<Object<'js>>> {
    match &self.object_proto {
      CachedGlobal::Present(proto) => return Ok(Some(proto.clone())),
      CachedGlobal::Missing => return Ok(None),
      CachedGlobal::Unchecked => {},
    }
    let proto = self
      .globals
      .get::<_, Option<Object<'js>>>("Object")?
      .map(|ctor| ctor.get::<_, Option<Object<'js>>>("prototype"))
      .transpose()?
      .flatten();
    self.object_proto = match &proto {
      Some(p) => CachedGlobal::Present(p.clone()),
      None => CachedGlobal::Missing,
    };
    Ok(proto)
  }

  fn is_array_buffer_view(&mut self, v: &Value<'js>) -> rquickjs::Result<bool> {
    let is_view = match &self.is_view_fn {
      CachedGlobal::Present(f) => Some(f.clone()),
      CachedGlobal::Missing => None,
      CachedGlobal::Unchecked => {
        let f = self
          .globals
          .get::<_, Option<Object<'js>>>("ArrayBuffer")?
          .map(|ctor| ctor.get::<_, Option<Function<'js>>>("isView"))
          .transpose()?
          .flatten();
        self.is_view_fn = match &f {
          Some(func) => CachedGlobal::Present(func.clone()),
          None => CachedGlobal::Missing,
        };
        f
      },
    };
    match is_view {
      Some(f) => f.call((v.clone(),)),
      None => Ok(true),
    }
  }

  /// Walk a JS value into a wire [`ferridriver::protocol::SerializedValue`].
  fn serialize(&mut self, v: &Value<'js>, depth: u32) -> rquickjs::Result<ferridriver::protocol::SerializedValue> {
    use ferridriver::protocol::{ArgPathStep, SerializedValue, SpecialValue};

    if depth > MAX_ARG_DEPTH {
      return Err(type_error(v.ctx(), "argument too deeply nested or cyclic"));
    }

    if v.is_undefined() {
      return Ok(SerializedValue::Special(SpecialValue::Undefined));
    }
    if v.is_null() {
      return Ok(SerializedValue::Special(SpecialValue::Null));
    }
    if let Some(b) = v.as_bool() {
      return Ok(SerializedValue::Bool(b));
    }
    if let Some(i) = v.as_int() {
      return Ok(SerializedValue::from_f64(f64::from(i)));
    }
    if let Some(f) = v.as_float() {
      return Ok(SerializedValue::from_f64(f));
    }
    if let Some(s) = v.as_string() {
      return Ok(SerializedValue::Str(s.to_string()?));
    }
    if let Some(bi) = v.as_big_int() {
      // Wire BigInt is a decimal string. `to_i64` covers the common
      // range; a value outside it errors (the old serde_json path could
      // not represent BigInt at all, so erroring is not a regression).
      return match bi.clone().to_i64() {
        Ok(n) => Ok(SerializedValue::BigInt(n.to_string())),
        Err(_) => Err(type_error(v.ctx(), "BigInt argument out of i64 range")),
      };
    }
    if let Some(f) = v.as_function() {
      return self.function(v, f.clone());
    }
    if let Some(arr) = v.as_array() {
      if let Some((_, id)) = self.visited.iter().find(|(seen, _)| seen == v) {
        return Ok(SerializedValue::Reference(*id));
      }
      let id = self.alloc.alloc_id();
      self.visited.push((v.clone(), id));
      let mut items = Vec::with_capacity(arr.len());
      for idx in 0..arr.len() {
        let el: Value<'_> = arr.get(idx)?;
        self.path.push(ArgPathStep::Index(idx));
        // A hole reads as `undefined` and crosses as one, as Playwright's
        // `serializeValue` has it.
        items.push(self.serialize(&el, depth + 1)?);
        self.path.pop();
      }
      return Ok(SerializedValue::Array { id, items });
    }
    if let Some(rich) = self.rich_object(v)? {
      return Ok(rich);
    }
    if let Some((_, id)) = self.visited.iter().find(|(seen, _)| seen == v) {
      return Ok(SerializedValue::Reference(*id));
    }
    if let Some(obj) = v.as_object() {
      // Plain object: own enumerable string keys. An undefined or
      // symbol-valued property stays, as `undefined` (Playwright's
      // `serializeValue` walks `Object.keys`).
      let id = self.alloc.alloc_id();
      self.visited.push((v.clone(), id));
      let mut entries = Vec::new();
      for key in obj.keys::<String>() {
        let key = key?;
        let val: Value<'_> = obj.get(&key)?;
        self.path.push(ArgPathStep::Key(key.clone()));
        let v = self.serialize(&val, depth + 1)?;
        self.path.pop();
        entries.push(ferridriver::protocol::PropertyEntry { k: key, v });
      }
      if entries.is_empty()
        && let Some(handle) = handle_value_to_serialized(v, &mut self.handles)?
      {
        return Ok(handle);
      }
      return Ok(SerializedValue::Object { id, entries });
    }

    // Symbol / other non-JSON value at this position:
    // JSON.stringify treats it as undefined.
    Ok(SerializedValue::Special(SpecialValue::Undefined))
  }

  /// Upstream's `serializeValue` rich types: an `Error`, `Date`, `URL`,
  /// `RegExp`, `ArrayBuffer` or typed array crosses as itself rather than
  /// as its own keys.
  fn rich_object(&mut self, v: &Value<'js>) -> rquickjs::Result<Option<ferridriver::protocol::SerializedValue>> {
    use ferridriver::protocol::{ArrayBufferValue, ErrorValue, RegExpValue, SerializedValue, TypedArrayValue};

    let Some(obj) = v.as_object() else {
      return Ok(None);
    };
    let text = |key: &str| -> rquickjs::Result<String> {
      Ok(
        obj
          .get::<_, Option<rquickjs::convert::Coerced<String>>>(key)?
          .map(|s| s.0)
          .unwrap_or_default(),
      )
    };
    if v.is_error() {
      return Ok(Some(SerializedValue::Error(ErrorValue {
        m: text("message")?,
        n: text("name")?,
        s: text("stack")?,
      })));
    }
    if let Some(buffer) = rquickjs::ArrayBuffer::from_object(obj.clone()) {
      return Ok(Some(SerializedValue::ArrayBuffer(ArrayBufferValue {
        b: array_buffer_bytes(&buffer, 0, usize::MAX).unwrap_or_default(),
      })));
    }
    // Plain objects (`{}` and `Object.create(null)`) have `Object.prototype`
    // or `null` as their direct prototype, so they cannot be `instanceof` any
    // rich built-in (`Date`, `URL`, `RegExp`, or any `TypedArray`). Skipping
    // the 14 global lookups + `instanceof` checks here keeps plain-object
    // walks fast.
    let proto = obj.get_prototype();
    if proto.is_none() {
      return Ok(None);
    }
    if let Some(object_proto) = self.object_proto()?
      && proto.as_ref() == Some(&object_proto)
    {
      return Ok(None);
    }
    let is = |globals: &Object<'js>, class: &str| -> rquickjs::Result<bool> {
      Ok(match globals.get::<_, Option<rquickjs::Object<'js>>>(class)? {
        Some(constructor) => obj.is_instance_of(&constructor),
        None => false,
      })
    };
    let to_json = || -> rquickjs::Result<String> {
      let method: rquickjs::Function<'js> = obj.get("toJSON")?;
      let json: Option<rquickjs::convert::Coerced<String>> = method.call((rquickjs::function::This(obj.clone()),))?;
      Ok(json.map(|s| s.0).unwrap_or_default())
    };
    if is(&self.globals, "Date")? {
      return Ok(Some(SerializedValue::Date(to_json()?)));
    }
    if is(&self.globals, "URL")? {
      return Ok(Some(SerializedValue::Url(to_json()?)));
    }
    if is(&self.globals, "RegExp")? {
      return Ok(Some(SerializedValue::RegExp(RegExpValue {
        p: text("source")?,
        f: text("flags")?,
      })));
    }
    if self.is_array_buffer_view(v)? {
      for (class, k) in TYPED_ARRAYS {
        if !is(&self.globals, class)? {
          continue;
        }
        // A view covers `byteLength` bytes of its buffer from `byteOffset`,
        // which is the whole of it only when nothing sliced it.
        let buffer: rquickjs::ArrayBuffer<'js> = obj.get("buffer")?;
        let (offset, length): (usize, usize) = (obj.get("byteOffset")?, obj.get("byteLength")?);
        let b = buffer_bytes(&buffer, offset, length);
        return Ok(Some(SerializedValue::TypedArray(TypedArrayValue { b, k })));
      }
    }
    Ok(None)
  }

  fn function(
    &mut self,
    v: &Value<'js>,
    f: rquickjs::Function<'js>,
  ) -> rquickjs::Result<ferridriver::protocol::SerializedValue> {
    match self.mode {
      ArgFunctions::Reject => {
        let display = v
          .clone()
          .get::<rquickjs::convert::Coerced<String>>()
          .map(|s| s.0)
          .unwrap_or_default();
        let message = ferridriver::protocol::unexpected_value_error(&self.path, &display).to_string();
        Err(throw_named(self.ctx, "Error", message))
      },
      ArgFunctions::Expose { page_key } => {
        let function = crate::bindings::page::argument_function(self.ctx, f, page_key)?;
        let value = ferridriver::protocol::SerializedValue::Function(function.name.clone());
        self.functions.push(function);
        Ok(value)
      },
    }
  }
}

fn handle_value_to_serialized(
  v: &Value<'_>,
  handles: &mut Vec<ferridriver::protocol::HandleId>,
) -> rquickjs::Result<Option<ferridriver::protocol::SerializedValue>> {
  if let Ok(class) = rquickjs::Class::<crate::bindings::js_handle::JSHandleJs>::from_value(v) {
    let inner = class.borrow();
    return Ok(Some(handle_backing_to_serialized(
      v.ctx(),
      inner.inner().backing(),
      handles,
    )?));
  }
  if let Ok(class) = rquickjs::Class::<crate::bindings::element_handle::ElementHandleJs>::from_value(v) {
    let inner = class.borrow();
    let handle = inner.inner().as_js_handle();
    return Ok(Some(handle_backing_to_serialized(v.ctx(), handle.backing(), handles)?));
  }
  Ok(None)
}

fn handle_backing_to_serialized(
  ctx: &Ctx<'_>,
  backing: &ferridriver::js_handle::JSHandleBacking,
  handles: &mut Vec<ferridriver::protocol::HandleId>,
) -> rquickjs::Result<ferridriver::protocol::SerializedValue> {
  match backing {
    ferridriver::js_handle::JSHandleBacking::Remote(remote) => {
      let idx = u32::try_from(handles.len()).map_err(|_| type_error(ctx, "too many JS handles"))?;
      handles.push(remote.to_handle_id());
      Ok(ferridriver::protocol::SerializedValue::Handle(idx))
    },
    ferridriver::js_handle::JSHandleBacking::Value(value) => Ok(value.clone()),
  }
}

/// Convert a [`ferridriver::protocol::SerializedValue`] into a native
/// `QuickJS` JS value — `Date` / `RegExp` / `BigInt` / `URL` / `Error` /
/// typed arrays / `NaN` / `±Infinity` / `undefined` / `-0` all round-trip
/// as their native JS form. Mirrors Playwright's `parseSerializedValue`
/// at `/tmp/playwright/packages/playwright-core/src/protocol/serializers.ts:19`.
///
/// # Errors
///
/// A `TypeError` when a global constructor the value needs is missing or a buffer is longer than
/// `u32::MAX` bytes, or whatever the engine throws while building the value.
pub fn serialized_value_to_quickjs<'js>(
  ctx: &Ctx<'js>,
  value: &ferridriver::protocol::SerializedValue,
) -> rquickjs::Result<Value<'js>> {
  let mut refs: rustc_hash::FxHashMap<u32, Value<'js>> = rustc_hash::FxHashMap::default();
  rehydrate(ctx, value, &mut refs)
}

fn rehydrate<'js>(
  ctx: &Ctx<'js>,
  value: &ferridriver::protocol::SerializedValue,
  refs: &mut rustc_hash::FxHashMap<u32, Value<'js>>,
) -> rquickjs::Result<Value<'js>> {
  use ferridriver::protocol::{ErrorValue, RegExpValue, SerializedValue, SpecialValue};

  match value {
    SerializedValue::Bool(b) => Ok(Value::new_bool(ctx.clone(), *b)),
    SerializedValue::Number(n) => {
      if let Some(i) = f64_as_exact_i32(*n) {
        Ok(Value::new_int(ctx.clone(), i))
      } else {
        Ok(Value::new_number(ctx.clone(), *n))
      }
    },
    SerializedValue::Str(s) => {
      let js = rquickjs::String::from_str(ctx.clone(), s)?;
      Ok(js.into_value())
    },
    SerializedValue::Special(SpecialValue::Null) => Ok(Value::new_null(ctx.clone())),
    SerializedValue::Special(SpecialValue::Undefined) => Ok(Value::new_undefined(ctx.clone())),
    SerializedValue::Special(SpecialValue::NaN) => Ok(Value::new_number(ctx.clone(), f64::NAN)),
    SerializedValue::Special(SpecialValue::Infinity) => Ok(Value::new_number(ctx.clone(), f64::INFINITY)),
    SerializedValue::Special(SpecialValue::NegInfinity) => Ok(Value::new_number(ctx.clone(), f64::NEG_INFINITY)),
    SerializedValue::Special(SpecialValue::NegZero) => Ok(Value::new_float(ctx.clone(), -0.0)),
    SerializedValue::Date(iso) => construct_global(ctx, "Date", (iso.as_str(),)),
    SerializedValue::Url(url) => construct_global(ctx, "URL", (url.as_str(),)),
    SerializedValue::BigInt(s) => {
      // BigInt(value) — must be called as function, not constructor.
      let func: Function<'js> = ctx.globals().get("BigInt")?;
      func.call((s.as_str(),))
    },
    SerializedValue::RegExp(RegExpValue { p, f }) => construct_global(ctx, "RegExp", (p.as_str(), f.as_str())),
    SerializedValue::Error(ErrorValue { m, n, s }) => {
      let err: Value<'js> = construct_global(ctx, "Error", (m.as_str(),))?;
      let obj = err
        .as_object()
        .ok_or_else(|| type_error(ctx, "cannot convert the value: not an object"))?;
      obj.set("name", n.as_str())?;
      obj.set("stack", s.as_str())?;
      Ok(err)
    },
    SerializedValue::TypedArray(ta) => rehydrate_typed_array(ctx, ta.k, &ta.b),
    SerializedValue::ArrayBuffer(ab) => Ok(rquickjs::ArrayBuffer::new_copy(ctx.clone(), &ab.b)?.into_value()),
    SerializedValue::Array { id, items } => {
      // `Array::new` already owns one `Ctx` dup (stored inside `arr`).
      // The back-ref map needs a `Value` handle to the same array while
      // `arr` stays alive for `set`; clone the borrowed underlying value
      // (one dup) rather than cloning the whole `Array` (which would dup
      // the `Ctx` a second time). `into_value` at the end is a move.
      let arr = rquickjs::Array::new(ctx.clone())?;
      refs.insert(*id, arr.as_value().clone());
      for (i, item) in items.iter().enumerate() {
        let v = rehydrate(ctx, item, refs)?;
        arr.set(i, v)?;
      }
      Ok(arr.into_value())
    },
    SerializedValue::Object { id, entries } => {
      let obj = Object::new(ctx.clone())?;
      refs.insert(*id, obj.as_value().clone());
      for entry in entries {
        let v = rehydrate(ctx, &entry.v, refs)?;
        define_own(&obj, &entry.k, v)?;
      }
      Ok(obj.into_value())
    },
    SerializedValue::Reference(id) => refs
      .get(id)
      .cloned()
      .ok_or_else(|| type_error(ctx, format!("cannot convert the value: unknown back-ref id {id}"))),
    SerializedValue::Handle(_) => Err(type_error(
      ctx,
      "cannot convert the value: bare Handle in return value — use evaluateHandle()",
    )),
    SerializedValue::Function(name) => Err(type_error(
      ctx,
      format!("cannot convert the value: function binding {name} in a return value"),
    )),
  }
}

pub(super) fn f64_as_exact_i32(n: f64) -> Option<i32> {
  if n.is_finite() && n.fract() == 0.0 && n >= f64::from(i32::MIN) && n <= f64::from(i32::MAX) {
    i32::try_from(f64_to_i64(n)).ok()
  } else {
    None
  }
}

fn construct_global<'js, Args>(ctx: &Ctx<'js>, ctor_name: &'static str, args: Args) -> rquickjs::Result<Value<'js>>
where
  Args: rquickjs::function::IntoArgs<'js>,
{
  let raw: Value<'js> = ctx.globals().get(ctor_name)?;
  let ctor = raw
    .try_into_constructor()
    .map_err(|_| type_error(ctx, "cannot convert the value: global is not a constructor"))?;
  ctor.construct(args)
}

fn rehydrate_typed_array<'js>(
  ctx: &Ctx<'js>,
  kind: ferridriver::protocol::TypedArrayKind,
  bytes: &[u8],
) -> rquickjs::Result<Value<'js>> {
  // Copy the bytes into a QuickJS ArrayBuffer in one memcpy, then wrap it
  // in `new <Kind>Array(buffer)` so every typed-array variant shares one path.
  let ab = rquickjs::ArrayBuffer::new_copy(ctx.clone(), bytes)?.into_value();
  let ctor_name = TYPED_ARRAYS
    .iter()
    .find(|(_, known)| *known == kind)
    .map_or("Uint8Array", |(name, _)| name);
  construct_global(ctx, ctor_name, (ab,))
}

/// `length` bytes of `buffer` from `offset`, or none of them once it is
/// detached or the range runs past its end.
fn buffer_bytes(buffer: &rquickjs::ArrayBuffer<'_>, offset: usize, length: usize) -> Vec<u8> {
  copy_array_buffer(buffer, |bytes| bytes.get(offset..offset.saturating_add(length))).unwrap_or_default()
}

/// Up to `length` bytes of `buffer` from `offset`, the range clamped to the
/// buffer; `None` once the buffer is detached.
pub(crate) fn array_buffer_bytes(buffer: &rquickjs::ArrayBuffer<'_>, offset: usize, length: usize) -> Option<Vec<u8>> {
  copy_array_buffer(buffer, |bytes| {
    let start = offset.min(bytes.len());
    let end = offset.saturating_add(length).min(bytes.len());
    bytes.get(start..end)
  })
}

/// A `Uint8Array`'s bytes; `None` once its buffer is detached.
#[expect(
  unsafe_code,
  reason = "rquickjs exposes a typed array's bytes only through the unsafe `as_bytes`"
)]
pub(crate) fn typed_array_bytes(array: &rquickjs::TypedArray<'_, u8>) -> Option<Vec<u8>> {
  // SAFETY: `as_bytes` requires that no JS runs while the slice lives. It
  // is copied into a `Vec` within this expression, before control returns
  // to anything that could run JS.
  unsafe { array.as_bytes() }.map(<[u8]>::to_vec)
}

/// Copy the part of `buffer` that `select` picks out. `select` is a plain
/// function of the bytes, which is what keeps the borrow below sound.
#[expect(
  unsafe_code,
  reason = "rquickjs exposes an ArrayBuffer's bytes only through the unsafe `as_bytes`"
)]
fn copy_array_buffer(
  buffer: &rquickjs::ArrayBuffer<'_>,
  select: impl FnOnce(&[u8]) -> Option<&[u8]>,
) -> Option<Vec<u8>> {
  // SAFETY: `as_bytes` requires that no JS runs while the slice lives. The
  // only code that sees it is `select`, which every caller in this module
  // passes as a pure range computation, and the selection is copied into a
  // `Vec` before this function returns.
  let bytes = unsafe { buffer.as_bytes() }?;
  select(bytes).map(<[u8]>::to_vec)
}

/// Each typed-array constructor and the tag it crosses the wire under.
const TYPED_ARRAYS: [(&str, ferridriver::protocol::TypedArrayKind); 11] = {
  use ferridriver::protocol::TypedArrayKind;
  [
    ("Int8Array", TypedArrayKind::I8),
    ("Uint8Array", TypedArrayKind::U8),
    ("Uint8ClampedArray", TypedArrayKind::U8Clamped),
    ("Int16Array", TypedArrayKind::I16),
    ("Uint16Array", TypedArrayKind::U16),
    ("Int32Array", TypedArrayKind::I32),
    ("Uint32Array", TypedArrayKind::U32),
    ("Float32Array", TypedArrayKind::F32),
    ("Float64Array", TypedArrayKind::F64),
    ("BigInt64Array", TypedArrayKind::BI64),
    ("BigUint64Array", TypedArrayKind::BUI64),
  ]
};

/// Extract `(fn_source, is_function_hint)` from an evaluate `pageFunction`
/// arg that can be a JS string or a JS function — matches Playwright's
/// `String(pageFunction)` + `typeof pageFunction === 'function'` check.
/// For functions, invokes the engine's `Function.prototype.toString()`
/// via global `String(...)`.
///
/// # Errors
///
/// Whatever `String(value)` throws.
pub fn extract_page_function<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> rquickjs::Result<(String, Option<bool>)> {
  let is_fn = value.is_function();
  let s: String = if let Some(str_val) = value.clone().into_string() {
    str_val.to_string()?
  } else {
    // For Function / other object: invoke global String(v) to run
    // ECMA ToString, which calls Function.prototype.toString for
    // functions (matching Playwright's `String(pageFunction)`).
    let string_fn: Function<'js> = ctx.globals().get("String")?;
    string_fn.call((value,))?
  };
  Ok((s, Some(is_fn)))
}
