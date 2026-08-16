//! Retry policy and flaky test detection.

use crate::model::{ExpectedStatus, TestOutcomeKind, TestStatus};

/// Determines whether a test should be retried and tracks flaky status.
pub struct RetryPolicy;

impl RetryPolicy {
  /// After all attempts, determine final status.
  /// If it failed on some attempts but passed on the last -> `Flaky`.
  pub fn final_status(attempts: &[TestStatus]) -> TestStatus {
    Self::final_status_for(attempts, ExpectedStatus::Pass)
  }

  /// [`Self::final_status`] against a declared expectation, so a
  /// `test.fail()` test that fails counts as passing. Shares the
  /// outcome rule with every reporter ([`crate::model::outcome_kind`]).
  pub fn final_status_for(attempts: &[TestStatus], expected: ExpectedStatus) -> TestStatus {
    let Some(last) = attempts.last().copied() else {
      return TestStatus::Skipped;
    };
    match crate::model::outcome_kind(attempts, expected) {
      TestOutcomeKind::Skipped => TestStatus::Skipped,
      TestOutcomeKind::Flaky => TestStatus::Flaky,
      TestOutcomeKind::Expected => TestStatus::Passed,
      // An unexpected *pass* (a `test.fail()` test that succeeded) has a
      // passing status and a failing outcome; report the outcome.
      TestOutcomeKind::Unexpected if last == TestStatus::Passed => TestStatus::Failed,
      TestOutcomeKind::Unexpected => last,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_declared_failure_that_fails_reads_as_passed() {
    assert_eq!(
      RetryPolicy::final_status_for(&[TestStatus::Failed], ExpectedStatus::Fail),
      TestStatus::Passed
    );
  }

  #[test]
  fn a_declared_failure_that_passes_reads_as_failed() {
    assert_eq!(
      RetryPolicy::final_status_for(&[TestStatus::Passed], ExpectedStatus::Fail),
      TestStatus::Failed
    );
  }

  #[test]
  fn failing_then_passing_is_flaky() {
    assert_eq!(
      RetryPolicy::final_status(&[TestStatus::Failed, TestStatus::Passed]),
      TestStatus::Flaky
    );
  }

  #[test]
  fn a_timeout_keeps_its_status() {
    assert_eq!(RetryPolicy::final_status(&[TestStatus::TimedOut]), TestStatus::TimedOut);
  }
}
