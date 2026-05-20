//! Re-export of [`ferridriver_expect`]'s value matchers in the test
//! runner's `expect` namespace, plus the `AssertionFailure` →
//! `TestFailure` adapter so callers can stay on `TestFailure`.

pub use ferridriver_expect::{
  ASYM_TAG_KEY, Asymmetric, ExpectFn, ExpectValue, ThrowMatcher, ThrownError, TypeTag, deep_equal, expect_fn,
  expect_value, match_object,
};

use crate::model::TestFailure;

impl From<ferridriver_expect::AssertionFailure> for TestFailure {
  fn from(a: ferridriver_expect::AssertionFailure) -> Self {
    TestFailure {
      message: a.message,
      stack: a.location.map(|loc| format!("at {loc}")),
      diff: a.diff,
      screenshot: None,
    }
  }
}
