//! Asymmetric matchers (`expect.any`, `expect.objectContaining`, ...).
//! Wire-encoded as a tagged JSON object so the script layer can produce
//! them in JS and have them round-trip through `serde_json::Value`.

use regex::Regex;
use serde_json::{Map, Value};

use crate::value::StringOrRegex;

/// Wire-protocol marker key shared with the QuickJS binding.
pub const ASYM_TAG_KEY: &str = "@@asym";

#[derive(Debug, Clone)]
pub enum Asymmetric {
  Anything,
  Any(TypeTag),
  ArrayContaining(Vec<Value>),
  ObjectContaining(Map<String, Value>),
  StringContaining(String),
  StringMatching(StringOrRegex),
  CloseTo {
    value: f64,
    digits: u8,
  },
  /// `expect.arrayOf(sample)` — an array whose EVERY item matches.
  ArrayOf(Box<Value>),
  /// A matcher registered through `expect.extend`, used as an expected
  /// value. Core cannot run it: the body belongs to the host, which
  /// installs an evaluator for the duration of the comparison.
  Custom {
    name: String,
    args: Vec<Value>,
  },
  Not(Box<Asymmetric>),
}

/// Runs a host-registered matcher during a structural comparison.
pub trait CustomAsymmetric {
  /// `expect.<name>(...args)` as an expected value: does `actual` match?
  fn matches(&self, name: &str, args: &[Value], actual: &Value) -> bool;
}

/// The evaluator a comparison carries, if the caller has one. Passed
/// explicitly rather than kept ambient: it borrows the host's context,
/// and a comparison must never outlive it.
pub type Evaluator<'a> = Option<&'a dyn CustomAsymmetric>;

#[derive(Debug, Clone)]
pub enum TypeTag {
  String,
  Number,
  Boolean,
  Object,
  Array,
  Function,
  Null,
  Custom(String),
}

impl TypeTag {
  pub fn from_name(name: &str) -> Self {
    match name {
      "String" => Self::String,
      "Number" => Self::Number,
      "Boolean" => Self::Boolean,
      "Object" => Self::Object,
      "Array" => Self::Array,
      "Function" => Self::Function,
      _ => Self::Custom(name.to_string()),
    }
  }

  pub fn matches_value(&self, v: &Value) -> bool {
    match self {
      Self::String => v.is_string(),
      Self::Number => v.is_number(),
      Self::Boolean => v.is_boolean(),
      Self::Object => v.is_object(),
      Self::Array => v.is_array(),
      // Functions don't survive JSON serialization.
      Self::Function => false,
      Self::Null => v.is_null(),
      Self::Custom(_) => v.is_object(),
    }
  }

  pub fn description(&self) -> String {
    match self {
      Self::String => "Any<String>".into(),
      Self::Number => "Any<Number>".into(),
      Self::Boolean => "Any<Boolean>".into(),
      Self::Object => "Any<Object>".into(),
      Self::Array => "Any<Array>".into(),
      Self::Function => "Any<Function>".into(),
      Self::Null => "Any<Null>".into(),
      Self::Custom(n) => format!("Any<{n}>"),
    }
  }
}

impl Asymmetric {
  /// Decode from a tagged JSON object. Returns `None` for any value
  /// that is not a properly tagged asymmetric.
  pub fn from_value(v: &Value) -> Option<Self> {
    let obj = v.as_object()?;
    let tag = obj.get(ASYM_TAG_KEY)?.as_str()?;
    match tag {
      "anything" => Some(Self::Anything),
      "any" => {
        let name = obj.get("name").and_then(Value::as_str).unwrap_or("Object");
        Some(Self::Any(TypeTag::from_name(name)))
      },
      "arrayContaining" => {
        let arr = obj.get("items")?.as_array()?.clone();
        Some(Self::ArrayContaining(arr))
      },
      "objectContaining" => {
        let map = obj.get("subset")?.as_object()?.clone();
        Some(Self::ObjectContaining(map))
      },
      "stringContaining" => {
        let s = obj.get("substring")?.as_str()?.to_string();
        Some(Self::StringContaining(s))
      },
      "stringMatching" => {
        if let Some(s) = obj.get("substring").and_then(Value::as_str) {
          Some(Self::StringMatching(StringOrRegex::String(s.to_string())))
        } else {
          let pat = obj.get("regex")?.as_str()?;
          let flags = obj.get("flags").and_then(Value::as_str).unwrap_or("");
          let re = compile_js_regex(pat, flags).ok()?;
          Some(Self::StringMatching(StringOrRegex::Regex(re)))
        }
      },
      "closeTo" => {
        let value = obj.get("value")?.as_f64()?;
        let digits = obj.get("digits").and_then(Value::as_u64).unwrap_or(2);
        Some(Self::CloseTo {
          value,
          digits: digits as u8,
        })
      },
      "arrayOf" => Some(Self::ArrayOf(Box::new(obj.get("sample")?.clone()))),
      "custom" => Some(Self::Custom {
        name: obj.get("name")?.as_str()?.to_string(),
        args: obj.get("args").and_then(Value::as_array).cloned().unwrap_or_default(),
      }),
      "not" => {
        let inner = obj.get("inner")?;
        let inner = Self::from_value(inner)?;
        Some(Self::Not(Box::new(inner)))
      },
      _ => None,
    }
  }

  /// Match without a host: a custom matcher cannot run, so it never
  /// matches (the host that registered it is not the one comparing).
  pub fn matches(&self, actual: &Value) -> bool {
    self.matches_with(actual, None)
  }

  pub fn matches_with(&self, actual: &Value, ev: Evaluator<'_>) -> bool {
    match self {
      Self::Anything => !actual.is_null(),
      Self::Any(tag) => tag.matches_value(actual),
      Self::ArrayContaining(items) => {
        let Some(arr) = actual.as_array() else { return false };
        items
          .iter()
          .all(|expected| arr.iter().any(|act| deep_equal_with(act, expected, ev)))
      },
      Self::ObjectContaining(subset) => {
        let Some(obj) = actual.as_object() else { return false };
        subset.iter().all(|(k, expected)| match obj.get(k) {
          Some(act) => deep_equal_with(act, expected, ev),
          None => false,
        })
      },
      Self::StringContaining(needle) => actual.as_str().is_some_and(|s| s.contains(needle.as_str())),
      Self::StringMatching(pat) => actual.as_str().is_some_and(|s| pat.matches(s)),
      Self::CloseTo { value, digits } => actual.as_f64().is_some_and(|a| close_enough(a, *value, *digits)),
      Self::ArrayOf(sample) => actual
        .as_array()
        .is_some_and(|arr| arr.iter().all(|item| deep_equal_with(item, sample, ev))),
      Self::Custom { name, args } => ev.is_some_and(|e| e.matches(name, args, actual)),
      Self::Not(inner) => !inner.matches_with(actual, ev),
    }
  }

  pub fn description(&self) -> String {
    match self {
      Self::Anything => "Anything".into(),
      Self::Any(tag) => tag.description(),
      Self::ArrayContaining(items) => format!("ArrayContaining({})", json_short(&Value::Array(items.clone()))),
      Self::ObjectContaining(map) => format!("ObjectContaining({})", json_short(&Value::Object(map.clone()))),
      Self::StringContaining(s) => format!("StringContaining({s:?})"),
      Self::StringMatching(p) => format!("StringMatching({})", p.description()),
      Self::CloseTo { value, digits } => format!("CloseTo({value}, {digits})"),
      Self::ArrayOf(sample) => format!("ArrayOf({})", json_short(sample)),
      Self::Custom { name, args } => format!("{name}({})", json_short(&Value::Array(args.clone()))),
      Self::Not(inner) => format!("Not({})", inner.description()),
    }
  }
}

/// Compile a JS-style regex (pattern + flag string).
pub fn compile_js_regex(pattern: &str, flags: &str) -> Result<Regex, regex::Error> {
  let mut prefix = String::new();
  if !flags.is_empty() {
    let mut letters = String::new();
    for c in flags.chars() {
      match c {
        'i' => letters.push('i'),
        'm' => letters.push('m'),
        's' => letters.push('s'),
        'x' => letters.push('x'),
        // 'g' / 'u' / 'y' have no Rust regex equivalent at the inline
        // flag level; ignored silently so a user-supplied global flag
        // does not break matching.
        _ => {},
      }
    }
    if !letters.is_empty() {
      prefix = format!("(?{letters})");
    }
  }
  Regex::new(&format!("{prefix}{pattern}"))
}

pub fn close_enough(a: f64, b: f64, digits: u8) -> bool {
  if !a.is_finite() || !b.is_finite() {
    return float_bit_eq(a, b);
  }
  let tol = 10f64.powi(-i32::from(digits)) / 2.0;
  (a - b).abs() < tol
}

/// `Object.is`-style equality: NaN equals itself, `+0 != -0`. Used to
/// sidestep `clippy::float_cmp` without losing the Jest semantic.
pub fn float_bit_eq(a: f64, b: f64) -> bool {
  a.to_bits() == b.to_bits()
}

pub fn json_short(v: &Value) -> String {
  let s = v.to_string();
  if s.len() > 80 { format!("{}…", &s[..80]) } else { s }
}

/// Deep equality treating any [`Asymmetric`] embedded in `expected` as
/// a matcher rather than a literal.
pub fn deep_equal(actual: &Value, expected: &Value) -> bool {
  deep_equal_with(actual, expected, None)
}

/// [`deep_equal`] with a host that can run custom asymmetric matchers.
pub fn deep_equal_with(actual: &Value, expected: &Value, ev: Evaluator<'_>) -> bool {
  if let Some(asym) = Asymmetric::from_value(expected) {
    return asym.matches_with(actual, ev);
  }
  match (actual, expected) {
    (Value::Null, Value::Null) => true,
    (Value::Bool(a), Value::Bool(b)) => a == b,
    (Value::Number(a), Value::Number(b)) => match (a.as_f64(), b.as_f64()) {
      (Some(x), Some(y)) => float_bit_eq(x, y) || (x.is_nan() && y.is_nan()),
      _ => false,
    },
    (Value::String(a), Value::String(b)) => a == b,
    (Value::Array(a), Value::Array(b)) => {
      a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| deep_equal_with(x, y, ev))
    },
    (Value::Object(a), Value::Object(b)) => {
      a.len() == b.len()
        && a
          .iter()
          .all(|(k, v)| b.get(k).is_some_and(|other| deep_equal_with(v, other, ev)))
    },
    _ => false,
  }
}

/// Subset equality (Jest's `toMatchObject`).
pub fn match_object(actual: &Value, subset: &Value) -> bool {
  match_object_with(actual, subset, None)
}

/// [`match_object`] with a host that can run custom asymmetric matchers.
pub fn match_object_with(actual: &Value, subset: &Value, ev: Evaluator<'_>) -> bool {
  if let Some(asym) = Asymmetric::from_value(subset) {
    return asym.matches_with(actual, ev);
  }
  match (actual, subset) {
    (Value::Object(a), Value::Object(b)) => b.iter().all(|(k, expected)| match a.get(k) {
      Some(act) => match_object_with(act, expected, ev),
      None => false,
    }),
    (Value::Array(a), Value::Array(b)) => {
      b.len() == a.len() && a.iter().zip(b.iter()).all(|(x, y)| match_object_with(x, y, ev))
    },
    _ => deep_equal_with(actual, subset, ev),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::json;

  #[test]
  fn deep_equal_primitives() {
    assert!(deep_equal(&json!(1), &json!(1)));
    assert!(deep_equal(&json!("x"), &json!("x")));
    assert!(deep_equal(&Value::Null, &Value::Null));
    assert!(!deep_equal(&json!(1), &json!("1")));
  }

  #[test]
  fn deep_equal_nested() {
    assert!(deep_equal(&json!({"a": [1, 2]}), &json!({"a": [1, 2]})));
    assert!(!deep_equal(&json!({"a": [1, 2]}), &json!({"a": [1, 3]})));
  }

  #[test]
  fn deep_equal_object_key_order_irrelevant() {
    assert!(deep_equal(&json!({"a": 1, "b": 2}), &json!({"b": 2, "a": 1})));
  }

  #[test]
  fn deep_equal_nan_self_equality() {
    let nan = json!(f64::NAN);
    assert!(deep_equal(&nan, &nan));
  }

  #[test]
  fn asymmetric_any_string() {
    let exp = json!({ASYM_TAG_KEY: "any", "name": "String"});
    let asym = Asymmetric::from_value(&exp).unwrap();
    assert!(asym.matches(&json!("hi")));
    assert!(!asym.matches(&json!(42)));
  }

  #[test]
  fn asymmetric_object_containing_inside_array() {
    let exp = json!([{ASYM_TAG_KEY: "objectContaining", "subset": {"id": 1}}]);
    assert!(deep_equal(&json!([{"id": 1, "name": "n"}]), &exp));
    assert!(!deep_equal(&json!([{"id": 2}]), &exp));
  }

  #[test]
  fn asymmetric_array_of() {
    let exp = json!({ASYM_TAG_KEY: "arrayOf", "sample": {ASYM_TAG_KEY: "any", "name": "Number"}});
    let asym = Asymmetric::from_value(&exp).unwrap();
    assert!(asym.matches(&json!([1, 2, 3])));
    assert!(!asym.matches(&json!([1, "two"])));
    assert!(!asym.matches(&json!("not an array")));
    // An empty array vacuously satisfies it, as upstream.
    assert!(asym.matches(&json!([])));
  }

  #[test]
  fn a_custom_asymmetric_needs_its_host() {
    struct EvenOnly;
    impl CustomAsymmetric for EvenOnly {
      fn matches(&self, name: &str, args: &[Value], actual: &Value) -> bool {
        assert_eq!(name, "toBeEven");
        assert!(args.is_empty());
        actual.as_i64().is_some_and(|n| n % 2 == 0)
      }
    }
    let exp = json!({ASYM_TAG_KEY: "custom", "name": "toBeEven", "args": []});
    // With no evaluator the matcher cannot run — and must not panic.
    assert!(!deep_equal(&json!(2), &exp));
    let ev = EvenOnly;
    let ev: Evaluator<'_> = Some(&ev);
    assert!(deep_equal_with(&json!(2), &exp, ev));
    assert!(!deep_equal_with(&json!(3), &exp, ev));
    // And it composes inside a structure, and under `not`.
    assert!(deep_equal_with(&json!({"n": 2}), &json!({"n": exp.clone()}), ev));
    let inverted = json!({ASYM_TAG_KEY: "not", "inner": exp.clone()});
    assert!(deep_equal_with(&json!(3), &inverted, ev));
  }

  #[test]
  fn asymmetric_array_containing() {
    let exp = json!({ASYM_TAG_KEY: "arrayContaining", "items": [2, 3]});
    let asym = Asymmetric::from_value(&exp).unwrap();
    assert!(asym.matches(&json!([1, 2, 3])));
    assert!(!asym.matches(&json!([1, 2])));
  }

  #[test]
  fn asymmetric_not_wraps_inner() {
    let inner = json!({ASYM_TAG_KEY: "any", "name": "String"});
    let exp = json!({ASYM_TAG_KEY: "not", "inner": inner});
    let asym = Asymmetric::from_value(&exp).unwrap();
    assert!(!asym.matches(&json!("x")));
    assert!(asym.matches(&json!(1)));
  }

  #[test]
  fn asymmetric_string_matching_regex() {
    let exp = json!({ASYM_TAG_KEY: "stringMatching", "regex": "hello\\s+world", "flags": "i"});
    let asym = Asymmetric::from_value(&exp).unwrap();
    assert!(asym.matches(&json!("Hello World")));
    assert!(!asym.matches(&json!("bye")));
  }

  #[test]
  fn asymmetric_close_to() {
    let exp = json!({ASYM_TAG_KEY: "closeTo", "value": 0.3, "digits": 2});
    let asym = Asymmetric::from_value(&exp).unwrap();
    assert!(asym.matches(&json!(0.1 + 0.2)));
    assert!(!asym.matches(&json!(0.5)));
  }

  #[test]
  fn match_object_subset() {
    assert!(match_object(&json!({"a": 1, "b": 2}), &json!({"a": 1})));
    assert!(!match_object(&json!({"a": 1, "b": 2}), &json!({"a": 2})));
  }

  #[test]
  fn match_object_with_nested_asym() {
    let subset = json!({"id": {ASYM_TAG_KEY: "any", "name": "Number"}});
    assert!(match_object(&json!({"id": 7, "name": "n"}), &subset));
    assert!(!match_object(&json!({"id": "x"}), &subset));
  }
}
