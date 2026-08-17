//! The live JavaScript subject behind `expect(...)`.
//!
//! [`crate::ExpectValue`] compares a `serde_json` snapshot of the subject.
//! That is the right model for a Rust caller and for the structural
//! matchers (`toEqual`, `toMatchObject`, ...), but a snapshot has no
//! identity, collapses `undefined` onto `null`, cannot hold a function
//! and cannot answer `instanceof` — so the matchers Playwright defines in
//! terms of `Object.is`, `instanceof`, `indexOf` and `typeof` cannot be
//! expressed against it.
//!
//! [`ExpectLive`] is the other half: the same failure shape and the same
//! `.not` / `.soft` / message handling, over a subject the host keeps
//! alive. A host implements [`LiveValue`] with the handful of primitive
//! queries the JS semantics need; every decision and every message stays
//! here.

use std::panic::Location;

use crate::asymmetric::float_bit_eq;
use crate::value::format_failure;
use crate::{AssertionFailure, CallerLocation};

/// `typeof`, refined with the `null` / array distinctions the matchers
/// branch on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsType {
  Undefined,
  Null,
  Boolean,
  Number,
  BigInt,
  String,
  Symbol,
  Function,
  Array,
  Object,
}

impl JsType {
  #[must_use]
  pub fn name(self) -> &'static str {
    match self {
      Self::Undefined => "undefined",
      Self::Null => "null",
      Self::Boolean => "boolean",
      Self::Number => "number",
      Self::BigInt => "bigint",
      Self::String => "string",
      Self::Symbol => "symbol",
      Self::Function => "function",
      Self::Array => "array",
      Self::Object => "object",
    }
  }

  /// True for the two values that have no properties at all — the ones
  /// `toContain` and `toHaveLength` refuse outright.
  #[must_use]
  pub fn is_nullish(self) -> bool {
    matches!(self, Self::Undefined | Self::Null)
  }
}

/// A matcher called with an argument or a receiver it cannot work on.
///
/// Playwright throws a `TypeError` for these rather than failing the
/// assertion, and `.not` does not flip them — `expect(null).not.toContain(1)`
/// throws upstream instead of passing.
#[derive(Debug, Clone)]
pub struct MatcherInputError {
  pub matcher: &'static str,
  pub message: String,
}

impl std::fmt::Display for MatcherInputError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "expect(received).{}(expected)\n\n{}", self.matcher, self.message)
  }
}

impl std::error::Error for MatcherInputError {}

/// What a live matcher can end with: a normal assertion failure, a
/// misuse the host must surface as a `TypeError`, or a host error raised
/// while reading the value (a JS exception out of a getter, an iterator
/// that threw).
#[derive(Debug)]
pub enum LiveError<E> {
  Failed(AssertionFailure),
  BadInput(MatcherInputError),
  Host(E),
}

impl<E> From<AssertionFailure> for LiveError<E> {
  fn from(f: AssertionFailure) -> Self {
    Self::Failed(f)
  }
}

impl<E> From<MatcherInputError> for LiveError<E> {
  fn from(e: MatcherInputError) -> Self {
    Self::BadInput(e)
  }
}

/// The primitive queries the live matchers need from a host value.
///
/// Everything here is a direct JS operation; no matcher logic belongs in
/// an implementation. `Error` is the host's own error type so a JS
/// exception thrown by a getter or an iterator propagates unchanged.
pub trait LiveValue: Sized {
  type Error;

  fn js_type(&self) -> JsType;

  /// `Object.is(self, other)`.
  fn same_value(&self, other: &Self) -> Result<bool, Self::Error>;

  /// Deep equality over the host's structural view of both values —
  /// used only to decide whether a failed `toBe` should suggest
  /// `toEqual`.
  fn structurally_equal(&self, other: &Self) -> bool;

  /// JS truthiness.
  fn truthy(&self) -> bool;

  /// `Some` only for the `number` type — never a numeric string, and
  /// never a `bigint` (whose value may not survive an `f64`).
  fn number(&self) -> Option<f64>;

  /// `Some` only for the `string` type.
  fn text(&self) -> Option<String>;

  /// The value's `.length`, when it has one and it is a number.
  fn length(&self) -> Result<Option<f64>, Self::Error>;

  /// `[...self]`, or `None` when the value is not iterable.
  fn spread(&self) -> Result<Option<Vec<Self>>, Self::Error>;

  /// `self instanceof ctor`. The caller has already checked that `ctor`
  /// is a function.
  fn instance_of(&self, ctor: &Self) -> Result<bool, Self::Error>;

  /// How the value prints in a failure message.
  fn describe(&self) -> String;

  /// The value's structure, for a deep comparison. One call so a host
  /// walks the value once rather than answering six questions about it.
  fn shape(&self) -> Result<Shape<Self>, Self::Error>;

  /// The constructor's name, for `toStrictEqual`'s type check — `None`
  /// for a value with no constructor (a null-prototype object).
  fn class_name(&self) -> Option<String>;

  /// A stable identity for a reference value, used to stop a cyclic
  /// comparison. `None` for primitives and for hosts with no notion of
  /// identity.
  fn ref_id(&self) -> Option<u64>;

  /// The structural view, for the matchers that still compare JSON and
  /// for rendering. `None` when the value has no such form.
  fn as_json(&self) -> Option<serde_json::Value>;

  /// Read a property the way JS does — through the prototype chain, so
  /// a getter or an inherited field answers. `None` when the value has
  /// no such property at all, which is what `toHaveProperty` reports as
  /// missing.
  ///
  /// The default derives from [`Self::shape`], which is enough for a
  /// host whose values are plain data.
  fn get_prop(&self, key: &str) -> Result<Option<Self>, Self::Error> {
    Ok(match self.shape()? {
      Shape::Object(entries) => entries.into_iter().find(|(k, _)| k == key).map(|(_, v)| v),
      Shape::Array(items) => match key.parse::<usize>() {
        Ok(i) => items.into_iter().nth(i).flatten(),
        Err(_) => None,
      },
      _ => None,
    })
  }

  /// The asymmetric matcher this value IS, if any — `expect.any(...)`
  /// and friends are ordinary objects carrying a tag.
  fn asymmetric(&self) -> Option<crate::asymmetric::Asymmetric> {
    self
      .as_json()
      .as_ref()
      .and_then(crate::asymmetric::Asymmetric::from_value)
  }

  /// `self === other`. Derived: strict equality differs from
  /// `Object.is` in exactly two places — `NaN` is never equal to itself,
  /// and `-0` equals `+0` (adding `+0.0` normalizes the sign bit, so one
  /// bit comparison still covers both zeros).
  fn strict_equals(&self, other: &Self) -> Result<bool, Self::Error> {
    if let (Some(a), Some(b)) = (self.number(), other.number()) {
      return Ok(!a.is_nan() && !b.is_nan() && float_bit_eq(a + 0.0, b + 0.0));
    }
    self.same_value(other)
  }
}

/// Which half of a settled promise a matcher chain asserts on.
///
/// Playwright: `expect(p).resolves.toBe(x)` / `expect(p).rejects.toThrow()`
/// (`matchers/expect.ts:311-320`). The mode is part of the matcher's
/// identity — it appears in the failure title and, once `expect.extend`
/// lands, in the matcher's `this.promise`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromiseMode {
  Resolves,
  Rejects,
}

impl PromiseMode {
  #[must_use]
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Resolves => "resolves",
      Self::Rejects => "rejects",
    }
  }
}

/// The three ways a `.resolves` / `.rejects` chain fails before its
/// matcher ever runs (jest `expectLibrary.ts:1725-1745`).
#[derive(Debug, Clone, Copy)]
pub enum PromiseMismatch {
  /// The subject is neither a promise nor a function returning one.
  NotAPromise,
  RejectedNotResolved,
  ResolvedNotRejected,
}

/// Build the failure for a `.resolves` / `.rejects` chain whose subject
/// settled the wrong way. `received` is the offending value, already
/// rendered.
#[must_use]
pub fn promise_failure(
  mode: PromiseMode,
  matcher: &str,
  is_not: bool,
  prefix: Option<&str>,
  mismatch: PromiseMismatch,
  received: &str,
) -> AssertionFailure {
  let (expected, received) = match mismatch {
    PromiseMismatch::NotAPromise => (
      "a promise, or a function returning a promise".to_string(),
      received.to_string(),
    ),
    PromiseMismatch::RejectedNotResolved => (
      "promise to resolve".to_string(),
      format!("promise rejected instead of resolved\nRejected to value: {received}"),
    ),
    PromiseMismatch::ResolvedNotRejected => (
      "promise to reject".to_string(),
      format!("promise resolved instead of rejected\nResolved to value: {received}"),
    ),
  };
  format_failure(
    prefix,
    is_not,
    &format!("{}.{matcher}", mode.as_str()),
    expected,
    received,
    None,
  )
}

/// What a value looks like to a deep comparison.
///
/// The variants are the ones jest's equality distinguishes: a `Date` is
/// its instant, a `RegExp` its pattern, a `Map` its entries, and an
/// array's holes are not the same as `undefined` values.
#[derive(Debug)]
pub enum Shape<V> {
  /// Compared with `Object.is` / type rules rather than by walking.
  Primitive,
  /// `None` is a hole — an index the array does not have at all.
  Array(Vec<Option<V>>),
  /// Own enumerable string-keyed properties, in the host's order.
  Object(Vec<(String, V)>),
  Map(Vec<(V, V)>),
  Set(Vec<V>),
  /// Epoch milliseconds; `NaN` for an invalid date.
  Date(f64),
  RegExp {
    source: String,
    flags: String,
  },
  Error {
    name: String,
    message: String,
  },
  /// `ArrayBuffer` / typed array contents.
  Bytes(Vec<u8>),
  /// Compared by identity: two functions are equal only if they are the
  /// same function.
  Function,
}

/// One step of a `toHaveProperty` path.
#[derive(Debug, Clone)]
pub enum PropSegment {
  Key(String),
  Index(usize),
}

impl PropSegment {
  #[must_use]
  pub fn describe(&self) -> String {
    match self {
      Self::Key(k) => k.clone(),
      Self::Index(i) => format!("[{i}]"),
    }
  }
}

/// Walk `path` through a live value.
fn descend_live<V: LiveValue>(root: &V, path: &[PropSegment]) -> Result<Option<V>, V::Error> {
  let mut current: Option<V> = None;
  for segment in path {
    let here = current.as_ref().unwrap_or(root);
    let key = match segment {
      PropSegment::Key(k) => k.clone(),
      PropSegment::Index(i) => i.to_string(),
    };
    match here.get_prop(&key)? {
      Some(v) => current = Some(v),
      None => return Ok(None),
    }
  }
  Ok(current)
}

/// A live-value assertion. Mirrors [`crate::ExpectValue`]'s builder and
/// produces byte-identical failure text.
pub struct ExpectLive<'a, V: LiveValue> {
  actual: &'a V,
  is_not: bool,
  is_soft: bool,
  message: Option<String>,
}

/// Wrap a live host value for assertion.
pub fn expect_live<V: LiveValue>(actual: &V) -> ExpectLive<'_, V> {
  ExpectLive {
    actual,
    is_not: false,
    is_soft: false,
    message: None,
  }
}

impl<'a, V: LiveValue> ExpectLive<'a, V> {
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

  pub fn actual(&self) -> &'a V {
    self.actual
  }

  fn fail(
    &self,
    method: &str,
    expected: impl Into<String>,
    received: impl Into<String>,
    rich_diff: Option<String>,
    location: Option<&'static Location<'static>>,
  ) -> AssertionFailure {
    let mut failure = format_failure(
      self.message.as_deref(),
      self.is_not,
      method,
      expected.into(),
      received.into(),
      rich_diff,
    )
    .with_soft(self.is_soft);
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
  ) -> Result<(), LiveError<V::Error>> {
    self.check_with_diff(pass, method, expected, received, None)
  }

  #[track_caller]
  fn check_with_diff(
    &self,
    pass: bool,
    method: &str,
    expected: impl Into<String>,
    received: impl Into<String>,
    diff: Option<String>,
  ) -> Result<(), LiveError<V::Error>> {
    let pass = if self.is_not { !pass } else { pass };
    if pass {
      return Ok(());
    }
    let failure = self.fail(method, expected, received, diff, Some(Location::caller()));
    match crate::soft::absorb(failure) {
      Ok(()) => Ok(()),
      Err(failure) => Err(LiveError::Failed(failure)),
    }
  }

  fn bad_input(matcher: &'static str, message: impl Into<String>) -> LiveError<V::Error> {
    LiveError::BadInput(MatcherInputError {
      matcher,
      message: message.into(),
    })
  }

  // ── identity ─────────────────────────────────────────────────────

  /// `toBe(expected)` — `Object.is`, as Playwright's is (jest
  /// `expectLibrary.ts::toBe`). Two structurally equal objects are NOT
  /// equal here; the failure says so and names the matcher that would
  /// pass.
  #[track_caller]
  pub fn to_be(&self, expected: &V) -> Result<(), LiveError<V::Error>> {
    let pass = self.actual.same_value(expected).map_err(LiveError::Host)?;
    let hint = (!pass && !self.is_not && self.actual.structurally_equal(expected))
      .then(|| "If it should pass with deep equality, replace \"toBe\" with \"toEqual\"".to_string());
    self.check_with_diff(pass, "toBe", expected.describe(), self.actual.describe(), hint)
  }

  /// `toBeInstanceOf(ctor)` — the real `instanceof` operator. A
  /// non-function argument is a `TypeError`, not a failure.
  #[track_caller]
  pub fn to_be_instance_of(&self, ctor: &V) -> Result<(), LiveError<V::Error>> {
    if ctor.js_type() != JsType::Function {
      return Err(Self::bad_input(
        "toBeInstanceOf",
        format!("expected value must be a function\n\nExpected: {}", ctor.describe()),
      ));
    }
    let pass = self.actual.instance_of(ctor).map_err(LiveError::Host)?;
    self.check(
      pass,
      "toBeInstanceOf",
      format!("instance of {}", ctor.describe()),
      self.actual.describe(),
    )
  }

  // ── containment ──────────────────────────────────────────────────

  /// `toContain(expected)` — substring for a string receiver, otherwise
  /// `[...received].indexOf(expected)`, i.e. strict equality over the
  /// live items (jest `expectLibrary.ts::toContain`).
  #[track_caller]
  pub fn to_contain(&self, expected: &V) -> Result<(), LiveError<V::Error>> {
    if self.actual.js_type().is_nullish() {
      return Err(Self::bad_input(
        "toContain",
        format!(
          "received value must not be null nor undefined\n\nReceived: {}",
          self.actual.describe()
        ),
      ));
    }
    if let Some(haystack) = self.actual.text() {
      let Some(needle) = expected.text() else {
        return Err(Self::bad_input(
          "toContain",
          format!(
            "expected value must be a string if received value is a string\n\nExpected: {}\nReceived: {}",
            expected.describe(),
            self.actual.describe()
          ),
        ));
      };
      let pass = haystack.contains(&needle);
      return self.check(
        pass,
        "toContain",
        format!("containing {}", expected.describe()),
        self.actual.describe(),
      );
    }
    let Some(items) = self.actual.spread().map_err(LiveError::Host)? else {
      return Err(Self::bad_input(
        "toContain",
        format!(
          "received value must be a string or an iterable\n\nReceived: {}",
          self.actual.describe()
        ),
      ));
    };
    let mut pass = false;
    for item in &items {
      if item.strict_equals(expected).map_err(LiveError::Host)? {
        pass = true;
        break;
      }
    }
    let suggest = (!pass && !self.is_not && items.iter().any(|i| i.structurally_equal(expected)))
      .then(|| "If it should pass with deep equality, replace \"toContain\" with \"toContainEqual\"".to_string());
    self.check_with_diff(
      pass,
      "toContain",
      format!("containing {}", expected.describe()),
      self.actual.describe(),
      suggest,
    )
  }

  /// `toHaveLength(expected)` — reads the receiver's own `.length`, so a
  /// function's arity and a `TypedArray`'s length work as they do
  /// upstream. A receiver without a numeric `.length` is a `TypeError`.
  #[track_caller]
  pub fn to_have_length(&self, expected: f64) -> Result<(), LiveError<V::Error>> {
    if !expected.is_finite() || expected < 0.0 || expected.fract() != 0.0 {
      return Err(Self::bad_input(
        "toHaveLength",
        format!("expected value must be a non-negative integer\n\nExpected: {expected}"),
      ));
    }
    let Some(len) = self.actual.length().map_err(LiveError::Host)? else {
      return Err(Self::bad_input(
        "toHaveLength",
        format!(
          "received value must have a length property whose value must be a number\n\nReceived: {}",
          self.actual.describe()
        ),
      ));
    };
    self.check(
      float_bit_eq(len, expected),
      "toHaveLength",
      format!("length {expected}"),
      format!("length {len} ({})", self.actual.describe()),
    )
  }

  // ── structure ────────────────────────────────────────────────────
  //
  // The same engine `ExpectValue` uses, over the live values, so a Map,
  // a Date, a class instance and an `undefined`-valued key mean here
  // what they mean in Playwright.

  /// `toEqual(expected)` — jest's non-strict deep equality.
  #[track_caller]
  pub fn to_equal(&self, expected: &V, ev: crate::asymmetric::Evaluator<'_>) -> Result<(), LiveError<V::Error>> {
    self.structural(expected, "toEqual", crate::equality::Mode::Loose, ev)
  }

  /// `toStrictEqual(expected)` — adds the constructor check, array
  /// sparseness, and `undefined`-valued keys.
  #[track_caller]
  pub fn to_strict_equal(&self, expected: &V, ev: crate::asymmetric::Evaluator<'_>) -> Result<(), LiveError<V::Error>> {
    self.structural(expected, "toStrictEqual", crate::equality::Mode::Strict, ev)
  }

  /// `toMatchObject(subset)`.
  #[track_caller]
  pub fn to_match_object(&self, subset: &V, ev: crate::asymmetric::Evaluator<'_>) -> Result<(), LiveError<V::Error>> {
    self.structural(subset, "toMatchObject", crate::equality::Mode::Subset, ev)
  }

  #[track_caller]
  fn structural(
    &self,
    expected: &V,
    method: &'static str,
    mode: crate::equality::Mode,
    ev: crate::asymmetric::Evaluator<'_>,
  ) -> Result<(), LiveError<V::Error>> {
    let pass = crate::equality::equals(self.actual, expected, mode, ev).map_err(LiveError::Host)?;
    let diff = (!pass)
      .then(|| match (expected.as_json(), self.actual.as_json()) {
        (Some(e), Some(a)) => Some(crate::diff::json_diff(&e, &a)),
        _ => None,
      })
      .flatten();
    self.check_with_diff(pass, method, expected.describe(), self.actual.describe(), diff)
  }

  /// `toContainEqual(expected)` — deep equality against each item.
  #[track_caller]
  pub fn to_contain_equal(
    &self,
    expected: &V,
    ev: crate::asymmetric::Evaluator<'_>,
  ) -> Result<(), LiveError<V::Error>> {
    let items = match self.actual.shape().map_err(LiveError::Host)? {
      Shape::Array(items) => items,
      Shape::Set(values) => values.into_iter().map(Some).collect(),
      _ => Vec::new(),
    };
    let mut pass = false;
    for item in items.iter().flatten() {
      if crate::equality::equals(item, expected, crate::equality::Mode::Loose, ev).map_err(LiveError::Host)? {
        pass = true;
        break;
      }
    }
    self.check(
      pass,
      "toContainEqual",
      format!("containing equal {}", expected.describe()),
      self.actual.describe(),
    )
  }

  /// `toHaveProperty(path, value?)` — walks the live value, so a getter
  /// or a class field answers as it would in the page.
  #[track_caller]
  pub fn to_have_property(
    &self,
    path: &[PropSegment],
    expected: Option<&V>,
    ev: crate::asymmetric::Evaluator<'_>,
  ) -> Result<(), LiveError<V::Error>> {
    let found = descend_live(self.actual, path).map_err(LiveError::Host)?;
    let pass = match (&found, expected) {
      (Some(_), None) => true,
      (Some(got), Some(want)) => {
        crate::equality::equals(got, want, crate::equality::Mode::Loose, ev).map_err(LiveError::Host)?
      },
      (None, _) => false,
    };
    let described = path.iter().map(PropSegment::describe).collect::<Vec<_>>().join(".");
    let expectation = match expected {
      Some(v) => format!("property {described} = {}", v.describe()),
      None => format!("property {described}"),
    };
    let received = match &found {
      Some(v) => format!("= {}", v.describe()),
      None => "(missing)".to_string(),
    };
    self.check(pass, "toHaveProperty", expectation, received)
  }

  // ── type and truthiness ──────────────────────────────────────────

  /// `toBeNull()` — `received === null`, which a snapshot cannot tell
  /// from `undefined`.
  #[track_caller]
  pub fn to_be_null(&self) -> Result<(), LiveError<V::Error>> {
    self.check(
      self.actual.js_type() == JsType::Null,
      "toBeNull",
      "null",
      self.actual.describe(),
    )
  }

  /// `toBeUndefined()` — `received === undefined`.
  #[track_caller]
  pub fn to_be_undefined(&self) -> Result<(), LiveError<V::Error>> {
    self.check(
      self.actual.js_type() == JsType::Undefined,
      "toBeUndefined",
      "undefined",
      self.actual.describe(),
    )
  }

  /// `toBeDefined()` — `received !== undefined`. `null` IS defined.
  #[track_caller]
  pub fn to_be_defined(&self) -> Result<(), LiveError<V::Error>> {
    self.check(
      self.actual.js_type() != JsType::Undefined,
      "toBeDefined",
      "defined value",
      self.actual.describe(),
    )
  }

  #[track_caller]
  pub fn to_be_truthy(&self) -> Result<(), LiveError<V::Error>> {
    self.check(self.actual.truthy(), "toBeTruthy", "truthy", self.actual.describe())
  }

  #[track_caller]
  pub fn to_be_falsy(&self) -> Result<(), LiveError<V::Error>> {
    self.check(!self.actual.truthy(), "toBeFalsy", "falsy", self.actual.describe())
  }

  /// `toBeNaN()` — `Number.isNaN(received)`, so a non-number never
  /// passes (a snapshot turns `NaN` into `null` and loses the case).
  #[track_caller]
  pub fn to_be_nan(&self) -> Result<(), LiveError<V::Error>> {
    let pass = self.actual.number().is_some_and(f64::is_nan);
    self.check(pass, "toBeNaN", "NaN", self.actual.describe())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// A stand-in for a host's JS value: enough shape to exercise every
  /// decision in this module without a VM. `Obj` carries an id so two
  /// structurally identical objects can still be distinct references.
  #[derive(Debug, Clone, PartialEq)]
  enum Mock {
    Undefined,
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Fun { name: &'static str, arity: f64 },
    Arr(Vec<Mock>),
    Obj { id: u32, shape: Vec<(String, String)> },
  }

  impl LiveValue for Mock {
    type Error = String;

    fn js_type(&self) -> JsType {
      match self {
        Self::Undefined => JsType::Undefined,
        Self::Null => JsType::Null,
        Self::Bool(_) => JsType::Boolean,
        Self::Num(_) => JsType::Number,
        Self::Str(_) => JsType::String,
        Self::Fun { .. } => JsType::Function,
        Self::Arr(_) => JsType::Array,
        Self::Obj { .. } => JsType::Object,
      }
    }

    fn same_value(&self, other: &Self) -> Result<bool, Self::Error> {
      Ok(match (self, other) {
        (Self::Num(a), Self::Num(b)) => float_bit_eq(*a, *b),
        (Self::Obj { id: a, .. }, Self::Obj { id: b, .. }) => a == b,
        (a, b) => a == b,
      })
    }

    fn structurally_equal(&self, other: &Self) -> bool {
      match (self, other) {
        (Self::Obj { shape: a, .. }, Self::Obj { shape: b, .. }) => a == b,
        (a, b) => a == b,
      }
    }

    fn truthy(&self) -> bool {
      match self {
        Self::Undefined | Self::Null => false,
        Self::Bool(b) => *b,
        Self::Num(n) => *n != 0.0 && !n.is_nan(),
        Self::Str(s) => !s.is_empty(),
        _ => true,
      }
    }

    fn number(&self) -> Option<f64> {
      match self {
        Self::Num(n) => Some(*n),
        _ => None,
      }
    }

    fn text(&self) -> Option<String> {
      match self {
        Self::Str(s) => Some(s.clone()),
        _ => None,
      }
    }

    fn length(&self) -> Result<Option<f64>, Self::Error> {
      Ok(match self {
        Self::Arr(items) => Some(items.len() as f64),
        Self::Str(s) => Some(s.chars().count() as f64),
        Self::Fun { arity, .. } => Some(*arity),
        _ => None,
      })
    }

    fn spread(&self) -> Result<Option<Vec<Self>>, Self::Error> {
      Ok(match self {
        Self::Arr(items) => Some(items.clone()),
        _ => None,
      })
    }

    fn instance_of(&self, ctor: &Self) -> Result<bool, Self::Error> {
      match (self, ctor) {
        (Self::Arr(_), Self::Fun { name: "Array", .. }) => Ok(true),
        (_, Self::Fun { name: "Object", .. }) => Ok(!matches!(self, Self::Undefined | Self::Null)),
        _ => Ok(false),
      }
    }

    fn shape(&self) -> Result<Shape<Self>, Self::Error> {
      Ok(match self {
        Self::Arr(items) => Shape::Array(items.iter().cloned().map(Some).collect()),
        Self::Obj { shape, .. } => {
          Shape::Object(shape.iter().map(|(k, v)| (k.clone(), Self::Str(v.clone()))).collect())
        },
        Self::Fun { .. } => Shape::Function,
        _ => Shape::Primitive,
      })
    }

    fn class_name(&self) -> Option<String> {
      Some(
        match self {
          Self::Arr(_) => "Array",
          Self::Obj { .. } => "Object",
          Self::Fun { .. } => "Function",
          _ => return None,
        }
        .to_string(),
      )
    }

    fn ref_id(&self) -> Option<u64> {
      match self {
        Self::Obj { id, .. } => Some(u64::from(*id)),
        _ => None,
      }
    }

    fn as_json(&self) -> Option<serde_json::Value> {
      match self {
        Self::Undefined => None,
        Self::Null => Some(serde_json::Value::Null),
        Self::Bool(b) => Some(serde_json::json!(b)),
        Self::Num(n) => Some(serde_json::json!(n)),
        Self::Str(s) => Some(serde_json::json!(s)),
        Self::Arr(items) => Some(serde_json::Value::Array(
          items.iter().filter_map(Self::as_json).collect(),
        )),
        Self::Obj { shape, .. } => Some(serde_json::Value::Object(
          shape.iter().map(|(k, v)| (k.clone(), serde_json::json!(v))).collect(),
        )),
        Self::Fun { .. } => None,
      }
    }

    fn describe(&self) -> String {
      match self {
        Self::Undefined => "undefined".into(),
        Self::Null => "null".into(),
        Self::Bool(b) => b.to_string(),
        Self::Num(n) => n.to_string(),
        Self::Str(s) => format!("\"{s}\""),
        Self::Fun { name, .. } => format!("[Function: {name}]"),
        Self::Arr(items) => format!("[{}]", items.iter().map(Self::describe).collect::<Vec<_>>().join(", ")),
        Self::Obj { id, .. } => format!("Object#{id}"),
      }
    }
  }

  fn obj(id: u32) -> Mock {
    Mock::Obj {
      id,
      shape: vec![("a".into(), "1".into())],
    }
  }

  fn failure<E: std::fmt::Debug>(e: LiveError<E>) -> AssertionFailure {
    match e {
      LiveError::Failed(f) => f,
      other => panic!("expected an assertion failure, got {other:?}"),
    }
  }

  fn bad_input<E: std::fmt::Debug>(e: LiveError<E>) -> MatcherInputError {
    match e {
      LiveError::BadInput(b) => b,
      other => panic!("expected a bad-input error, got {other:?}"),
    }
  }

  #[test]
  fn to_be_is_object_is_not_deep_equality() {
    // Distinct references with identical shape: fails, and says which
    // matcher would have passed.
    let err = failure(expect_live(&obj(1)).to_be(&obj(2)).unwrap_err());
    assert!(err.message.contains("expect(value).toBe() failed"), "{}", err.message);
    assert!(
      err
        .diff
        .as_deref()
        .unwrap_or_default()
        .contains("replace \"toBe\" with \"toEqual\""),
      "{err:?}"
    );
    // Same reference passes.
    expect_live(&obj(1)).to_be(&obj(1)).unwrap();
    // And `.not` on two distinct references passes, where a JSON
    // snapshot would have called them equal.
    expect_live(&obj(1)).not().to_be(&obj(2)).unwrap();
  }

  #[test]
  fn to_be_number_edge_cases_follow_object_is() {
    expect_live(&Mock::Num(f64::NAN)).to_be(&Mock::Num(f64::NAN)).unwrap();
    expect_live(&Mock::Num(0.0)).not().to_be(&Mock::Num(-0.0)).unwrap();
  }

  #[test]
  fn strict_equals_is_not_same_value_for_nan_and_signed_zero() {
    // `toContain` compares items with `===`, which inverts both of the
    // cases `Object.is` decides above.
    assert!(!Mock::Num(f64::NAN).strict_equals(&Mock::Num(f64::NAN)).unwrap());
    assert!(Mock::Num(0.0).strict_equals(&Mock::Num(-0.0)).unwrap());
    assert!(Mock::Num(1.5).strict_equals(&Mock::Num(1.5)).unwrap());
    expect_live(&Mock::Arr(vec![Mock::Num(f64::NAN)]))
      .not()
      .to_contain(&Mock::Num(f64::NAN))
      .unwrap();
  }

  #[test]
  fn undefined_and_null_are_distinguished() {
    expect_live(&Mock::Undefined).to_be_undefined().unwrap();
    expect_live(&Mock::Undefined).not().to_be_null().unwrap();
    expect_live(&Mock::Null).to_be_null().unwrap();
    expect_live(&Mock::Null).not().to_be_undefined().unwrap();
    // `null` is defined; only `undefined` is not.
    expect_live(&Mock::Null).to_be_defined().unwrap();
    expect_live(&Mock::Undefined).not().to_be_defined().unwrap();
  }

  #[test]
  fn to_be_nan_needs_a_number() {
    expect_live(&Mock::Num(f64::NAN)).to_be_nan().unwrap();
    expect_live(&Mock::Str("NaN".into())).not().to_be_nan().unwrap();
  }

  #[test]
  fn to_be_instance_of_uses_the_operator() {
    let array_ctor = Mock::Fun {
      name: "Array",
      arity: 1.0,
    };
    let object_ctor = Mock::Fun {
      name: "Object",
      arity: 1.0,
    };
    expect_live(&Mock::Arr(vec![])).to_be_instance_of(&array_ctor).unwrap();
    // A subclass relationship a constructor-NAME comparison would miss.
    expect_live(&Mock::Arr(vec![])).to_be_instance_of(&object_ctor).unwrap();
    let err = bad_input(
      expect_live(&Mock::Arr(vec![]))
        .to_be_instance_of(&Mock::Num(1.0))
        .unwrap_err(),
    );
    assert_eq!(err.matcher, "toBeInstanceOf");
    assert!(err.message.contains("must be a function"), "{err}");
  }

  #[test]
  fn to_contain_is_strict_equality_over_items() {
    let list = Mock::Arr(vec![obj(1), obj(2)]);
    expect_live(&list).to_contain(&obj(1)).unwrap();
    // Structurally equal but a different reference: not contained, and
    // the failure names toContainEqual.
    let err = failure(expect_live(&list).to_contain(&obj(9)).unwrap_err());
    assert!(
      err
        .diff
        .as_deref()
        .unwrap_or_default()
        .contains("replace \"toContain\" with \"toContainEqual\""),
      "{err:?}"
    );
  }

  #[test]
  fn to_contain_string_forms_and_misuse() {
    expect_live(&Mock::Str("hello world".into()))
      .to_contain(&Mock::Str("lo wo".into()))
      .unwrap();
    let err = bad_input(
      expect_live(&Mock::Str("hi".into()))
        .to_contain(&Mock::Num(1.0))
        .unwrap_err(),
    );
    assert!(err.message.contains("must be a string"), "{err}");
    // Misuse is a TypeError even under `.not` — it never silently passes.
    let err = bad_input(expect_live(&Mock::Null).not().to_contain(&Mock::Num(1.0)).unwrap_err());
    assert!(err.message.contains("null nor undefined"), "{err}");
    let err = bad_input(expect_live(&Mock::Num(3.0)).to_contain(&Mock::Num(1.0)).unwrap_err());
    assert!(err.message.contains("string or an iterable"), "{err}");
  }

  #[test]
  fn to_have_length_reads_the_live_length() {
    expect_live(&Mock::Arr(vec![Mock::Num(1.0)]))
      .to_have_length(1.0)
      .unwrap();
    // A function's arity — impossible through a JSON snapshot.
    expect_live(&Mock::Fun { name: "f", arity: 2.0 })
      .to_have_length(2.0)
      .unwrap();
    let err = bad_input(expect_live(&obj(1)).to_have_length(0.0).unwrap_err());
    assert!(err.message.contains("length property"), "{err}");
    let err = bad_input(expect_live(&Mock::Arr(vec![])).to_have_length(-1.0).unwrap_err());
    assert!(err.message.contains("non-negative integer"), "{err}");
  }

  #[test]
  fn truthiness_covers_every_type() {
    expect_live(&Mock::Fun { name: "f", arity: 0.0 })
      .to_be_truthy()
      .unwrap();
    expect_live(&Mock::Str(String::new())).to_be_falsy().unwrap();
    expect_live(&Mock::Num(f64::NAN)).to_be_falsy().unwrap();
    expect_live(&Mock::Arr(vec![])).to_be_truthy().unwrap();
    expect_live(&Mock::Bool(false)).to_be_falsy().unwrap();
    expect_live(&Mock::Bool(true)).to_be_truthy().unwrap();
  }

  #[test]
  fn message_prefix_and_negation_render_like_expect_value() {
    let err = failure(
      expect_live(&Mock::Num(1.0))
        .with_message("ids match")
        .not()
        .to_be(&Mock::Num(1.0))
        .unwrap_err(),
    );
    assert_eq!(err.message, "ids match: expect(value).not.toBe() failed");
  }
}
