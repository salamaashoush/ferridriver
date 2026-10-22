//! Process exit statuses of a suite run.
//!
//! A caller that retries work elsewhere (a CI matrix, a test grid) has to
//! tell "the tests failed" from "the run never got to judge them", so the
//! two never share a status.

/// Every test ended as expected (flaky counts, unless `failOnFlakyTests`).
pub const PASSED: i32 = 0;
/// At least one test ended unexpectedly.
pub const FAILED: i32 = 1;
/// The command line or the configuration is wrong; nothing ran.
pub const USAGE: i32 = 2;
/// A signal (SIGINT, SIGTERM) stopped the run before it finished.
pub const INTERRUPTED: i32 = 3;
/// The run could not execute its tests: a browser failed to launch, a
/// worker crashed, a web server did not come up.
pub const INFRASTRUCTURE: i32 = 4;

/// The status that says more of two: an interruption outranks everything
/// (the run is incomplete), an infrastructure failure outranks test
/// results (they were not a fair verdict), and a failure outranks a pass.
#[must_use]
pub fn worst(a: i32, b: i32) -> i32 {
  fn rank(code: i32) -> u8 {
    match code {
      PASSED => 0,
      FAILED => 1,
      USAGE => 2,
      INFRASTRUCTURE => 3,
      INTERRUPTED => 4,
      _ => 1,
    }
  }
  if rank(b) > rank(a) { b } else { a }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn interruption_outranks_infrastructure_which_outranks_failure() {
    assert_eq!(worst(PASSED, FAILED), FAILED);
    assert_eq!(worst(FAILED, INFRASTRUCTURE), INFRASTRUCTURE);
    assert_eq!(worst(INFRASTRUCTURE, INTERRUPTED), INTERRUPTED);
    assert_eq!(worst(INTERRUPTED, FAILED), INTERRUPTED);
    assert_eq!(worst(PASSED, PASSED), PASSED);
  }
}
