//! LocalStorage/SessionStorage step definitions.

use crate::step::StepError;
use crate::world::BrowserWorld;
use ferridriver_bdd_macros::{step, when};

#[when("I set local storage {string} to {string}")]
async fn set_local_storage(world: &mut BrowserWorld, key: String, value: String) {
  world
    .page()
    .evaluate(&format!(
      "localStorage.setItem('{}', '{}')",
      key.replace('\'', "\\'"),
      value.replace('\'', "\\'")
    ))
    .await
    .map_err(|e| StepError::from(format!("set localStorage \"{key}\": {e}")))?;
}

#[when("I remove local storage {string}")]
async fn remove_local_storage(world: &mut BrowserWorld, key: String) {
  world
    .page()
    .evaluate(&format!(
      "localStorage.removeItem('{}')",
      key.replace('\'', "\\'")
    ))
    .await
    .map_err(|e| StepError::from(format!("remove localStorage \"{key}\": {e}")))?;
}

#[step("I clear local storage")]
async fn clear_local_storage(world: &mut BrowserWorld) {
  world
    .page()
    .evaluate("localStorage.clear()")
    .await
    .map_err(|e| StepError::from(format!("clear localStorage: {e}")))?;
}

#[when("I set session storage {string} to {string}")]
async fn set_session_storage(world: &mut BrowserWorld, key: String, value: String) {
  world
    .page()
    .evaluate(&format!(
      "sessionStorage.setItem('{}', '{}')",
      key.replace('\'', "\\'"),
      value.replace('\'', "\\'")
    ))
    .await
    .map_err(|e| StepError::from(format!("set sessionStorage \"{key}\": {e}")))?;
}

#[when("I remove session storage {string}")]
async fn remove_session_storage(world: &mut BrowserWorld, key: String) {
  world
    .page()
    .evaluate(&format!(
      "sessionStorage.removeItem('{}')",
      key.replace('\'', "\\'")
    ))
    .await
    .map_err(|e| StepError::from(format!("remove sessionStorage \"{key}\": {e}")))?;
}

#[step("I clear session storage")]
async fn clear_session_storage(world: &mut BrowserWorld) {
  world
    .page()
    .evaluate("sessionStorage.clear()")
    .await
    .map_err(|e| StepError::from(format!("clear sessionStorage: {e}")))?;
}
