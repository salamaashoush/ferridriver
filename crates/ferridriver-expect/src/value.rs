//! Synchronous value matchers (Jest-style).

use std::panic::Location;

use regex::Regex;
use serde_json::Value;

use crate::asymmetric::{deep_equal, float_bit_eq, json_short, match_object};
use crate::diff::json_diff;
use crate::{AssertionFailure, CallerLocation};

/// A match that supports both exact string equality and regex.
///
/// Note: web-first matchers in `ferridriver-test` use this with
/// *exact-equality* semantics for the `String` variant (matching
/// Playwright). The Jest-style [`ExpectValue::to_match`] downgrades the
/// `String` variant to *substring containment* explicitly.
#[derive(Debug, Clone)]
pub enum StringOrRegex {
  String(String),
  Regex(Regex),
}

impl StringOrRegex {
  pub fn matches(&self, actual: &str) -> bool {
    match self {
      Self::String(expected) => actual == expected,
      Self::Regex(re) => re.is_match(actual),
    }
  }

  pub fn description(&self) -> String {
    match self {
      Self::String(s) => format!("\"{s}\""),
      Self::Regex(re) => format!("/{}/", re.as_str()),
    }
  }
}

impl From<&str> for StringOrRegex {
  fn from(s: &str) -> Self {
    Self::String(s.to_string())
  }
}

impl From<String> for StringOrRegex {
  fn from(s: String) -> Self {
    Self::String(s)
  }
}

impl From<Regex> for StringOrRegex {
  fn from(re: Regex) -> Self {
    Self::Regex(re)
  }
}

// ── ExpectValue ──────────────────────────────────────────────────────

pub struct ExpectValue {
  actual: Value,
  is_not: bool,
  is_soft: bool,
  message: Option<String>,
}

/// Wrap a value for synchronous assertion.
#[must_use]
pub fn expect_value(actual: Value) -> ExpectValue {
  ExpectValue {
    actual,
    is_not: false,
    is_soft: false,
    message: None,
  }
}

impl ExpectValue {
  #[must_use]
  pub fn not(mut self) -> Self {
    self.is_not = !self.is_not;
    self
  }

  #[must_use]
  pub fn soft(mut self) -> Self {
    self.is_soft = true;
    self
  }

  #[must_use]
  pub fn with_message(mut self, message: impl Into<String>) -> Self {
    self.message = Some(message.into());
    self
  }

  pub fn is_soft(&self) -> bool {
    self.is_soft
  }

  pub fn actual(&self) -> &Value {
    &self.actual
  }

  fn fail(
    &self,
    method: &str,
    expected: impl Into<String>,
    received: impl Into<String>,
    rich_diff: Option<String>,
    location: Option<&'static Location<'static>>,
  ) -> AssertionFailure {
    let expected = expected.into();
    let received = received.into();
    let not = if self.is_not { ".not" } else { "" };
    let prefix = self
      .message
      .as_ref()
      .map(|m| format!("{m}: "))
      .unwrap_or_default();
    // Two-field split:
    //   `message` = a single-line title that a reporter can highlight
    //               on its own (`expect(value).toEqual() failed`).
    //   `diff`    = the full body (Expected/Received + optional unified
    //               diff) — printed below the title.
    // JS-throw concatenates the two; reporters print them in sequence.
    let message = format!("{prefix}expect(value){not}.{method}() failed");
    let summary_diff = format!("Expected: {expected}\nReceived: {received}");
    let body = match rich_diff {
      Some(d) => format!("{summary_diff}\n\nDiff:\n{d}"),
      None => summary_diff,
    };
    let mut failure = AssertionFailure::new(message, Some(body));
    if let Some(loc) = location {
      failure = failure.with_location(CallerLocation::from_std(loc));
    }
    failure
  }

  #[track_caller]
  fn check(
    &self,
    pass: bool,
    method: &str,
    expected: impl Into<String>,
    received: impl Into<String>,
  ) -> Result<(), AssertionFailure> {
    let pass = if self.is_not { !pass } else { pass };
    if pass {
      Ok(())
    } else {
      Err(self.fail(method, expected, received, None, Some(Location::caller())))
    }
  }

  #[track_caller]
  fn check_with_diff(
    &self,
    pass: bool,
    method: &str,
    expected: impl Into<String>,
    received: impl Into<String>,
    diff: String,
  ) -> Result<(), AssertionFailure> {
    let pass = if self.is_not { !pass } else { pass };
    if pass {
      Ok(())
    } else {
      Err(self.fail(method, expected, received, Some(diff), Some(Location::caller())))
    }
  }

  // ── primitive equality ──────────────────────────────────────────

  /// `toBe(expected)` — strict-equality of primitives. Object/array
  /// values are rejected with guidance to use `toEqual`.
  #[track_caller]
  pub fn to_be(&self, expected: &Value) -> Result<(), AssertionFailure> {
    if self.actual.is_object() || self.actual.is_array() || expected.is_object() || expected.is_array() {
      return Err(self.fail(
        "toBe",
        format!("primitive equal to {}", json_short(expected)),
        format!("{} (use toEqual for objects/arrays)", json_short(&self.actual)),
        None,
        Some(Location::caller()),
      ));
    }
    let pass = deep_equal(&self.actual, expected);
    self.check(pass, "toBe", json_short(expected), json_short(&self.actual))
  }

  /// `toEqual(expected)` — recursive equality. Honours asymmetric
  /// matchers embedded in `expected`.
  #[track_caller]
  pub fn to_equal(&self, expected: &Value) -> Result<(), AssertionFailure> {
    let pass = deep_equal(&self.actual, expected);
    let diff = if pass {
      None
    } else {
      Some(json_diff(expected, &self.actual))
    };
    match diff {
      Some(d) => self.check_with_diff(pass, "toEqual", json_short(expected), json_short(&self.actual), d),
      None => self.check(pass, "toEqual", json_short(expected), json_short(&self.actual)),
    }
  }

  /// `toStrictEqual(expected)` — alias of [`Self::to_equal`] for
  /// `serde_json::Value` (no `undefined` to differentiate).
  #[track_caller]
  pub fn to_strict_equal(&self, expected: &Value) -> Result<(), AssertionFailure> {
    let pass = deep_equal(&self.actual, expected);
    let diff = if pass {
      None
    } else {
      Some(json_diff(expected, &self.actual))
    };
    match diff {
      Some(d) => self.check_with_diff(pass, "toStrictEqual", json_short(expected), json_short(&self.actual), d),
      None => self.check(pass, "toStrictEqual", json_short(expected), json_short(&self.actual)),
    }
  }

  // ── nullishness ──────────────────────────────────────────────────

  #[track_caller]
  pub fn to_be_null(&self) -> Result<(), AssertionFailure> {
    self.check(self.actual.is_null(), "toBeNull", "null", json_short(&self.actual))
  }

  /// `toBeUndefined` is satisfied by `null` because `serde_json` cannot
  /// distinguish — the QuickJS binding emits both as `Value::Null`.
  #[track_caller]
  pub fn to_be_undefined(&self) -> Result<(), AssertionFailure> {
    self.check(
      self.actual.is_null(),
      "toBeUndefined",
      "undefined",
      json_short(&self.actual),
    )
  }

  #[track_caller]
  pub fn to_be_defined(&self) -> Result<(), AssertionFailure> {
    self.check(
      !self.actual.is_null(),
      "toBeDefined",
      "defined value",
      json_short(&self.actual),
    )
  }

  // ── truthiness ───────────────────────────────────────────────────

  #[track_caller]
  pub fn to_be_truthy(&self) -> Result<(), AssertionFailure> {
    self.check(is_truthy(&self.actual), "toBeTruthy", "truthy", json_short(&self.actual))
  }

  #[track_caller]
  pub fn to_be_falsy(&self) -> Result<(), AssertionFailure> {
    self.check(!is_truthy(&self.actual), "toBeFalsy", "falsy", json_short(&self.actual))
  }

  // ── numeric ──────────────────────────────────────────────────────

  #[track_caller]
  pub fn to_be_nan(&self) -> Result<(), AssertionFailure> {
    let pass = self.actual.as_f64().is_some_and(f64::is_nan);
    self.check(pass, "toBeNaN", "NaN", json_short(&self.actual))
  }

  #[track_caller]
  pub fn to_be_close_to(&self, expected: f64, digits: Option<u8>) -> Result<(), AssertionFailure> {
    let digits = digits.unwrap_or(2);
    let actual = self.actual.as_f64().ok_or_else(|| {
      self.fail(
        "toBeCloseTo",
        format!("number close to {expected}"),
        json_short(&self.actual),
        None,
        Some(Location::caller()),
      )
    })?;
    let pass = close_enough_within(actual, expected, digits);
    self.check(
      pass,
      "toBeCloseTo",
      format!("{expected} (±{digits} decimal places)"),
      format!("{actual}"),
    )
  }

  #[track_caller]
  pub fn to_be_greater_than(&self, expected: f64) -> Result<(), AssertionFailure> {
    let actual = self.numeric_or_fail("toBeGreaterThan", expected)?;
    self.check(
      actual > expected,
      "toBeGreaterThan",
      format!("> {expected}"),
      format!("{actual}"),
    )
  }

  #[track_caller]
  pub fn to_be_greater_than_or_equal(&self, expected: f64) -> Result<(), AssertionFailure> {
    let actual = self.numeric_or_fail("toBeGreaterThanOrEqual", expected)?;
    self.check(
      actual >= expected,
      "toBeGreaterThanOrEqual",
      format!(">= {expected}"),
      format!("{actual}"),
    )
  }

  #[track_caller]
  pub fn to_be_less_than(&self, expected: f64) -> Result<(), AssertionFailure> {
    let actual = self.numeric_or_fail("toBeLessThan", expected)?;
    self.check(
      actual < expected,
      "toBeLessThan",
      format!("< {expected}"),
      format!("{actual}"),
    )
  }

  pub fn to_be_less_than_or_equal(&self, expected: f64) -> Result<(), AssertionFailure> {
    let actual = self.numeric_or_fail("toBeLessThanOrEqual", expected)?;
    self.check(
      actual <= expected,
      "toBeLessThanOrEqual",
      format!("<= {expected}"),
      format!("{actual}"),
    )
  }

  #[track_caller]
  fn numeric_or_fail(&self, method: &str, expected: f64) -> Result<f64, AssertionFailure> {
    let loc = Location::caller();
    self.actual.as_f64().ok_or_else(|| {
      self.fail(
        method,
        format!("{expected}"),
        format!("non-numeric {}", json_short(&self.actual)),
        None,
        Some(loc),
      )
    })
  }

  // ── containment ──────────────────────────────────────────────────

  /// `toContain` — primitive membership in array, substring in string.
  #[track_caller]
  pub fn to_contain(&self, expected: &Value) -> Result<(), AssertionFailure> {
    let pass = match (&self.actual, expected) {
      (Value::Array(arr), exp) => arr.iter().any(|v| primitive_strict_equal(v, exp)),
      (Value::String(s), Value::String(needle)) => s.contains(needle.as_str()),
      _ => false,
    };
    self.check(
      pass,
      "toContain",
      format!("containing {}", json_short(expected)),
      json_short(&self.actual),
    )
  }

  /// Deep `toContain` — every element compared by `deep_equal`.
  #[track_caller]
  pub fn to_contain_equal(&self, expected: &Value) -> Result<(), AssertionFailure> {
    let pass = match &self.actual {
      Value::Array(arr) => arr.iter().any(|v| deep_equal(v, expected)),
      _ => false,
    };
    if pass {
      self.check(
        pass,
        "toContainEqual",
        format!("containing equal {}", json_short(expected)),
        json_short(&self.actual),
      )
    } else {
      let diff = json_diff(expected, &self.actual);
      self.check_with_diff(
        pass,
        "toContainEqual",
        format!("containing equal {}", json_short(expected)),
        json_short(&self.actual),
        diff,
      )
    }
  }

  #[track_caller]
  pub fn to_have_length(&self, expected: usize) -> Result<(), AssertionFailure> {
    let actual_len = match &self.actual {
      Value::Array(a) => Some(a.len()),
      Value::String(s) => Some(s.chars().count()),
      _ => None,
    };
    match actual_len {
      Some(len) => self.check(
        len == expected,
        "toHaveLength",
        format!("length {expected}"),
        format!("length {len}"),
      ),
      None => Err(self.fail(
        "toHaveLength",
        format!("length {expected}"),
        format!("value without .length: {}", json_short(&self.actual)),
        None,
        Some(Location::caller()),
      )),
    }
  }

  /// `toHaveProperty(path, value?)` — `path` may be `"a.b.c"` or a
  /// JSON array of keys / indexes.
  #[track_caller]
  pub fn to_have_property(&self, path: &Value, expected: Option<&Value>) -> Result<(), AssertionFailure> {
    let loc = Location::caller();
    let segments =
      parse_property_path(path).map_err(|e| self.fail("toHaveProperty", e, json_short(path), None, Some(loc)))?;
    let descended = descend(&self.actual, &segments);
    let pass = match (descended, expected) {
      (Some(_), None) => true,
      (Some(val), Some(exp)) => deep_equal(val, exp),
      (None, _) => false,
    };
    let desc = format!(
      "property {} {}",
      path_describe(&segments),
      expected.map(|v| format!("= {}", json_short(v))).unwrap_or_default()
    );
    let received = match descend(&self.actual, &segments) {
      Some(v) => format!("= {}", json_short(v)),
      None => "(missing)".to_string(),
    };
    self.check(pass, "toHaveProperty", desc, received)
  }

  /// Jest's `toMatch(string)` is substring containment; `toMatch(/re/)`
  /// is regex. Differs from [`StringOrRegex::matches`] (which is
  /// exact-equality for the string variant).
  #[track_caller]
  pub fn to_match(&self, pattern: &StringOrRegex) -> Result<(), AssertionFailure> {
    let actual = match self.actual.as_str() {
      Some(s) => s,
      None => {
        return Err(self.fail(
          "toMatch",
          format!("matching {}", pattern.description()),
          format!("non-string {}", json_short(&self.actual)),
          None,
          Some(Location::caller()),
        ));
      },
    };
    let pass = match pattern {
      StringOrRegex::String(needle) => actual.contains(needle.as_str()),
      StringOrRegex::Regex(re) => re.is_match(actual),
    };
    self.check(pass, "toMatch", pattern.description(), format!("{actual:?}"))
  }

  #[track_caller]
  pub fn to_match_object(&self, subset: &Value) -> Result<(), AssertionFailure> {
    let pass = match_object(&self.actual, subset);
    if pass {
      self.check(pass, "toMatchObject", json_short(subset), json_short(&self.actual))
    } else {
      let diff = json_diff(subset, &self.actual);
      self.check_with_diff(
        pass,
        "toMatchObject",
        json_short(subset),
        json_short(&self.actual),
        diff,
      )
    }
  }

  /// `toBeInstanceOf(ctorName)` — checked against the binding-supplied
  /// constructor name (`Class.name`). When `actual_ctor_name` is
  /// `None`, falls back to the inferred built-in (e.g. `Array` for an
  /// array value).
  #[track_caller]
  pub fn to_be_instance_of(&self, ctor_name: &str, actual_ctor_name: Option<&str>) -> Result<(), AssertionFailure> {
    let actual_name = actual_ctor_name.unwrap_or_else(|| infer_builtin_ctor(&self.actual));
    let pass = actual_name == ctor_name;
    self.check(
      pass,
      "toBeInstanceOf",
      format!("instance of {ctor_name}"),
      format!("instance of {actual_name}"),
    )
  }
}

fn close_enough_within(a: f64, b: f64, digits: u8) -> bool {
  if !a.is_finite() || !b.is_finite() {
    return float_bit_eq(a, b);
  }
  let tol = 10f64.powi(-i32::from(digits)) / 2.0;
  (a - b).abs() < tol
}

fn is_truthy(v: &Value) -> bool {
  match v {
    Value::Null => false,
    Value::Bool(b) => *b,
    Value::Number(n) => n.as_f64().is_some_and(|f| !float_bit_eq(f, 0.0) && !float_bit_eq(f, -0.0) && !f.is_nan()),
    Value::String(s) => !s.is_empty(),
    Value::Array(_) | Value::Object(_) => true,
  }
}

fn primitive_strict_equal(a: &Value, b: &Value) -> bool {
  match (a, b) {
    (Value::Null, Value::Null) => true,
    (Value::Bool(x), Value::Bool(y)) => x == y,
    (Value::Number(x), Value::Number(y)) => match (x.as_f64(), y.as_f64()) {
      (Some(xf), Some(yf)) => float_bit_eq(xf, yf),
      _ => false,
    },
    (Value::String(x), Value::String(y)) => x == y,
    _ => deep_equal(a, b),
  }
}

#[derive(Debug, Clone)]
enum PropSegment {
  Key(String),
  Index(usize),
}

fn parse_property_path(path: &Value) -> Result<Vec<PropSegment>, String> {
  match path {
    Value::String(s) => Ok(s.split('.').map(|seg| PropSegment::Key(seg.to_string())).collect()),
    Value::Array(arr) => arr
      .iter()
      .map(|seg| match seg {
        Value::String(s) => Ok(PropSegment::Key(s.clone())),
        Value::Number(n) => n
          .as_u64()
          .map(|i| PropSegment::Index(i as usize))
          .ok_or_else(|| "property path index must be a non-negative integer".to_string()),
        other => Err(format!(
          "property path segment must be string or integer; got {}",
          json_short(other)
        )),
      })
      .collect(),
    _ => Err("property path must be a string or array".into()),
  }
}

fn descend<'a>(v: &'a Value, segments: &[PropSegment]) -> Option<&'a Value> {
  let mut cur = v;
  for seg in segments {
    cur = match (cur, seg) {
      (Value::Object(map), PropSegment::Key(k)) => map.get(k)?,
      (Value::Array(arr), PropSegment::Index(i)) => arr.get(*i)?,
      (Value::Array(arr), PropSegment::Key(k)) => arr.get(k.parse::<usize>().ok()?)?,
      _ => return None,
    };
  }
  Some(cur)
}

fn path_describe(segments: &[PropSegment]) -> String {
  let parts: Vec<String> = segments
    .iter()
    .map(|s| match s {
      PropSegment::Key(k) => k.clone(),
      PropSegment::Index(i) => format!("[{i}]"),
    })
    .collect();
  parts.join(".")
}

fn infer_builtin_ctor(v: &Value) -> &'static str {
  match v {
    Value::Null => "Null",
    Value::Bool(_) => "Boolean",
    Value::Number(_) => "Number",
    Value::String(_) => "String",
    Value::Array(_) => "Array",
    Value::Object(_) => "Object",
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::asymmetric::ASYM_TAG_KEY;
  use serde_json::json;

  fn ok(r: Result<(), AssertionFailure>) {
    if let Err(e) = r {
      panic!("expected ok, got: {}", e.message);
    }
  }
  fn err(r: Result<(), AssertionFailure>) {
    assert!(r.is_err(), "expected err");
  }

  #[test]
  fn to_be_primitive_only() {
    ok(expect_value(json!(1)).to_be(&json!(1)));
    err(expect_value(json!(1)).to_be(&json!(2)));
    err(expect_value(json!([1])).to_be(&json!([1])));
  }

  #[test]
  fn to_equal_failure_carries_diff_and_location() {
    let actual = json!({"id": 1, "name": "Alice", "tags": ["admin", "user"]});
    let expected = json!({"id": 2, "name": "Alice", "tags": ["admin"]});
    let err = expect_value(actual)
      .to_equal(&expected)
      .expect_err("toEqual should fail");
    // Diff is multi-line + has +/- markers.
    let diff = err.diff.as_deref().unwrap_or("");
    assert!(diff.contains('-'), "diff missing '-' line: {diff}");
    assert!(diff.contains('+'), "diff missing '+' line: {diff}");
    assert!(diff.contains("\"id\""), "diff lacks pretty-JSON key context: {diff}");
    // Location captured.
    let loc = err.location.expect("location captured");
    assert!(loc.file.contains("value.rs"), "location file: {}", loc.file);
    assert!(loc.line > 0);
  }

  #[test]
  fn to_equal_recurses() {
    ok(expect_value(json!({"a": [1, 2]})).to_equal(&json!({"a": [1, 2]})));
    err(expect_value(json!({"a": [1, 2]})).to_equal(&json!({"a": [1, 3]})));
  }

  #[test]
  fn to_equal_with_asymmetric() {
    let exp = json!({"id": {ASYM_TAG_KEY: "any", "name": "Number"}});
    ok(expect_value(json!({"id": 7})).to_equal(&exp));
    err(expect_value(json!({"id": "x"})).to_equal(&exp));
  }

  #[test]
  fn not_inverts() {
    ok(expect_value(json!(1)).not().to_be(&json!(2)));
    err(expect_value(json!(1)).not().to_be(&json!(1)));
  }

  #[test]
  fn to_contain_array_and_string() {
    ok(expect_value(json!([1, 2, 3])).to_contain(&json!(2)));
    err(expect_value(json!([1, 2, 3])).to_contain(&json!(4)));
    ok(expect_value(json!("hello world")).to_contain(&json!("world")));
  }

  #[test]
  fn to_contain_equal_deep() {
    ok(expect_value(json!([{"id": 1}, {"id": 2}])).to_contain_equal(&json!({"id": 2})));
    err(expect_value(json!([{"id": 1}, {"id": 2}])).to_contain_equal(&json!({"id": 3})));
  }

  #[test]
  fn to_have_length_works() {
    ok(expect_value(json!([1, 2, 3])).to_have_length(3));
    ok(expect_value(json!("abcd")).to_have_length(4));
    err(expect_value(json!(42)).to_have_length(1));
  }

  #[test]
  fn to_have_property_dot_path() {
    ok(expect_value(json!({"a": {"b": 1}})).to_have_property(&json!("a.b"), None));
    ok(expect_value(json!({"a": {"b": 1}})).to_have_property(&json!("a.b"), Some(&json!(1))));
    err(expect_value(json!({"a": {"b": 1}})).to_have_property(&json!("a.c"), None));
  }

  #[test]
  fn to_have_property_array_path_with_index() {
    ok(expect_value(json!({"arr": [10, 20]})).to_have_property(&json!(["arr", 1]), Some(&json!(20))));
  }

  #[test]
  fn to_match_string_substring() {
    ok(expect_value(json!("hello")).to_match(&StringOrRegex::String("ello".into())));
    ok(expect_value(json!("hello")).to_match(&StringOrRegex::Regex(Regex::new("^h.+o$").unwrap())));
    err(expect_value(json!("hello")).to_match(&StringOrRegex::String("bye".into())));
  }

  #[test]
  fn to_match_object_subset() {
    ok(expect_value(json!({"a": 1, "b": 2})).to_match_object(&json!({"a": 1})));
    err(expect_value(json!({"a": 1, "b": 2})).to_match_object(&json!({"a": 2})));
  }

  #[test]
  fn close_to_default_two_digits() {
    ok(expect_value(json!(0.1 + 0.2)).to_be_close_to(0.3, None));
    err(expect_value(json!(0.1 + 0.2)).to_be_close_to(0.4, None));
  }

  #[test]
  fn truthy_and_falsy() {
    ok(expect_value(json!(1)).to_be_truthy());
    ok(expect_value(json!("")).to_be_falsy());
    ok(expect_value(json!(0)).to_be_falsy());
    ok(expect_value(Value::Null).to_be_falsy());
  }

  #[test]
  fn to_be_instance_of_builtins() {
    ok(expect_value(json!([1])).to_be_instance_of("Array", None));
    ok(expect_value(json!("x")).to_be_instance_of("String", None));
    err(expect_value(json!(1)).to_be_instance_of("String", None));
  }

  #[test]
  fn greater_less_than() {
    ok(expect_value(json!(5)).to_be_greater_than(3.0));
    err(expect_value(json!(3)).to_be_greater_than(3.0));
    ok(expect_value(json!(3)).to_be_greater_than_or_equal(3.0));
    ok(expect_value(json!(2)).to_be_less_than(3.0));
    ok(expect_value(json!(3)).to_be_less_than_or_equal(3.0));
  }
}
