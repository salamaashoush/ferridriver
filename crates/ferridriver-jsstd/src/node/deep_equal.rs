//! Structural equality, in Node's two flavours.
//!
//! One implementation, shared by `util.isDeepStrictEqual` and by
//! `assert.deepEqual` / `assert.deepStrictEqual`.

use rquickjs::{Function, Object, Type, Value, function::This};

/// How leaf values compare.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
  /// `assert.deepEqual`: `==` on primitives, prototypes ignored.
  Loose,
  /// `assert.deepStrictEqual`: `Object.is` on primitives, prototypes must
  /// match.
  Strict,
}

/// Recursion ceiling. A cyclic graph would otherwise never terminate;
/// Node tracks visited pairs instead, which needs identity keys QuickJS
/// does not hand out cheaply.
const MAX_DEPTH: usize = 64;

/// Compare two values the way `assert.deepEqual` / `deepStrictEqual` do.
///
/// # Errors
///
/// Propagates JS-side property reads.
pub fn deep_equal<'js>(a: &Value<'js>, b: &Value<'js>, mode: Mode) -> rquickjs::Result<bool> {
  equal_at(a, b, mode, 0)
}

fn equal_at<'js>(a: &Value<'js>, b: &Value<'js>, mode: Mode, depth: usize) -> rquickjs::Result<bool> {
  if depth > MAX_DEPTH {
    return Ok(false);
  }
  if a.type_of() != b.type_of() {
    // `1 == '1'` under the loose flavour; nothing else crosses types.
    return Ok(mode == Mode::Loose && loose_primitive_eq(a, b));
  }

  match a.type_of() {
    Type::Undefined | Type::Null | Type::Uninitialized => Ok(true),
    Type::Bool => Ok(a.as_bool() == b.as_bool()),
    Type::Int | Type::Float => Ok(number_eq(a, b, mode)),
    Type::String => Ok(js_string(a)? == js_string(b)?),
    Type::Symbol => Ok(a.as_symbol() == b.as_symbol()),
    Type::Array => array_eq(a, b, mode, depth),
    Type::Object | Type::Exception => object_eq(a, b, mode, depth),
    // Functions, constructors and everything else compare by identity.
    _ => Ok(a == b),
  }
}

fn js_string(value: &Value<'_>) -> rquickjs::Result<String> {
  value.as_string().map_or_else(|| Ok(String::new()), rquickjs::String::to_string)
}

/// `Object.is` semantics under Strict (so `NaN` equals itself and `0` does
/// not equal `-0`), `==` semantics under Loose.
fn number_eq(a: &Value<'_>, b: &Value<'_>, mode: Mode) -> bool {
  let (Some(x), Some(y)) = (a.as_number(), b.as_number()) else {
    return false;
  };
  match mode {
    Mode::Strict => {
      if x.is_nan() && y.is_nan() {
        true
      } else {
        x == y && x.is_sign_negative() == y.is_sign_negative()
      }
    },
    Mode::Loose => x == y,
  }
}

/// The only cross-type comparison `assert.deepEqual` performs: a primitive
/// against a primitive through `==`.
fn loose_primitive_eq(a: &Value<'_>, b: &Value<'_>) -> bool {
  match (a.as_number(), b.as_number()) {
    (Some(x), Some(y)) => x == y,
    _ => match (a.as_string(), b.as_string()) {
      (Some(x), Some(y)) => x.to_string().ok() == y.to_string().ok(),
      _ => a.is_null() && b.is_undefined() || a.is_undefined() && b.is_null(),
    },
  }
}

fn array_eq<'js>(a: &Value<'js>, b: &Value<'js>, mode: Mode, depth: usize) -> rquickjs::Result<bool> {
  let (Some(x), Some(y)) = (a.as_array(), b.as_array()) else {
    return Ok(false);
  };
  if x.len() != y.len() {
    return Ok(false);
  }
  for i in 0..x.len() {
    let (lhs, rhs): (Value<'_>, Value<'_>) = (x.get(i)?, y.get(i)?);
    if !equal_at(&lhs, &rhs, mode, depth + 1)? {
      return Ok(false);
    }
  }
  Ok(true)
}

fn object_eq<'js>(a: &Value<'js>, b: &Value<'js>, mode: Mode, depth: usize) -> rquickjs::Result<bool> {
  let (Some(x), Some(y)) = (a.as_object(), b.as_object()) else {
    return Ok(false);
  };

  if mode == Mode::Strict && constructor_name(x)? != constructor_name(y)? {
    return Ok(false);
  }

  // Dates and RegExps carry their state outside their own properties.
  if let (Some(lhs), Some(rhs)) = (value_of_number(x)?, value_of_number(y)?) {
    return Ok(lhs == rhs || (lhs.is_nan() && rhs.is_nan()));
  }
  if let (Some(lhs), Some(rhs)) = (regexp_source(x)?, regexp_source(y)?) {
    return Ok(lhs == rhs);
  }

  let keys: Vec<String> = own_keys(x)?;
  let other: Vec<String> = own_keys(y)?;
  if keys.len() != other.len() {
    return Ok(false);
  }
  for key in keys {
    if !y.contains_key(key.as_str())? {
      return Ok(false);
    }
    let (lhs, rhs): (Value<'_>, Value<'_>) = (x.get(key.as_str())?, y.get(key.as_str())?);
    if !equal_at(&lhs, &rhs, mode, depth + 1)? {
      return Ok(false);
    }
  }
  Ok(true)
}

fn own_keys(object: &Object<'_>) -> rquickjs::Result<Vec<String>> {
  object.keys::<String>().collect::<rquickjs::Result<Vec<String>>>()
}

fn constructor_name(object: &Object<'_>) -> rquickjs::Result<Option<String>> {
  let ctor: Option<Object<'_>> = object.get("constructor").ok();
  match ctor {
    Some(c) => Ok(c.get::<_, String>("name").ok()),
    None => Ok(None),
  }
}

/// `valueOf()` for the wrappers whose identity is a number — `Date`, and the
/// boxed primitives.
fn value_of_number(object: &Object<'_>) -> rquickjs::Result<Option<f64>> {
  if constructor_name(object)?.as_deref() != Some("Date") {
    return Ok(None);
  }
  let value_of: Function<'_> = object.get("valueOf")?;
  let millis: f64 = value_of.call((This(object.clone()),))?;
  Ok(Some(millis))
}

fn regexp_source(object: &Object<'_>) -> rquickjs::Result<Option<String>> {
  if constructor_name(object)?.as_deref() != Some("RegExp") {
    return Ok(None);
  }
  let source: String = object.get("source")?;
  let flags: String = object.get("flags").unwrap_or_default();
  Ok(Some(format!("/{source}/{flags}")))
}
