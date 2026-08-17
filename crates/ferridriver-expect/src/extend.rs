//! Custom matchers — the semantics behind `expect.extend`.
//!
//! Everything a custom matcher needs to behave like a built-in one lives
//! here: the context its body reads, the result it returns, the pass /
//! fail decision, the failure text, and the rules by which a set of
//! matchers composes. A Rust test registers a plain function and gets
//! the same behaviour a JS suite gets from `expect.extend`; the QuickJS
//! binding only marshals values across, and owns none of this.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::AssertionFailure;
use crate::poll::DEFAULT_EXPECT_TIMEOUT;
use crate::subject::PromiseMode;
use crate::value::format_failure;

/// The state a matcher body reads about the assertion it is running in —
/// Playwright's `MatcherContext` (`matchers/expect.ts:394-400`).
#[derive(Debug, Clone)]
pub struct MatcherContext {
  /// Set when the assertion is negated. A matcher reports what it
  /// actually found and lets [`finalize`] apply the inversion; `is_not`
  /// is for wording the message, not for flipping `pass`.
  pub is_not: bool,
  pub is_soft: bool,
  /// Set when the assertion runs under `.resolves` / `.rejects`.
  pub promise: Option<PromiseMode>,
  pub timeout: Duration,
  /// The `expect(value, message)` prefix, if the caller gave one.
  pub custom_message: Option<String>,
}

impl Default for MatcherContext {
  fn default() -> Self {
    Self {
      is_not: false,
      is_soft: false,
      promise: None,
      timeout: DEFAULT_EXPECT_TIMEOUT,
      custom_message: None,
    }
  }
}

/// What a matcher body returns — Playwright's `MatcherResult`.
///
/// `pass` is what the matcher observed, never already-inverted.
#[derive(Debug, Clone, Default)]
pub struct MatcherResult {
  pub pass: bool,
  /// The full failure text. Playwright takes whatever the matcher
  /// returns verbatim, so this is a message, not a title.
  pub message: Option<String>,
  /// Optional rendered values; when present they print under the
  /// message the way a built-in matcher's do.
  pub expected: Option<String>,
  pub received: Option<String>,
  /// Diagnostic lines a matcher collected while running.
  pub log: Vec<String>,
}

impl MatcherResult {
  #[must_use]
  pub fn new(pass: bool) -> Self {
    Self {
      pass,
      ..Default::default()
    }
  }

  #[must_use]
  pub fn with_message(mut self, message: impl Into<String>) -> Self {
    self.message = Some(message.into());
    self
  }

  #[must_use]
  pub fn with_values(mut self, expected: impl Into<String>, received: impl Into<String>) -> Self {
    self.expected = Some(expected.into());
    self.received = Some(received.into());
    self
  }

  #[must_use]
  pub fn with_log(mut self, log: Vec<String>) -> Self {
    self.log = log;
    self
  }
}

/// Playwright's stand-in when a matcher returns no message
/// (`expectLibrary.ts:1703`).
pub const NO_MESSAGE: &str = "No message was specified for this matcher.";

/// A matcher that returned something that is not a matcher result.
/// Playwright throws this rather than failing the assertion
/// (`expectLibrary.ts:1707`).
#[must_use]
pub fn invalid_result_message(returned: &str) -> String {
  format!(
    "Unexpected return from a matcher function.\nMatcher functions should return an object in the following \
     format:\n  {{message?: string | function, pass: boolean}}\n'{returned}' was returned"
  )
}

/// Playwright's message for `expect.extend` handed a non-function
/// (`matchers/expect.ts:283`).
#[must_use]
pub fn not_a_matcher_message(name: &str, type_of: &str) -> String {
  format!("expect.extend: `{name}` is not a valid matcher. Must be a function, is \"{type_of}\"")
}

/// Apply the assertion's negation to what the matcher observed and, on
/// failure, produce the error text.
///
/// Playwright's finalizer is `result.pass === !!info.isNot` — the
/// matcher's own message is used verbatim, with the `expect(value, msg)`
/// prefix in front of it.
pub fn finalize(cx: &MatcherContext, matcher: &str, result: &MatcherResult) -> Result<(), AssertionFailure> {
  if result.pass != cx.is_not {
    return Ok(());
  }
  let body = match (&result.expected, &result.received) {
    (Some(e), Some(r)) => Some(format!("Expected: {e}\nReceived: {r}")),
    _ => None,
  };
  let body = match (body, result.log.is_empty()) {
    (Some(b), false) => Some(format!("{b}\n\nCall log:\n{}", result.log.join("\n"))),
    (None, false) => Some(format!("Call log:\n{}", result.log.join("\n"))),
    (b, true) => b,
  };
  let message = result.message.clone().unwrap_or_else(|| NO_MESSAGE.to_string());
  let message = match &cx.custom_message {
    Some(prefix) => format!("{prefix}\n\n{message}"),
    None => message,
  };
  let _ = matcher;
  Err(AssertionFailure::new(message, body))
}

/// Fall back to the built-in failure shape when a custom matcher gives
/// no message of its own, so the output still names the matcher and the
/// values instead of only saying that nothing was specified.
#[must_use]
pub fn default_failure(cx: &MatcherContext, matcher: &str, expected: &str, received: &str) -> AssertionFailure {
  format_failure(
    cx.custom_message.as_deref(),
    cx.is_not,
    matcher,
    expected.to_string(),
    received.to_string(),
    None,
  )
}

/// Every matcher ferridriver ships. `expect.extend` may not shadow one
/// of these on the expect it was called on — only on the expect it
/// returns (Playwright's "legacy behavior" comment,
/// `matchers/expect.ts:288-297`).
///
/// The QuickJS binding asserts its own prototype covers this list, so
/// the two cannot drift.
pub const BUILTIN_MATCHER_NAMES: &[&str] = &[
  "toBe",
  "toBeAttached",
  "toBeChecked",
  "toBeCloseTo",
  "toBeDefined",
  "toBeDisabled",
  "toBeEditable",
  "toBeEmpty",
  "toBeEnabled",
  "toBeFalsy",
  "toBeFocused",
  "toBeGreaterThan",
  "toBeGreaterThanOrEqual",
  "toBeHidden",
  "toBeInViewport",
  "toBeInstanceOf",
  "toBeLessThan",
  "toBeLessThanOrEqual",
  "toBeNaN",
  "toBeNull",
  "toBeOK",
  "toBeTruthy",
  "toBeUndefined",
  "toBeVisible",
  "toContain",
  "toContainClass",
  "toContainEqual",
  "toContainText",
  "toEqual",
  "toHaveAccessibleDescription",
  "toHaveAccessibleName",
  "toHaveAttribute",
  "toHaveCSS",
  "toHaveClass",
  "toHaveCount",
  "toHaveId",
  "toHaveJSProperty",
  "toHaveLength",
  "toHaveProperty",
  "toHaveRole",
  "toHaveScreenshot",
  "toHaveText",
  "toHaveTitle",
  "toHaveURL",
  "toHaveValue",
  "toHaveValues",
  "toMatch",
  "toMatchAriaSnapshot",
  "toMatchObject",
  "toMatchSnapshot",
  "toPass",
  "toStrictEqual",
  "toThrow",
];

#[must_use]
pub fn is_builtin_matcher(name: &str) -> bool {
  BUILTIN_MATCHER_NAMES.binary_search(&name).is_ok()
}

/// An ordered, immutable set of custom matcher names.
///
/// Generic over what a host stores per name: a Rust caller keeps the
/// function itself, while the QuickJS binding keeps `()` because the
/// function lives in a JS object that QuickJS traces. Both get the same
/// composition rules.
#[derive(Debug, Clone)]
pub struct MatcherSet<M> {
  entries: Vec<(String, M)>,
}

impl<M> Default for MatcherSet<M> {
  fn default() -> Self {
    Self { entries: Vec::new() }
  }
}

impl<M: Clone> MatcherSet<M> {
  #[must_use]
  pub fn new() -> Self {
    Self::default()
  }

  /// Playwright's `expect.extend` composition: a later registration of
  /// the same name replaces the earlier one in place, keeping
  /// registration order stable, and the result is a NEW set — the
  /// receiver is untouched, because `expect.extend` returns a new
  /// expect.
  #[must_use]
  pub fn extend(&self, additions: impl IntoIterator<Item = (String, M)>) -> Self {
    let mut out = self.clone();
    for (name, matcher) in additions {
      match out.entries.iter_mut().find(|(n, _)| *n == name) {
        Some(slot) => slot.1 = matcher,
        None => out.entries.push((name, matcher)),
      }
    }
    out
  }

  #[must_use]
  pub fn get(&self, name: &str) -> Option<&M> {
    self.entries.iter().find(|(n, _)| n == name).map(|(_, m)| m)
  }

  pub fn names(&self) -> impl Iterator<Item = &str> {
    self.entries.iter().map(|(n, _)| n.as_str())
  }

  #[must_use]
  pub fn len(&self) -> usize {
    self.entries.len()
  }

  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.entries.is_empty()
  }

  /// The names that may be published on the expect that `extend` was
  /// CALLED on. A built-in name is only shadowed on the expect `extend`
  /// returns, so an unrelated caller of the original expect keeps the
  /// built-in.
  pub fn publishable<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    names.into_iter().filter(|n| !is_builtin_matcher(n)).collect()
  }

  /// `mergeExpects(...)`: fold every set into this one, left to right.
  #[must_use]
  pub fn merge(&self, others: impl IntoIterator<Item = Self>) -> Self {
    let mut out = self.clone();
    for other in others {
      out = out.extend(other.entries.iter().cloned());
    }
    out
  }
}

/// A Rust-side matcher body: reads the context and the subject's JSON
/// view, returns what it observed.
pub type ValueMatcher = Arc<dyn Fn(&MatcherContext, &Value, &[Value]) -> MatcherResult + Send + Sync>;

/// Wrap a plain function as a registrable matcher.
pub fn matcher<F>(f: F) -> ValueMatcher
where
  F: Fn(&MatcherContext, &Value, &[Value]) -> MatcherResult + Send + Sync + 'static,
{
  Arc::new(f)
}

/// One key of an `expect.configure` call. Playwright decides on
/// PRESENCE (`'key' in configuration`), so passing a key with no value
/// clears it while leaving the key out keeps what was there.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Setting<T> {
  #[default]
  Keep,
  Set(T),
  Clear,
}

impl<T: Clone> Setting<T> {
  /// Apply this setting to a current value.
  #[must_use]
  pub fn apply(&self, current: Option<T>) -> Option<T> {
    match self {
      Self::Keep => current,
      Self::Set(v) => Some(v.clone()),
      Self::Clear => None,
    }
  }

  /// The setting a JS caller expressed: the key was absent, present
  /// with a value, or present with `undefined`.
  #[must_use]
  pub fn from_optional(present: bool, value: Option<T>) -> Self {
    match (present, value) {
      (false, _) => Self::Keep,
      (true, Some(v)) => Self::Set(v),
      (true, None) => Self::Clear,
    }
  }
}

/// Expect-level configuration — `expect.configure({ message, timeout, soft })`.
#[derive(Debug, Clone, Default)]
pub struct ExpectConfigure {
  pub message: Setting<String>,
  pub timeout: Setting<Duration>,
  pub soft: Option<bool>,
}

/// The state an `expect` carries between calls — Playwright's
/// `ExpectMetaInfo`. `expect.configure` / `.soft` / `.extend` each
/// return a new one rather than mutating.
#[derive(Debug, Clone)]
pub struct ExpectMeta<M> {
  pub message: Option<String>,
  pub is_soft: bool,
  pub timeout: Option<Duration>,
  pub matchers: MatcherSet<M>,
}

impl<M> Default for ExpectMeta<M> {
  fn default() -> Self {
    Self {
      message: None,
      is_soft: false,
      timeout: None,
      matchers: MatcherSet::default(),
    }
  }
}

impl<M: Clone> ExpectMeta<M> {
  #[must_use]
  pub fn configure(&self, cfg: &ExpectConfigure) -> Self {
    let mut out = self.clone();
    out.message = cfg.message.apply(out.message);
    out.timeout = cfg.timeout.apply(out.timeout);
    if let Some(soft) = cfg.soft {
      out.is_soft = soft;
    }
    out
  }

  /// `expect.soft` — already-soft returns itself, so the getter is
  /// idempotent (`matchers/expect.ts:268`).
  #[must_use]
  pub fn softened(&self) -> Self {
    let mut out = self.clone();
    out.is_soft = true;
    out
  }

  #[must_use]
  pub fn extended(&self, additions: impl IntoIterator<Item = (String, M)>) -> Self {
    let mut out = self.clone();
    out.matchers = out.matchers.extend(additions);
    out
  }

  /// The context a matcher body sees when this expect runs `matcher`.
  #[must_use]
  pub fn context(&self, is_not: bool, promise: Option<PromiseMode>) -> MatcherContext {
    MatcherContext {
      is_not,
      is_soft: self.is_soft,
      promise,
      timeout: self.timeout.unwrap_or_else(crate::poll::default_expect_timeout),
      custom_message: self.message.clone(),
    }
  }
}

/// Run a Rust matcher against a JSON subject and apply the assertion's
/// negation — the whole path a Rust test takes to a custom matcher.
pub fn run_value_matcher(
  cx: &MatcherContext,
  name: &str,
  matcher: &ValueMatcher,
  actual: &Value,
  args: &[Value],
) -> Result<(), AssertionFailure> {
  let result = matcher(cx, actual, args);
  finalize(cx, name, &result)
}

/// Names registered more than once across a merge, for a host that wants
/// to report shadowing.
#[must_use]
pub fn shadowed_names<M: Clone>(sets: &[MatcherSet<M>]) -> BTreeSet<String> {
  let mut seen: BTreeSet<String> = BTreeSet::new();
  let mut dupes = BTreeSet::new();
  for set in sets {
    for name in set.names() {
      if !seen.insert(name.to_string()) {
        dupes.insert(name.to_string());
      }
    }
  }
  dupes
}

#[cfg(test)]
mod tests {
  use serde_json::json;

  use super::*;

  fn within(cx: &MatcherContext, actual: &Value, args: &[Value]) -> MatcherResult {
    let lo = args.first().and_then(Value::as_f64).unwrap_or(0.0);
    let hi = args.get(1).and_then(Value::as_f64).unwrap_or(0.0);
    let got = actual.as_f64().unwrap_or(f64::NAN);
    let pass = got >= lo && got <= hi;
    MatcherResult::new(pass)
      .with_message(format!(
        "expected {got} {}to be within {lo}..{hi}",
        if cx.is_not { "not " } else { "" }
      ))
      .with_values(format!("{lo}..{hi}"), got.to_string())
  }

  #[test]
  fn a_rust_matcher_passes_and_fails_like_a_builtin() {
    let m = matcher(within);
    let cx = MatcherContext::default();
    run_value_matcher(&cx, "toBeWithin", &m, &json!(5), &[json!(0), json!(10)]).unwrap();
    let err = run_value_matcher(&cx, "toBeWithin", &m, &json!(50), &[json!(0), json!(10)]).unwrap_err();
    assert!(err.message.contains("to be within 0..10"), "{}", err.message);
    assert!(err.diff.unwrap().contains("Received: 50"));
  }

  #[test]
  fn negation_is_applied_by_the_caller_not_the_matcher() {
    let m = matcher(within);
    let not = MatcherContext {
      is_not: true,
      ..Default::default()
    };
    // The matcher still reports what it saw; `.not` inverts the verdict.
    run_value_matcher(&not, "toBeWithin", &m, &json!(50), &[json!(0), json!(10)]).unwrap();
    let err = run_value_matcher(&not, "toBeWithin", &m, &json!(5), &[json!(0), json!(10)]).unwrap_err();
    assert!(err.message.contains("not to be within"), "{}", err.message);
  }

  #[test]
  fn the_expect_message_prefixes_a_matcher_message() {
    let cx = MatcherContext {
      custom_message: Some("ids match".into()),
      ..Default::default()
    };
    let err = finalize(&cx, "toBeWithin", &MatcherResult::new(false).with_message("nope")).unwrap_err();
    assert_eq!(err.message, "ids match\n\nnope");
  }

  #[test]
  fn a_matcher_without_a_message_says_so() {
    let err = finalize(&MatcherContext::default(), "toBeX", &MatcherResult::new(false)).unwrap_err();
    assert_eq!(err.message, NO_MESSAGE);
  }

  #[test]
  fn a_call_log_prints_under_the_values() {
    let result = MatcherResult::new(false)
      .with_message("nope")
      .with_log(vec!["locator resolved to <div>".into()]);
    let err = finalize(&MatcherContext::default(), "toBeX", &result).unwrap_err();
    assert!(err.diff.unwrap().contains("Call log:\nlocator resolved to <div>"));
  }

  #[test]
  fn extend_returns_a_new_set_and_keeps_registration_order() {
    let a: MatcherSet<u8> = MatcherSet::new().extend([("toBeA".to_string(), 1), ("toBeB".to_string(), 2)]);
    let b = a.extend([("toBeA".to_string(), 9)]);
    assert_eq!(a.get("toBeA"), Some(&1), "the receiver must not be mutated");
    assert_eq!(b.get("toBeA"), Some(&9));
    assert_eq!(b.names().collect::<Vec<_>>(), vec!["toBeA", "toBeB"]);
  }

  #[test]
  fn merge_folds_left_to_right() {
    let a: MatcherSet<u8> = MatcherSet::new().extend([("toBeA".to_string(), 1)]);
    let b: MatcherSet<u8> = MatcherSet::new().extend([("toBeB".to_string(), 2), ("toBeA".to_string(), 3)]);
    let merged = MatcherSet::new().merge([a.clone(), b.clone()]);
    assert_eq!(merged.get("toBeA"), Some(&3));
    assert_eq!(merged.len(), 2);
    assert_eq!(shadowed_names(&[a, b]).into_iter().collect::<Vec<_>>(), vec!["toBeA"]);
  }

  #[test]
  fn a_builtin_name_is_never_published_on_the_original_expect() {
    assert!(is_builtin_matcher("toBe"));
    assert!(!is_builtin_matcher("toBeWithin"));
    assert_eq!(
      MatcherSet::<u8>::publishable(["toBe", "toBeWithin"]),
      vec!["toBeWithin"]
    );
  }

  #[test]
  fn the_builtin_list_is_sorted_so_lookup_is_a_binary_search() {
    let mut sorted = BUILTIN_MATCHER_NAMES.to_vec();
    sorted.sort_unstable();
    assert_eq!(sorted, BUILTIN_MATCHER_NAMES.to_vec());
  }

  #[test]
  fn configure_only_touches_the_keys_it_was_given() {
    let base: ExpectMeta<u8> = ExpectMeta {
      message: Some("base".into()),
      is_soft: false,
      timeout: Some(Duration::from_millis(100)),
      matchers: MatcherSet::new(),
    };
    let timed = base.configure(&ExpectConfigure {
      timeout: Setting::Set(Duration::from_millis(900)),
      ..Default::default()
    });
    assert_eq!(timed.message.as_deref(), Some("base"), "message must survive");
    assert_eq!(timed.timeout, Some(Duration::from_millis(900)));
    // Passing the key with no value clears it, as `'message' in cfg` does.
    let cleared = base.configure(&ExpectConfigure {
      message: Setting::Clear,
      ..Default::default()
    });
    assert_eq!(cleared.message, None);
    assert!(base.message.is_some(), "the receiver must not be mutated");
    assert!(base.softened().is_soft);
  }
}
