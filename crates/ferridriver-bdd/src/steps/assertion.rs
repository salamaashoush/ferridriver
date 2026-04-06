//! Assertion step definitions using ferridriver-test's auto-retrying expect API.
//!
//! All assertions use `expect()` with `poll_until`-based auto-retry,
//! matching Playwright's behavior exactly.

use crate::step::StepError;
use crate::world::BrowserWorld;
use ferridriver_bdd_macros::then;
use ferridriver_test::expect::expect;
use ferridriver_test::expect::StringOrRegex;
use ferridriver_test::model::TestFailure;

fn to_step_err(e: TestFailure) -> StepError {
  StepError {
    message: e.message,
    diff: e.diff.map(|d| (d, String::new())),
  }
}

#[then("{string} should be visible")]
async fn should_be_visible(world: &mut BrowserWorld, selector: String) {
  let locator = world.page().locator(&selector);
  expect(&locator).to_be_visible().await.map_err(to_step_err)?;
}

#[then("{string} should be hidden")]
async fn should_be_hidden(world: &mut BrowserWorld, selector: String) {
  let locator = world.page().locator(&selector);
  expect(&locator).to_be_hidden().await.map_err(to_step_err)?;
}

#[then("{string} should be enabled")]
async fn should_be_enabled(world: &mut BrowserWorld, selector: String) {
  let locator = world.page().locator(&selector);
  expect(&locator).to_be_enabled().await.map_err(to_step_err)?;
}

#[then("{string} should be disabled")]
async fn should_be_disabled(world: &mut BrowserWorld, selector: String) {
  let locator = world.page().locator(&selector);
  expect(&locator).to_be_disabled().await.map_err(to_step_err)?;
}

#[then("{string} should be checked")]
async fn should_be_checked(world: &mut BrowserWorld, selector: String) {
  let locator = world.page().locator(&selector);
  expect(&locator).to_be_checked().await.map_err(to_step_err)?;
}

#[then("{string} should contain text {string}")]
async fn should_contain_text(world: &mut BrowserWorld, selector: String, expected: String) {
  let locator = world.page().locator(&selector);
  expect(&locator)
    .to_contain_text(expected.as_str())
    .await
    .map_err(to_step_err)?;
}

#[then("{string} should have text {string}")]
async fn should_have_text(world: &mut BrowserWorld, selector: String, expected: String) {
  let locator = world.page().locator(&selector);
  expect(&locator)
    .to_have_text(expected.as_str())
    .await
    .map_err(to_step_err)?;
}

#[then("{string} should have value {string}")]
async fn should_have_value(world: &mut BrowserWorld, selector: String, expected: String) {
  let locator = world.page().locator(&selector);
  expect(&locator)
    .to_have_value(expected.as_str())
    .await
    .map_err(to_step_err)?;
}

#[then("{string} should have attribute {string} with value {string}")]
async fn should_have_attribute(
  world: &mut BrowserWorld,
  selector: String,
  attr: String,
  expected: String,
) {
  let locator = world.page().locator(&selector);
  expect(&locator)
    .to_have_attribute(&attr, expected.as_str())
    .await
    .map_err(to_step_err)?;
}

#[then("{string} should have class {string}")]
async fn should_have_class(world: &mut BrowserWorld, selector: String, expected_class: String) {
  let locator = world.page().locator(&selector);
  expect(&locator)
    .to_have_class(expected_class.as_str())
    .await
    .map_err(to_step_err)?;
}

#[then("there should be {int} {string} element(s)")]
async fn should_have_count(world: &mut BrowserWorld, expected: i64, selector: String) {
  let locator = world.page().locator(&selector);
  expect(&locator)
    .to_have_count(expected as usize)
    .await
    .map_err(to_step_err)?;
}

#[then("the page title should be {string}")]
async fn page_title_equals(world: &mut BrowserWorld, expected: String) {
  expect(world.page())
    .to_have_title(expected.as_str())
    .await
    .map_err(to_step_err)?;
}

#[then("the page title should contain {string}")]
async fn page_title_contains(world: &mut BrowserWorld, expected: String) {
  expect(world.page())
    .to_contain_title(&expected)
    .await
    .map_err(to_step_err)?;
}

// --- Negated assertions ---

#[then("{string} should not be visible")]
async fn should_not_be_visible(world: &mut BrowserWorld, selector: String) {
  let locator = world.page().locator(&selector);
  expect(&locator)
    .not()
    .to_be_visible()
    .await
    .map_err(to_step_err)?;
}

#[then("{string} should not be hidden")]
async fn should_not_be_hidden(world: &mut BrowserWorld, selector: String) {
  let locator = world.page().locator(&selector);
  expect(&locator)
    .not()
    .to_be_hidden()
    .await
    .map_err(to_step_err)?;
}

#[then("{string} should not contain text {string}")]
async fn should_not_contain_text(world: &mut BrowserWorld, selector: String, expected: String) {
  let locator = world.page().locator(&selector);
  expect(&locator)
    .not()
    .to_contain_text(expected.as_str())
    .await
    .map_err(to_step_err)?;
}

#[then("{string} should not have text {string}")]
async fn should_not_have_text(world: &mut BrowserWorld, selector: String, expected: String) {
  let locator = world.page().locator(&selector);
  expect(&locator)
    .not()
    .to_have_text(expected.as_str())
    .await
    .map_err(to_step_err)?;
}

#[then("{string} should not have class {string}")]
async fn should_not_have_class(world: &mut BrowserWorld, selector: String, expected_class: String) {
  let locator = world.page().locator(&selector);
  expect(&locator)
    .not()
    .to_have_class(expected_class.as_str())
    .await
    .map_err(to_step_err)?;
}

#[then("{string} should not have value {string}")]
async fn should_not_have_value(world: &mut BrowserWorld, selector: String, expected: String) {
  let locator = world.page().locator(&selector);
  expect(&locator)
    .not()
    .to_have_value(expected.as_str())
    .await
    .map_err(to_step_err)?;
}

#[then("{string} should not have attribute {string}")]
async fn should_not_have_attribute(world: &mut BrowserWorld, selector: String, attr: String) {
  let locator = world.page().locator(&selector);
  let any = StringOrRegex::Regex(regex::Regex::new(".*").expect("valid regex"));
  expect(&locator)
    .not()
    .to_have_attribute(&attr, any)
    .await
    .map_err(to_step_err)?;
}

// --- State assertions ---

#[then("{string} should be focused")]
async fn should_be_focused(world: &mut BrowserWorld, selector: String) {
  let locator = world.page().locator(&selector);
  expect(&locator).to_be_focused().await.map_err(to_step_err)?;
}

#[then("{string} should not be focused")]
async fn should_not_be_focused(world: &mut BrowserWorld, selector: String) {
  let locator = world.page().locator(&selector);
  expect(&locator)
    .not()
    .to_be_focused()
    .await
    .map_err(to_step_err)?;
}

#[then("{string} should be empty")]
async fn should_be_empty(world: &mut BrowserWorld, selector: String) {
  let locator = world.page().locator(&selector);
  expect(&locator).to_be_empty().await.map_err(to_step_err)?;
}

#[then("{string} should not be empty")]
async fn should_not_be_empty(world: &mut BrowserWorld, selector: String) {
  let locator = world.page().locator(&selector);
  expect(&locator)
    .not()
    .to_be_empty()
    .await
    .map_err(to_step_err)?;
}

#[then("{string} should be editable")]
async fn should_be_editable(world: &mut BrowserWorld, selector: String) {
  let locator = world.page().locator(&selector);
  expect(&locator).to_be_editable().await.map_err(to_step_err)?;
}

#[then("{string} should not be editable")]
async fn should_not_be_editable(world: &mut BrowserWorld, selector: String) {
  let locator = world.page().locator(&selector);
  expect(&locator)
    .not()
    .to_be_editable()
    .await
    .map_err(to_step_err)?;
}

#[then("{string} should be in viewport")]
async fn should_be_in_viewport(world: &mut BrowserWorld, selector: String) {
  let locator = world.page().locator(&selector);
  expect(&locator)
    .to_be_in_viewport()
    .await
    .map_err(to_step_err)?;
}

#[then("{string} should be attached")]
async fn should_be_attached(world: &mut BrowserWorld, selector: String) {
  let locator = world.page().locator(&selector);
  expect(&locator).to_be_attached().await.map_err(to_step_err)?;
}

// --- CSS assertions ---

#[then("{string} should have CSS {string} with value {string}")]
async fn should_have_css(
  world: &mut BrowserWorld,
  selector: String,
  property: String,
  expected: String,
) {
  let locator = world.page().locator(&selector);
  expect(&locator)
    .to_have_css(&property, expected.as_str())
    .await
    .map_err(to_step_err)?;
}

// --- Accessibility assertions ---

#[then("{string} should have role {string}")]
async fn should_have_role(world: &mut BrowserWorld, selector: String, expected: String) {
  let locator = world.page().locator(&selector);
  expect(&locator)
    .to_have_role(expected.as_str())
    .await
    .map_err(to_step_err)?;
}

#[then("{string} should have accessible name {string}")]
async fn should_have_accessible_name(world: &mut BrowserWorld, selector: String, expected: String) {
  let locator = world.page().locator(&selector);
  expect(&locator)
    .to_have_accessible_name(expected.as_str())
    .await
    .map_err(to_step_err)?;
}

#[then("{string} should have accessible description {string}")]
async fn should_have_accessible_description(
  world: &mut BrowserWorld,
  selector: String,
  expected: String,
) {
  let locator = world.page().locator(&selector);
  expect(&locator)
    .to_have_accessible_description(expected.as_str())
    .await
    .map_err(to_step_err)?;
}

// --- ID assertion ---

#[then("{string} should have id {string}")]
async fn should_have_id(world: &mut BrowserWorld, selector: String, expected: String) {
  let locator = world.page().locator(&selector);
  expect(&locator)
    .to_have_id(expected.as_str())
    .await
    .map_err(to_step_err)?;
}
