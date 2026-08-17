//! The one deep-equality engine, over live values.
//!
//! Ports jest's `equals` (the function Playwright's `toEqual` /
//! `toStrictEqual` / `toMatchObject` / `toContainEqual` /
//! `toHaveProperty` all call) onto [`LiveValue`], so a host that can
//! describe its values gets Playwright's equality rather than an
//! approximation of them:
//!
//! - `toEqual` is `equals(a, b, [iterableEquality])` — non-strict, so a
//!   key whose value is `undefined` counts as absent on BOTH sides.
//! - `toStrictEqual` adds `typeEquality` and `sparseArrayEquality` and
//!   sets strict, so `{a: undefined}` is not `{}`, an array hole is not
//!   an `undefined` element, and a class instance is not a literal with
//!   the same fields.
//! - `toMatchObject` adds `subsetEquality`: the expected side only has
//!   to be a subset, recursively.
//!
//! `serde_json::Value` implements [`LiveValue`] too, so a Rust caller
//! gets the same engine — degrading exactly where JSON has nothing to
//! say (no `undefined`, no `Map`, no identity).

use serde_json::Value as JsonValue;

use crate::asymmetric::Evaluator;
use crate::subject::{JsType, LiveValue, Shape};

/// Which of jest's tester sets to apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
  /// `toEqual`: undefined-valued keys are absent, holes equal
  /// `undefined`, and the constructor is not compared.
  Loose,
  /// `toStrictEqual`.
  Strict,
  /// `toMatchObject`: the expected side is a subset.
  Subset,
}

impl Mode {
  fn strict(self) -> bool {
    self == Self::Strict
  }

  fn subset(self) -> bool {
    self == Self::Subset
  }
}

/// Deep equality of two live values.
pub fn equals<V: LiveValue>(actual: &V, expected: &V, mode: Mode, ev: Evaluator<'_>) -> Result<bool, V::Error> {
  let mut seen = Vec::new();
  compare(actual, expected, mode, ev, &mut seen)
}

fn compare<V: LiveValue>(
  actual: &V,
  expected: &V,
  mode: Mode,
  ev: Evaluator<'_>,
  seen: &mut Vec<(u64, u64)>,
) -> Result<bool, V::Error> {
  // An asymmetric matcher on either side decides on its own, before any
  // structural rule — that is what makes `expect.any(Number)` usable
  // anywhere a value can appear.
  if let Some(asym) = expected.asymmetric() {
    return asym.matches_live(actual, ev);
  }
  if let Some(asym) = actual.asymmetric() {
    return asym.matches_live(expected, ev);
  }

  // Same reference: equal, and the only way a cycle terminates.
  if let (Some(a), Some(b)) = (actual.ref_id(), expected.ref_id()) {
    if a == b {
      return Ok(true);
    }
    if seen.contains(&(a, b)) {
      // Already comparing this pair further up the walk; assume equal
      // and let the rest of the structure decide, as jest does.
      return Ok(true);
    }
    seen.push((a, b));
  }
  let out = compare_uncycled(actual, expected, mode, ev, seen);
  if let (Some(a), Some(b)) = (actual.ref_id(), expected.ref_id()) {
    seen.retain(|pair| *pair != (a, b));
  }
  out
}

fn compare_uncycled<V: LiveValue>(
  actual: &V,
  expected: &V,
  mode: Mode,
  ev: Evaluator<'_>,
  seen: &mut Vec<(u64, u64)>,
) -> Result<bool, V::Error> {
  let (at, et) = (actual.js_type(), expected.js_type());
  if at != et {
    return Ok(false);
  }
  if !matches!(
    at,
    JsType::Object | JsType::Array | JsType::Function | JsType::Symbol | JsType::BigInt
  ) {
    // Primitives: `Object.is`, which is what jest's `equals` bottoms out
    // in (so `NaN` equals itself and `0` does not equal `-0`).
    return actual.same_value(expected);
  }

  let (a_shape, e_shape) = (actual.shape()?, expected.shape()?);

  // `toStrictEqual`'s typeEquality: a class instance is not a literal
  // with the same fields.
  if mode.strict() && actual.class_name() != expected.class_name() {
    return Ok(false);
  }

  match (a_shape, e_shape) {
    (Shape::Primitive, Shape::Primitive) => actual.same_value(expected),
    (Shape::Function, Shape::Function) => actual.same_value(expected),
    // Two dates are equal when they name the same instant; two invalid
    // dates are equal to each other, as `getTime()` NaNs compare in jest.
    (Shape::Date(a), Shape::Date(b)) => Ok(crate::asymmetric::float_bit_eq(a, b) || (a.is_nan() && b.is_nan())),
    (
      Shape::RegExp {
        source: a_src,
        flags: a_flags,
      },
      Shape::RegExp {
        source: b_src,
        flags: b_flags,
      },
    ) => Ok(a_src == b_src && a_flags == b_flags),
    (
      Shape::Error {
        name: a_name,
        message: a_msg,
      },
      Shape::Error {
        name: b_name,
        message: b_msg,
      },
    ) => Ok(a_name == b_name && a_msg == b_msg),
    (Shape::Bytes(a), Shape::Bytes(b)) => Ok(a == b),
    (Shape::Array(a), Shape::Array(b)) => compare_arrays(&a, &b, mode, ev, seen),
    (Shape::Object(a), Shape::Object(b)) => compare_objects(&a, &b, mode, ev, seen),
    (Shape::Map(a), Shape::Map(b)) => compare_maps(&a, &b, mode, ev, seen),
    (Shape::Set(a), Shape::Set(b)) => compare_sets(&a, &b, mode, ev, seen),
    // Different shapes of the same `typeof` — a Map is not a plain
    // object, a Date is not an Error.
    _ => Ok(false),
  }
}

fn compare_arrays<V: LiveValue>(
  a: &[Option<V>],
  b: &[Option<V>],
  mode: Mode,
  ev: Evaluator<'_>,
  seen: &mut Vec<(u64, u64)>,
) -> Result<bool, V::Error> {
  if mode.subset() {
    // `toMatchObject` compares arrays element-wise and requires the same
    // length, but each element only as a subset.
    if a.len() != b.len() {
      return Ok(false);
    }
  } else if a.len() != b.len() {
    return Ok(false);
  }
  for (x, y) in a.iter().zip(b.iter()) {
    match (x, y) {
      (Some(x), Some(y)) => {
        if !compare(x, y, mode, ev, seen)? {
          return Ok(false);
        }
      },
      // A hole. Strict mode (sparseArrayEquality) says a hole is not an
      // `undefined` element; loose mode treats them alike.
      (None, None) => {},
      (None, Some(v)) | (Some(v), None) => {
        if mode.strict() || v.js_type() != JsType::Undefined {
          return Ok(false);
        }
      },
    }
  }
  Ok(true)
}

fn compare_objects<V: LiveValue>(
  a: &[(String, V)],
  b: &[(String, V)],
  mode: Mode,
  ev: Evaluator<'_>,
  seen: &mut Vec<(u64, u64)>,
) -> Result<bool, V::Error> {
  // jest's `hasKey` vs `hasDefinedKey`: outside strict mode a key whose
  // value is `undefined` counts as absent, on both sides.
  let counts = |entries: &[(String, V)]| -> usize {
    entries
      .iter()
      .filter(|(_, v)| mode.strict() || v.js_type() != JsType::Undefined)
      .count()
  };
  if !mode.subset() && counts(a) != counts(b) {
    return Ok(false);
  }
  for (key, want) in b {
    if !mode.strict() && want.js_type() == JsType::Undefined {
      // Absent on the expected side; the actual side must not define it
      // either, which the count check above already settled for the
      // non-subset modes.
      if mode.subset()
        && let Some((_, got)) = a.iter().find(|(k, _)| k == key)
        && got.js_type() != JsType::Undefined
      {
        return Ok(false);
      }
      continue;
    }
    let Some((_, got)) = a.iter().find(|(k, _)| k == key) else {
      return Ok(false);
    };
    if !compare(got, want, mode, ev, seen)? {
      return Ok(false);
    }
  }
  Ok(true)
}

/// `iterableEquality` for maps: same size, and every entry has a partner
/// — by key equality first, falling back to a scan, since a key may
/// itself be a structure.
fn compare_maps<V: LiveValue>(
  a: &[(V, V)],
  b: &[(V, V)],
  mode: Mode,
  ev: Evaluator<'_>,
  seen: &mut Vec<(u64, u64)>,
) -> Result<bool, V::Error> {
  if a.len() != b.len() {
    return Ok(false);
  }
  let mut taken = vec![false; a.len()];
  for (want_key, want_value) in b {
    let mut matched = false;
    for (i, (got_key, got_value)) in a.iter().enumerate() {
      if taken[i] {
        continue;
      }
      if compare(got_key, want_key, mode, ev, seen)? && compare(got_value, want_value, mode, ev, seen)? {
        taken[i] = true;
        matched = true;
        break;
      }
    }
    if !matched {
      return Ok(false);
    }
  }
  Ok(true)
}

fn compare_sets<V: LiveValue>(
  a: &[V],
  b: &[V],
  mode: Mode,
  ev: Evaluator<'_>,
  seen: &mut Vec<(u64, u64)>,
) -> Result<bool, V::Error> {
  if a.len() != b.len() {
    return Ok(false);
  }
  let mut taken = vec![false; a.len()];
  for want in b {
    let mut matched = false;
    for (i, got) in a.iter().enumerate() {
      if taken[i] {
        continue;
      }
      if compare(got, want, mode, ev, seen)? {
        taken[i] = true;
        matched = true;
        break;
      }
    }
    if !matched {
      return Ok(false);
    }
  }
  Ok(true)
}

// ── serde_json, so a Rust caller runs the same engine ────────────────

impl LiveValue for JsonValue {
  /// JSON cannot fail to describe itself.
  type Error = std::convert::Infallible;

  fn js_type(&self) -> JsType {
    match self {
      Self::Null => JsType::Null,
      Self::Bool(_) => JsType::Boolean,
      Self::Number(_) => JsType::Number,
      Self::String(_) => JsType::String,
      Self::Array(_) => JsType::Array,
      Self::Object(_) => JsType::Object,
    }
  }

  fn same_value(&self, other: &Self) -> Result<bool, Self::Error> {
    Ok(match (self, other) {
      (Self::Number(a), Self::Number(b)) => match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => crate::asymmetric::float_bit_eq(x, y) || (x.is_nan() && y.is_nan()),
        _ => false,
      },
      // Structures have no identity in JSON; `equals` never reaches here
      // for them, and `toBe` on a Rust value is documented as structural.
      (a, b) => a == b,
    })
  }

  fn structurally_equal(&self, other: &Self) -> bool {
    crate::asymmetric::deep_equal(self, other)
  }

  fn truthy(&self) -> bool {
    match self {
      Self::Null => false,
      Self::Bool(b) => *b,
      Self::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
      Self::String(s) => !s.is_empty(),
      _ => true,
    }
  }

  fn number(&self) -> Option<f64> {
    self.as_f64()
  }

  fn text(&self) -> Option<String> {
    self.as_str().map(ToString::to_string)
  }

  fn length(&self) -> Result<Option<f64>, Self::Error> {
    Ok(match self {
      Self::Array(a) => Some(a.len() as f64),
      Self::String(s) => Some(s.encode_utf16().count() as f64),
      _ => None,
    })
  }

  fn spread(&self) -> Result<Option<Vec<Self>>, Self::Error> {
    Ok(match self {
      Self::Array(a) => Some(a.clone()),
      _ => None,
    })
  }

  fn instance_of(&self, _ctor: &Self) -> Result<bool, Self::Error> {
    // JSON has no constructors to test against.
    Ok(false)
  }

  fn describe(&self) -> String {
    crate::asymmetric::json_short(self)
  }

  fn shape(&self) -> Result<Shape<Self>, Self::Error> {
    Ok(match self {
      Self::Array(items) => Shape::Array(items.iter().cloned().map(Some).collect()),
      Self::Object(map) => Shape::Object(map.iter().map(|(k, v)| (k.clone(), v.clone())).collect()),
      _ => Shape::Primitive,
    })
  }

  fn class_name(&self) -> Option<String> {
    match self {
      Self::Array(_) => Some("Array".into()),
      Self::Object(_) => Some("Object".into()),
      _ => None,
    }
  }

  fn ref_id(&self) -> Option<u64> {
    None
  }

  fn as_json(&self) -> Option<JsonValue> {
    Some(self.clone())
  }
}

#[cfg(test)]
mod tests {
  use serde_json::json;

  use super::*;
  use crate::asymmetric::ASYM_TAG_KEY;

  fn eq(a: &JsonValue, b: &JsonValue, mode: Mode) -> bool {
    equals(a, b, mode, None).expect("json cannot fail")
  }

  #[test]
  fn json_values_compare_as_they_always_did() {
    assert!(eq(&json!({"a": [1, 2]}), &json!({"a": [1, 2]}), Mode::Loose));
    assert!(!eq(&json!({"a": 1}), &json!({"a": 2}), Mode::Loose));
    assert!(!eq(&json!({"a": 1}), &json!({"a": 1, "b": 2}), Mode::Loose));
    assert!(!eq(&json!([1, 2]), &json!([1, 2, 3]), Mode::Loose));
    assert!(!eq(&json!(null), &json!(0), Mode::Loose));
    // serde_json has no NaN — it serializes to null, and both sides
    // agree on that, which is the JSON view's honest answer.
    assert!(eq(&json!(f64::NAN), &json!(null), Mode::Loose));
    // Key order is irrelevant.
    assert!(eq(&json!({"a": 1, "b": 2}), &json!({"b": 2, "a": 1}), Mode::Loose));
  }

  #[test]
  fn subset_mode_is_to_match_object() {
    assert!(eq(&json!({"a": 1, "b": 2}), &json!({"a": 1}), Mode::Subset));
    assert!(!eq(&json!({"a": 1}), &json!({"a": 1, "b": 2}), Mode::Subset));
    assert!(eq(
      &json!({"a": {"b": {"c": 1, "d": 2}}}),
      &json!({"a": {"b": {"c": 1}}}),
      Mode::Subset
    ));
  }

  #[test]
  fn an_asymmetric_matcher_decides_wherever_it_appears() {
    let any_number = json!({ASYM_TAG_KEY: "any", "name": "Number"});
    assert!(eq(&json!({"id": 7}), &json!({"id": any_number.clone()}), Mode::Loose));
    assert!(!eq(&json!({"id": "7"}), &json!({"id": any_number}), Mode::Loose));
  }

  #[test]
  fn strict_mode_compares_the_class() {
    // JSON has only Array and Object, but the rule still holds between
    // them, and loose mode already refuses a type mismatch.
    assert!(!eq(&json!([]), &json!({}), Mode::Strict));
    assert!(eq(&json!({"a": 1}), &json!({"a": 1}), Mode::Strict));
  }

  #[test]
  fn deep_equal_still_agrees_with_the_engine() {
    for (a, b) in [
      (json!(1), json!(1)),
      (json!("x"), json!("x")),
      (json!([1, {"a": 2}]), json!([1, {"a": 2}])),
      (json!({"a": null}), json!({"a": null})),
      (json!(1), json!(2)),
      (json!([1]), json!([1, 2])),
    ] {
      assert_eq!(
        crate::asymmetric::deep_equal(&a, &b),
        eq(&a, &b, Mode::Loose),
        "deep_equal and the engine disagree on {a} vs {b}"
      );
    }
  }
}
