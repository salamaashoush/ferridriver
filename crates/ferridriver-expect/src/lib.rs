#![allow(
  clippy::missing_errors_doc,
  clippy::missing_panics_doc,
  clippy::must_use_candidate,
  clippy::module_name_repetitions,
  clippy::cast_possible_truncation,
  clippy::cast_precision_loss,
  clippy::cast_sign_loss,
  clippy::cast_possible_wrap,
  clippy::uninlined_format_args,
  clippy::needless_pass_by_value,
  clippy::doc_markdown,
  clippy::match_same_arms,
  clippy::should_implement_trait,
  clippy::manual_let_else,
  clippy::too_many_lines,
  clippy::return_self_not_must_use,
  // `to_*` builders that consume `self` are the conventional shape for
  // poll-style terminal matchers (`.to_equal()` consumes the
  // `ExpectPoll` to drop the closure).
  clippy::wrong_self_convention,
  clippy::redundant_closure_for_method_calls
)]

//! Value matchers (Jest-compatible) and asymmetric matchers for
//! ferridriver's `expect()` API. Shared between the test runner
//! (`ferridriver-test`) and the QuickJS scripting layer
//! (`ferridriver-script`) so both surfaces dispatch through the same
//! Rust core (per [the Rust-is-the-source-of-truth rule]).
//!
//! Web-first matchers (locator/page/apiResponse) still live in
//! `ferridriver-test::expect` because they need the test runner's
//! polling + screenshot context. This crate is intentionally tiny:
//! `regex`, `serde`, `serde_json` deps only.
//!
//! [the Rust-is-the-source-of-truth rule]: https://github.com/salamaashoush/ferridriver/blob/main/CLAUDE.md

pub mod api_response;
pub mod asymmetric;
pub mod builder;
pub mod diff;
pub mod locator;
pub mod page;
pub mod poll;
pub mod throw;
pub mod value;

pub use asymmetric::{ASYM_TAG_KEY, Asymmetric, TypeTag, deep_equal, match_object};
pub use builder::{
  Expect, ExpectPoll, HaveCssOptions, InViewportOptions, ToPassOptions, expect, expect_configured, expect_poll,
  to_pass, to_pass_with_options,
};
pub use diff::{json_diff, pretty_json, unified_diff};
pub use poll::{
  DEFAULT_EXPECT_TIMEOUT, ExpectContext, MatchError, POLL_INTERVALS, default_expect_timeout, poll_traced, poll_until,
  set_default_expect_timeout,
};
pub use throw::{ExpectFn, ThrowMatcher, ThrownError, expect_fn};
pub use value::{ExpectValue, StringOrRegex, expect_value};

/// Failure produced by a synchronous value matcher.
///
/// `message` is the formatted, JS-ready failure body (already includes
/// the diff inline so the rquickjs error and a Rust panic carry the
/// same text). `diff` is the same diff in isolation — handy for
/// reporters that want to colorize / re-render it. `location` is the
/// `#[track_caller]` site that invoked the matcher; the test-runner
/// adapter splices it into the printed failure.
#[derive(Debug, Clone)]
pub struct AssertionFailure {
  pub message: String,
  pub diff: Option<String>,
  pub location: Option<CallerLocation>,
}

/// Source location captured at the matcher call site. `'static` strings
/// come straight from `std::panic::Location` — no allocations on the
/// happy path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallerLocation {
  pub file: &'static str,
  pub line: u32,
  pub column: u32,
}

impl CallerLocation {
  #[must_use]
  pub fn from_std(loc: &'static std::panic::Location<'static>) -> Self {
    Self {
      file: loc.file(),
      line: loc.line(),
      column: loc.column(),
    }
  }
}

impl std::fmt::Display for CallerLocation {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}:{}:{}", self.file, self.line, self.column)
  }
}

impl AssertionFailure {
  pub fn new(message: impl Into<String>, diff: Option<String>) -> Self {
    Self {
      message: message.into(),
      diff,
      location: None,
    }
  }

  #[must_use]
  pub fn with_location(mut self, loc: CallerLocation) -> Self {
    self.location = Some(loc);
    self
  }
}

impl std::fmt::Display for AssertionFailure {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(&self.message)
  }
}

impl std::error::Error for AssertionFailure {}
