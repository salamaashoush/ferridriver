//! Interaction step definitions: click, fill, type, hover, drag, scroll, select, check.

use crate::step::StepError;
use crate::world::BrowserWorld;
use ferridriver_bdd_macros::{step, when};

#[when("I click {string}")]
async fn click(world: &mut BrowserWorld, selector: String) {
  world
    .page()
    .locator(&selector)
    .click()
    .await
    .map_err(|e| StepError::wrap(format!("click \"{selector}\""), e))?;
}

#[when("I double click {string}")]
async fn double_click(world: &mut BrowserWorld, selector: String) {
  world
    .page()
    .locator(&selector)
    .dblclick()
    .await
    .map_err(|e| StepError::wrap(format!("double click \"{selector}\""), e))?;
}

#[when("I right click {string}")]
async fn right_click(world: &mut BrowserWorld, selector: String) {
  world
    .page()
    .locator(&selector)
    .right_click()
    .await
    .map_err(|e| StepError::wrap(format!("right click \"{selector}\""), e))?;
}

#[when("I fill {string} with {string}")]
async fn fill(world: &mut BrowserWorld, selector: String, value: String) {
  world
    .page()
    .locator(&selector)
    .fill(&value)
    .await
    .map_err(|e| StepError::wrap(format!("fill \"{selector}\" with \"{value}\""), e))?;
}

#[when("I clear {string}")]
async fn clear(world: &mut BrowserWorld, selector: String) {
  world
    .page()
    .locator(&selector)
    .clear()
    .await
    .map_err(|e| StepError::wrap(format!("clear \"{selector}\""), e))?;
}

#[when("I type {string} into {string}")]
async fn type_into(world: &mut BrowserWorld, text: String, selector: String) {
  world
    .page()
    .locator(&selector)
    .r#type(&text)
    .await
    .map_err(|e| StepError::wrap(format!("type \"{text}\" into \"{selector}\""), e))?;
}

#[when("I hover over {string}")]
async fn hover(world: &mut BrowserWorld, selector: String) {
  world
    .page()
    .locator(&selector)
    .hover()
    .await
    .map_err(|e| StepError::wrap(format!("hover \"{selector}\""), e))?;
}

#[when("I focus on {string}")]
async fn focus(world: &mut BrowserWorld, selector: String) {
  world
    .page()
    .locator(&selector)
    .focus()
    .await
    .map_err(|e| StepError::wrap(format!("focus \"{selector}\""), e))?;
}

#[when("I select {string} from {string}")]
async fn select_option(world: &mut BrowserWorld, value: String, selector: String) {
  // Match Playwright's plain-string selectOption semantics: a string can be
  // either the option's `value` or its label. The injected script OR-matches
  // across descriptors, so passing both descriptors selects whichever option
  // matches either field.
  world
    .page()
    .locator(&selector)
    .select_option(vec![
      ferridriver::options::SelectOptionValue::by_value(value.clone()),
      ferridriver::options::SelectOptionValue::by_label(value.clone()),
    ])
    .await
    .map_err(|e| StepError::wrap(format!("select \"{value}\" from \"{selector}\""), e))?;
}

#[when("I check {string}")]
async fn check(world: &mut BrowserWorld, selector: String) {
  world
    .page()
    .locator(&selector)
    .check()
    .await
    .map_err(|e| StepError::wrap(format!("check \"{selector}\""), e))?;
}

#[when("I uncheck {string}")]
async fn uncheck(world: &mut BrowserWorld, selector: String) {
  world
    .page()
    .locator(&selector)
    .uncheck()
    .await
    .map_err(|e| StepError::wrap(format!("uncheck \"{selector}\""), e))?;
}

#[when("I scroll down")]
async fn scroll_down(world: &mut BrowserWorld) {
  world
    .page()
    .evaluate(
      "window.scrollBy(0, 500)",
      ferridriver::protocol::SerializedArgument::default(),
      None,
    )
    .await
    .map_err(|e| StepError::wrap("scroll down", e))?;
}

#[when("I scroll up")]
async fn scroll_up(world: &mut BrowserWorld) {
  world
    .page()
    .evaluate(
      "window.scrollBy(0, -500)",
      ferridriver::protocol::SerializedArgument::default(),
      None,
    )
    .await
    .map_err(|e| StepError::wrap("scroll up", e))?;
}

#[when("I scroll to {string}")]
async fn scroll_to(world: &mut BrowserWorld, selector: String) {
  world
    .page()
    .locator(&selector)
    .scroll_into_view_if_needed()
    .await
    .map_err(|e| StepError::wrap(format!("scroll to \"{selector}\""), e))?;
}

#[when("I drag {string} to {string}")]
async fn drag(world: &mut BrowserWorld, source: String, target: String) {
  let target_locator = world.page().locator(&target);
  world
    .page()
    .locator(&source)
    .drag_to(&target_locator)
    .await
    .map_err(|e| StepError::wrap(format!("drag \"{source}\" to \"{target}\""), e))?;
}

#[when("I click the first {string}")]
async fn click_first(world: &mut BrowserWorld, selector: String) {
  world
    .page()
    .locator(&selector)
    .first()
    .click()
    .await
    .map_err(|e| StepError::wrap(format!("click first \"{selector}\""), e))?;
}

#[when("I click the last {string}")]
async fn click_last(world: &mut BrowserWorld, selector: String) {
  world
    .page()
    .locator(&selector)
    .last()
    .click()
    .await
    .map_err(|e| StepError::wrap(format!("click last \"{selector}\""), e))?;
}

#[when("I click the {int}th {string}")]
async fn click_nth(world: &mut BrowserWorld, n: i64, selector: String) {
  world
    .page()
    .locator(&selector)
    .nth(n as i32)
    .click()
    .await
    .map_err(|e| StepError::wrap(format!("click {n}th \"{selector}\""), e))?;
}

#[when("I tap {string}")]
async fn tap_element(world: &mut BrowserWorld, selector: String) {
  world
    .page()
    .locator(&selector)
    .tap()
    .await
    .map_err(|e| StepError::wrap(format!("tap \"{selector}\""), e))?;
}

#[when("I blur {string}")]
async fn blur(world: &mut BrowserWorld, selector: String) {
  world
    .page()
    .locator(&selector)
    .blur()
    .await
    .map_err(|e| StepError::wrap(format!("blur \"{selector}\""), e))?;
}
