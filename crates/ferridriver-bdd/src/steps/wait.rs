//! Wait step definitions using proper APIs (expect auto-retry, locator wait_for).

use std::time::Duration;

use crate::step::StepError;
use crate::world::BrowserWorld;
use ferridriver_bdd_macros::step;
use ferridriver_test::expect::expect;
use ferridriver_test::model::TestFailure;

fn to_step_err(e: TestFailure) -> StepError {
  StepError {
    message: e.message,
    diff: e.diff.map(|d| (d, String::new())),
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
