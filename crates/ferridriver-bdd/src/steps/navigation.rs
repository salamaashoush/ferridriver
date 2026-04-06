//! Navigation step definitions.

use crate::step::StepError;
use crate::world::BrowserWorld;
use ferridriver_bdd_macros::{given, step, then};
use ferridriver_test::expect::expect;
use ferridriver_test::model::TestFailure;

fn to_step_err(e: TestFailure) -> StepError {
  StepError {
    message: e.message,
    diff: e.diff.map(|d| (d, String::new())),
  }
}

#[given("I navigate to {string}")]
async fn navigate(world: &mut BrowserWorld, url: String) {
  world
    .page()
    .goto(&url, None)
    .await
    .map_err(|e| StepError::from(format!("navigate to \"{url}\": {e}")))?;
}

#[given("I go back")]
async fn go_back(world: &mut BrowserWorld) {
  world
    .page()
    .go_back(None)
    .await
    .map_err(|e| StepError::from(format!("go back: {e}")))?;
}

#[given("I go forward")]
async fn go_forward(world: &mut BrowserWorld) {
  world
    .page()
    .go_forward(None)
    .await
    .map_err(|e| StepError::from(format!("go forward: {e}")))?;
}

#[step("I reload the page")]
async fn reload(world: &mut BrowserWorld) {
  world
    .page()
    .reload(None)
    .await
    .map_err(|e| StepError::from(format!("reload: {e}")))?;
}

#[then("the URL should contain {string}")]
async fn url_contains(world: &mut BrowserWorld, expected: String) {
  expect(world.page())
    .to_contain_url(&expected)
    .await
    .map_err(to_step_err)?;
}

#[then("the URL should be {string}")]
async fn url_equals(world: &mut BrowserWorld, expected: String) {
  expect(world.page())
    .to_have_url(expected.as_str())
    .await
    .map_err(to_step_err)?;
}
