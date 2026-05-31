//! Verifies `#[ferritest_suite(mode = "...")]` sets the discovered suite's
//! execution mode. The serial *dispatch* itself (one worker, source order,
//! skip-on-failure) is shared with the BDD `@serial` path and exercised
//! there; here we only prove the Rust user-facing toggle wires through to
//! `SuiteMode`, with no browser needed.

use ferridriver_test::config::TestConfig;
use ferridriver_test::discovery::collect_rust_tests;
use ferridriver_test::model::SuiteMode;
use ferridriver_test::prelude::*;

#[ferritest_suite(mode = "serial")]
mod serial_suite {
  use ferridriver_test::prelude::*;

  #[ferritest]
  async fn first(_ctx: TestContext) {}

  #[ferritest]
  async fn second(_ctx: TestContext) {}
}

#[ferritest_suite(mode = "parallel")]
mod parallel_suite {
  use ferridriver_test::prelude::*;

  #[ferritest]
  async fn only(_ctx: TestContext) {}
}

#[test]
fn suite_mode_attribute_sets_serial_and_parallel() {
  let plan = collect_rust_tests(&TestConfig::default());

  let serial = plan
    .suites
    .iter()
    .find(|s| s.name == "serial_suite")
    .expect("serial_suite discovered");
  assert_eq!(serial.mode, SuiteMode::Serial, "serial_suite should be marked Serial");
  assert_eq!(serial.tests.len(), 2);

  let parallel = plan
    .suites
    .iter()
    .find(|s| s.name == "parallel_suite")
    .expect("parallel_suite discovered");
  assert_eq!(
    parallel.mode,
    SuiteMode::Parallel,
    "parallel_suite should default to Parallel"
  );
}
