//! Web-platform globals with no upstream in llrt: `atob` / `btoa`,
//! `structuredClone` and `performance`.

use base64::Engine as _;
use base64::engine::GeneralPurpose;
use base64::engine::general_purpose::GeneralPurposeConfig;
use rquickjs::function::{Func, This};
use rquickjs::{Class, Ctx, Object, TypedArray, Value};

/// Monotonic base for `performance.now()`, and the wall-clock instant it
/// corresponds to (`performance.timeOrigin`). Both are fixed at first
/// use, which is process start for any real session.
static PROCESS_START: std::sync::LazyLock<std::time::Instant> = std::sync::LazyLock::new(std::time::Instant::now);
static TIME_ORIGIN: std::sync::LazyLock<f64> = std::sync::LazyLock::new(|| {
  // Touch the monotonic base first so the two are taken together.
  let _ = *PROCESS_START;
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map_or(0.0, |d| d.as_secs_f64() * 1000.0)
});

/// Install `atob`, `btoa`, `structuredClone` and `performance`.
///
/// # Errors
///
/// Propagates the global writes.
pub fn init(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
  let globals = ctx.globals();

  // btoa/atob over a Latin1 "binary string", per the WHATWG contract.
  globals.set(
    "btoa",
    Func::from(|s: String| -> rquickjs::Result<String> {
      let mut bytes = Vec::with_capacity(s.len());
      for ch in s.chars() {
        let c = ch as u32;
        if c > 0xFF {
          return Err(rquickjs::Error::new_from_js_message(
            "btoa",
            "InvalidCharacterError",
            "string contains characters outside the Latin1 range".to_string(),
          ));
        }
        bytes.push(c as u8);
      }
      Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
    }),
  )?;
  globals.set(
    "atob",
    Func::from(|s: String| -> rquickjs::Result<String> {
      let bytes = forgiving_base64_decode(&s)
        .map_err(|m| rquickjs::Error::new_from_js_message("atob", "InvalidCharacterError", m.to_string()))?;
      Ok(bytes.into_iter().map(|b| b as char).collect())
    }),
  )?;

  globals.set("structuredClone", Func::from(structured_clone))?;

  // `performance.now()` — milliseconds (fractional) since the session's
  // process start, plus the `timeOrigin` those are relative to. A
  // monotonic `Instant` base, so it cannot go backwards across a wall-
  // clock adjustment the way `Date.now()` deltas can.
  let performance = Object::new(ctx.clone())?;
  performance.set("now", Func::from(|| PROCESS_START.elapsed().as_secs_f64() * 1000.0))?;
  performance.set("timeOrigin", *TIME_ORIGIN)?;
  globals.set("performance", performance)?;

  Ok(())
}

/// WHATWG "forgiving-base64 decode"
/// (<https://infra.spec.whatwg.org/#forgiving-base64-decode>): strip
/// ALL ASCII whitespace (not just the ends), reject a length ≡ 1 mod 4,
/// tolerate missing/partial `=` padding, and discard non-zero trailing
/// bits. `base64::STANDARD` does none of this (canonical padding only,
/// no whitespace), so a spec-conformant `atob` needs the explicit
/// algorithm here.
fn forgiving_base64_decode(input: &str) -> Result<Vec<u8>, &'static str> {
  let mut s: String = input
    .chars()
    .filter(|c| !matches!(c, '\t' | '\n' | '\u{0C}' | '\r' | ' '))
    .collect();
  // At most two trailing '=' are stripped; any remaining '=' (or one
  // that leaves length ≡ 1 mod 4) is invalid.
  if s.ends_with('=') {
    s.pop();
    if s.ends_with('=') {
      s.pop();
    }
  }
  if s.len() % 4 == 1 || s.contains('=') {
    return Err("invalid base64 length");
  }
  if !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/') {
    return Err("invalid base64 character");
  }
  // No-pad alphabet, padding indifferent (we stripped it), trailing
  // bits discarded — exactly the forgiving contract.
  let engine = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new()
      .with_encode_padding(false)
      .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent)
      .with_decode_allow_trailing_bits(true),
  );
  engine.decode(s.as_bytes()).map_err(|_| "invalid base64")
}

/// HTML `structuredClone(value)` — a deep clone by the structured-clone
/// algorithm.
///
/// Handles cycles and repeated references (the same object reached twice
/// stays the same object in the clone), `Array`, plain `Object`, `Map`,
/// `Set`, `Date`, `RegExp`, `ArrayBuffer` and typed arrays. Functions,
/// symbols and class instances are not cloneable and raise a
/// `DataCloneError` `DOMException`, per spec — never a silent
/// pass-through, which would alias the original.
fn structured_clone<'js>(ctx: Ctx<'js>, value: Value<'js>) -> rquickjs::Result<Value<'js>> {
  let mut seen: Vec<(Value<'js>, Value<'js>)> = Vec::new();
  clone_value(&ctx, &value, &mut seen)
}

fn data_clone_error(ctx: &Ctx<'_>, what: &str) -> rquickjs::Error {
  let ex = crate::exceptions::DOMException::new_with_name(
    ctx,
    crate::exceptions::DOMExceptionName::DataCloneError,
    format!("{what} could not be cloned"),
  );
  match ex.and_then(|ex| Class::instance(ctx.clone(), ex)) {
    Ok(ex) => ctx.throw(ex.into_value()),
    Err(e) => e,
  }
}

fn clone_value<'js>(
  ctx: &Ctx<'js>,
  value: &Value<'js>,
  seen: &mut Vec<(Value<'js>, Value<'js>)>,
) -> rquickjs::Result<Value<'js>> {
  if value.is_function() {
    return Err(data_clone_error(ctx, "a function"));
  }
  if value.type_of() == rquickjs::Type::Symbol {
    return Err(data_clone_error(ctx, "a symbol"));
  }
  let Some(obj) = value.as_object() else {
    // Primitives are immutable: cloning is identity.
    return Ok(value.clone());
  };
  if let Some((_, clone)) = seen.iter().find(|(orig, _)| orig.as_object() == Some(obj)) {
    return Ok(clone.clone());
  }

  let globals = ctx.globals();
  let is_a = |name: &str| -> rquickjs::Result<bool> {
    let ctor: Value<'js> = globals.get(name)?;
    Ok(obj.is_instance_of(&ctor))
  };

  // Dates and RegExps round-trip through their own constructors.
  if is_a("Date")? {
    let ctor: rquickjs::function::Constructor<'js> = globals.get("Date")?;
    let time: f64 = obj
      .get::<_, rquickjs::Function<'js>>("getTime")?
      .call((This(obj.clone()),))?;
    return ctor.construct::<_, Value<'js>>((time,));
  }
  if is_a("RegExp")? {
    let ctor: rquickjs::function::Constructor<'js> = globals.get("RegExp")?;
    let source: String = obj.get("source")?;
    let flags: String = obj.get("flags")?;
    return ctor.construct::<_, Value<'js>>((source, flags));
  }
  if let Some(buf) = rquickjs::ArrayBuffer::from_object(obj.clone()) {
    let bytes = buf.as_bytes().unwrap_or_default().to_vec();
    return Ok(rquickjs::ArrayBuffer::new(ctx.clone(), bytes)?.into_value());
  }
  if let Ok(ta) = TypedArray::<u8>::from_value(value.clone()) {
    let bytes = ta.as_bytes().unwrap_or_default().to_vec();
    return Ok(TypedArray::new(ctx.clone(), bytes)?.into_value());
  }

  if let Some(arr) = value.as_array() {
    let out = rquickjs::Array::new(ctx.clone())?;
    seen.push((value.clone(), out.clone().into_value()));
    for i in 0..arr.len() {
      let item: Value<'js> = arr.get(i)?;
      out.set(i, clone_value(ctx, &item, seen)?)?;
    }
    return Ok(out.into_value());
  }

  if is_a("Map")? {
    let ctor: rquickjs::function::Constructor<'js> = globals.get("Map")?;
    let out: Value<'js> = ctor.construct(())?;
    seen.push((value.clone(), out.clone()));
    let out_obj = out.as_object().cloned().unwrap_or_else(|| obj.clone());
    let set: rquickjs::Function<'js> = out_obj.get("set")?;
    for entry in iterate_entries(ctx, obj)? {
      let (k, v) = entry?;
      set.call::<_, ()>((
        This(out_obj.clone()),
        clone_value(ctx, &k, seen)?,
        clone_value(ctx, &v, seen)?,
      ))?;
    }
    return Ok(out);
  }
  if is_a("Set")? {
    let ctor: rquickjs::function::Constructor<'js> = globals.get("Set")?;
    let out: Value<'js> = ctor.construct(())?;
    seen.push((value.clone(), out.clone()));
    let out_obj = out.as_object().cloned().unwrap_or_else(|| obj.clone());
    let add: rquickjs::Function<'js> = out_obj.get("add")?;
    for entry in iterate_entries(ctx, obj)? {
      let (k, _) = entry?;
      add.call::<_, ()>((This(out_obj.clone()), clone_value(ctx, &k, seen)?))?;
    }
    return Ok(out);
  }

  // Anything with a non-Object prototype (a class instance, including
  // the native web classes) is not a cloneable "plain object".
  let object_ctor: Value<'js> = globals.get("Object")?;
  let proto = obj.get_prototype();
  let object_proto = object_ctor
    .as_object()
    .and_then(|o| o.get::<_, Value<'js>>("prototype").ok())
    .and_then(|v| v.as_object().cloned());
  if proto.is_some() && proto != object_proto {
    return Err(data_clone_error(ctx, "an object that is not a plain object"));
  }

  let out = Object::new(ctx.clone())?;
  seen.push((value.clone(), out.clone().into_value()));
  for key in obj.keys::<String>() {
    let key = key?;
    let v: Value<'js> = obj.get(&key)?;
    out.set(key, clone_value(ctx, &v, seen)?)?;
  }
  Ok(out.into_value())
}

/// `[...target.entries()]` as `(key, value)` pairs — how a `Map`'s
/// contents (and, with the value ignored, a `Set`'s) are read without
/// assuming an internal representation.
#[allow(clippy::type_complexity)]
fn iterate_entries<'js>(
  ctx: &Ctx<'js>,
  target: &Object<'js>,
) -> rquickjs::Result<Vec<rquickjs::Result<(Value<'js>, Value<'js>)>>> {
  let entries: rquickjs::Function<'js> = target.get("entries")?;
  let iter: Value<'js> = entries.call((This(target.clone()),))?;
  let array_ctor: Value<'js> = ctx.globals().get("Array")?;
  let from: rquickjs::Function<'js> = array_ctor
    .as_object()
    .ok_or_else(|| rquickjs::Exception::throw_type(ctx, "Array is not an object"))?
    .get("from")?;
  let list: rquickjs::Array<'js> = from.call((This(array_ctor), iter))?;
  Ok(
    (0..list.len())
      .map(|i| {
        let pair: rquickjs::Array<'js> = list.get(i)?;
        Ok((pair.get(0)?, pair.get(1)?))
      })
      .collect(),
  )
}
