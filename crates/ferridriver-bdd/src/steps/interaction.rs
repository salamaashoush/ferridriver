//! Interaction step definitions: click, fill, type, hover, drag, scroll, select, check.

use crate::step::StepError;
use crate::world::BrowserWorld;
use ferridriver_bdd_macros::{step, when};

#[when("I click {string}")]
async fn click(world: &mut BrowserWorld, selector: String) {
  world
    .page()
    .locator(&selector, None)
    .click(None)
    .await
    .map_err(|e| StepError::from(format!("click \"{selector}\": {e}")))?;
}

#[when("I double click {string}")]
async fn double_click(world: &mut BrowserWorld, selector: String) {
  world
    .page()
    .locator(&selector, None)
    .dblclick(None)
    .await
    .map_err(|e| StepError::from(format!("double click \"{selector}\": {e}")))?;
}

#[when("I right click {string}")]
async fn right_click(world: &mut BrowserWorld, selector: String) {
  world
    .page()
    .locator(&selector, None)
    .right_click()
    .await
    .map_err(|e| StepError::from(format!("right click \"{selector}\": {e}")))?;
}

#[when("I fill {string} with {string}")]
async fn fill(world: &mut BrowserWorld, selector: String, value: String) {
  world
    .page()
    .locator(&selector, None)
    .fill(&value, None)
    .await
    .map_err(|e| StepError::from(format!("fill \"{selector}\" with \"{value}\": {e}")))?;
}

#[when("I clear {string}")]
async fn clear(world: &mut BrowserWorld, selector: String) {
  world
    .page()
    .locator(&selector, None)
    .clear()
    .await
    .map_err(|e| StepError::from(format!("clear \"{selector}\": {e}")))?;
}

#[when("I type {string} into {string}")]
async fn type_into(world: &mut BrowserWorld, text: String, selector: String) {
  world
    .page()
    .locator(&selector, None)
    .r#type(&text, None)
    .await
    .map_err(|e| StepError::from(format!("type \"{text}\" into \"{selector}\": {e}")))?;
}

#[when("I hover over {string}")]
async fn hover(world: &mut BrowserWorld, selector: String) {
  world
    .page()
    .locator(&selector, None)
    .hover(None)
    .await
    .map_err(|e| StepError::from(format!("hover \"{selector}\": {e}")))?;
}

#[when("I focus on {string}")]
async fn focus(world: &mut BrowserWorld, selector: String) {
  world
    .page()
    .locator(&selector, None)
    .focus()
    .await
    .map_err(|e| StepError::from(format!("focus \"{selector}\": {e}")))?;
}

#[when("I select {string} from {string}")]
async fn select_option(world: &mut BrowserWorld, value: String, selector: String) {
  world
    .page()
    .locator(&selector, None)
    .select_option(
      vec![ferridriver::options::SelectOptionValue::by_value(value.clone())],
      None,
    )
    .await
    .map_err(|e| StepError::from(format!("select \"{value}\" from \"{selector}\": {e}")))?;
}

#[when("I check {string}")]
async fn check(world: &mut BrowserWorld, selector: String) {
  world
    .page()
    .locator(&selector, None)
    .check(None)
    .await
    .map_err(|e| StepError::from(format!("check \"{selector}\": {e}")))?;
}

#[when("I uncheck {string}")]
async fn uncheck(world: &mut BrowserWorld, selector: String) {
  world
    .page()
    .locator(&selector, None)
    .uncheck(None)
    .await
    .map_err(|e| StepError::from(format!("uncheck \"{selector}\": {e}")))?;
}

#[when("I scroll down")]
async fn scroll_down(world: &mut BrowserWorld) {
  world
    .page()
    .evaluate("window.scrollBy(0, 500)")
    .await
    .map_err(|e| StepError::from(format!("scroll down: {e}")))?;
}

#[when("I scroll up")]
async fn scroll_up(world: &mut BrowserWorld) {
  world
    .page()
    .evaluate("window.scrollBy(0, -500)")
    .await
    .map_err(|e| StepError::from(format!("scroll up: {e}")))?;
}

#[when("I scroll to {string}")]
async fn scroll_to(world: &mut BrowserWorld, selector: String) {
  world
    .page()
    .locator(&selector, None)
    .scroll_into_view_if_needed()
    .await
    .map_err(|e| StepError::from(format!("scroll to \"{selector}\": {e}")))?;
}

#[when("I drag {string} to {string}")]
async fn drag(world: &mut BrowserWorld, source: String, target: String) {
  let target_locator = world.page().locator(&target, None);
  world
    .page()
    .locator(&source, None)
    .drag_to(&target_locator, None)
    .await
    .map_err(|e| StepError::from(format!("drag \"{source}\" to \"{target}\": {e}")))?;
}

#[when("I click the first {string}")]
async fn click_first(world: &mut BrowserWorld, selector: String) {
  world
    .page()
    .locator(&selector, None)
    .first()
    .click(None)
    .await
    .map_err(|e| StepError::from(format!("click first \"{selector}\": {e}")))?;
}

#[when("I click the last {string}")]
async fn click_last(world: &mut BrowserWorld, selector: String) {
  world
    .page()
    .locator(&selector, None)
    .last()
    .click(None)
    .await
    .map_err(|e| StepError::from(format!("click last \"{selector}\": {e}")))?;
}

#[when("I click the {int}th {string}")]
async fn click_nth(world: &mut BrowserWorld, n: i64, selector: String) {
  world
    .page()
    .locator(&selector, None)
    .nth(n as i32)
    .click(None)
    .await
    .map_err(|e| StepError::from(format!("click {n}th \"{selector}\": {e}")))?;
}

#[when("I tap {string}")]
async fn tap_element(world: &mut BrowserWorld, selector: String) {
  world
    .page()
    .locator(&selector, None)
    .tap(None)
    .await
    .map_err(|e| StepError::from(format!("tap \"{selector}\": {e}")))?;
}

#[when("I blur {string}")]
async fn blur(world: &mut BrowserWorld, selector: String) {
  world
    .page()
    .locator(&selector, None)
    .blur()
    .await
    .map_err(|e| StepError::from(format!("blur \"{selector}\": {e}")))?;
}
