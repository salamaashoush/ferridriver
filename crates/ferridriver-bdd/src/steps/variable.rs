//! Variable management step definitions.

use crate::step::StepError;
use crate::world::BrowserWorld;
use ferridriver_bdd_macros::{step, when};

#[step("I set variable {string} to {string}")]
async fn set_variable(world: &mut BrowserWorld, name: String, value: String) {
  world.set_var(name, value);
}

#[when("I store the text of {string} as {string}")]
async fn store_text(world: &mut BrowserWorld, selector: String, var_name: String) {
  let text = world
    .page()
    .locator(&selector, None)
    .text_content()
    .await
    .map_err(|e| StepError::from(format!("get text of \"{selector}\": {e}")))?
    .unwrap_or_default();

  world.set_var(var_name, text);
}

#[when("I store the value of {string} as {string}")]
async fn store_value(world: &mut BrowserWorld, selector: String, var_name: String) {
  let value = world
    .page()
    .locator(&selector, None)
    .input_value()
    .await
    .map_err(|e| StepError::from(format!("get value of \"{selector}\": {e}")))?;

  world.set_var(var_name, value);
}
