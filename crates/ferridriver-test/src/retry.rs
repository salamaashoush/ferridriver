//! Retry policy and flaky test detection.

use crate::model::TestStatus;

/// Determines whether a test should be retried and tracks flaky status.
pub struct RetryPolicy;

impl RetryPolicy {
  /// After all attempts, determine final status.
  /// If it failed on some attempts but passed on the last -> `Flaky`.
  pub fn final_status(attempts: &[TestStatus]) -> TestStatus {
    if attempts.is_empty() {
      return TestStatus::Skipped;
    }
    let last = &attempts[attempts.len() - 1];
    if *last == TestStatus::Passed && attempts.len() > 1 {
      // Had failures before but passed on retry.
      TestStatus::Flaky
    } else {
      last.clone()
    }
  }
}
