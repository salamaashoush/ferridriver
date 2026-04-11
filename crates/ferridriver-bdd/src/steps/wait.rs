//! Wait step definitions using proper APIs (expect auto-retry, locator wait_for).

use std::time::Duration;

use crate::step::{StepError, StepParam};
use crate::world::BrowserWorld;
use ferridriver_bdd_macros::step;
use ferridriver_test::expect::{self, expect};
use ferridriver_test::model::TestFailure;

fn to_step_err(e: TestFailure) -> StepError {
  StepError {
    message: e.message,
    diff: e.diff.map(|d| (d, String::new())),
    pending: false,
  }
}

#[step("I wait {int} millisecond(s)")]
async fn wait_ms(world: &mut BrowserWorld, ms: i64) {
  tokio::time::sleep(Duration::from_millis(ms as u64)).await;
}

#[step("I wait {int} second(s)")]
async fn wait_seconds(world: &mut BrowserWorld, seconds: i64) {
  tokio::time::sleep(Duration::from_secs(seconds as u64)).await;
}

#[step("I wait for {string}")]
async fn wait_for_selector(world: &mut BrowserWorld, selector: String) {
  let locator = world.page().locator(&selector);
  expect(&locator).to_be_attached().await.map_err(to_step_err)?;
}

#[step("I wait for {string} to contain {string}")]
async fn wait_for_text(world: &mut BrowserWorld, selector: String, expected: String) {
  let locator = world.page().locator(&selector);
  expect(&locator)
    .to_contain_text(expected.as_str())
    .await
    .map_err(to_step_err)?;
}

#[step("I wait for {string} to be visible")]
async fn wait_for_visible(world: &mut BrowserWorld, selector: String) {
  let locator = world.page().locator(&selector);
  expect(&locator).to_be_visible().await.map_err(to_step_err)?;
}

#[step("I wait for {string} to be hidden")]
async fn wait_for_hidden(world: &mut BrowserWorld, selector: String) {
  let locator = world.page().locator(&selector);
  expect(&locator).to_be_hidden().await.map_err(to_step_err)?;
}

/// Retry an assertion step until it passes or the timeout is reached.
/// Example: `Then within 5 seconds, "h1" should have text "Hello"`
///
/// Matches the inner step text against the registry and retries with
/// configurable intervals until success or timeout.
#[step(regex = r#"^within (\d+) seconds?, (.+)$"#)]
async fn within_seconds(world: &mut BrowserWorld, timeout_secs: String, inner_step: String) {
  let timeout_secs: u64 = timeout_secs.parse().map_err(|e| StepError {
    message: format!("invalid timeout: {e}"),
    diff: None,
    pending: false,
  })?;

  let timeout = Duration::from_secs(timeout_secs);
  let base_fixtures = world.fixtures().clone();
  let registry = world.registry_arc();

  // Pre-match the inner step once to validate it exists.
  let registry = registry.ok_or_else(|| StepError {
    message: "step registry not available for retry step".into(),
    diff: None,
    pending: false,
  })?;
  let step_match = registry.find_match(&inner_step).map_err(|e| StepError {
    message: format!("inner step not found: {e}"),
    diff: None,
    pending: false,
  })?;
  let handler = step_match.def.handler.clone();
  let params = step_match.params;

  expect::to_pass(timeout, || {
    let handler = handler.clone();
    let params = params.clone();
    let base_fixtures = base_fixtures.clone();
    let registry = registry.clone();
    async move {
      let mut temp_world = BrowserWorld::new(base_fixtures);
      temp_world.set_registry(registry);
      handler(&mut temp_world, params, None, None)
        .await
        .map_err(|e| TestFailure::from(e.message))
    }
  })
  .await
  .map_err(to_step_err)?;
}
