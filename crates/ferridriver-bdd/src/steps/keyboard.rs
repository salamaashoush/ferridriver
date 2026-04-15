//! Keyboard step definitions.

use crate::step::StepError;
use crate::world::BrowserWorld;
use ferridriver_bdd_macros::when;

#[when("I press {string}")]
async fn press_key(world: &mut BrowserWorld, key: String) {
  world
    .page()
    .keyboard()
    .press(&key)
    .await
    .map_err(|e| StepError::from(format!("press \"{key}\": {e}")))?;
}

#[when("I press {string} on {string}")]
async fn press_key_on(world: &mut BrowserWorld, key: String, selector: String) {
  world
    .page()
    .locator(&selector)
    .press(&key)
    .await
    .map_err(|e| StepError::from(format!("press \"{key}\" on \"{selector}\": {e}")))?;
}

#[when("I type {string}")]
async fn type_text(world: &mut BrowserWorld, text: String) {
  world
    .page()
    .keyboard()
    .r#type(&text)
    .await
    .map_err(|e| StepError::from(format!("type \"{text}\": {e}")))?;
}

#[when("I press {string} with modifier {string}")]
async fn press_with_modifier(world: &mut BrowserWorld, key: String, modifier: String) {
  let combo = format!("{modifier}+{key}");
  world
    .page()
    .keyboard()
    .press(&combo)
    .await
    .map_err(|e| StepError::from(format!("press \"{combo}\": {e}")))?;
}
