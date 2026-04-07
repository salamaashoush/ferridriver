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

// ── Storage State save/load (Playwright auth pattern) ──────────────────

#[step("I save the storage state to {string}")]
async fn save_storage_state(world: &mut BrowserWorld, file_path: String) {
  let path = world.resolve_fixture_path(&file_path);
  let state = world
    .page()
    .storage_state()
    .await
    .map_err(|e| StepError::from(format!("save storage state: {e}")))?;

  let json = serde_json::to_string_pretty(&state)
    .map_err(|e| StepError::from(format!("serialize storage state: {e}")))?;

  if let Some(parent) = path.parent() {
    std::fs::create_dir_all(parent)
      .map_err(|e| StepError::from(format!("create dir for {}: {e}", path.display())))?;
  }
  std::fs::write(&path, json)
    .map_err(|e| StepError::from(format!("write storage state to {}: {e}", path.display())))?;
}

#[step("I load the storage state from {string}")]
async fn load_storage_state(world: &mut BrowserWorld, file_path: String) {
  let path = world.resolve_fixture_path(&file_path);
  let json = std::fs::read_to_string(&path)
    .map_err(|e| StepError::from(format!("read storage state from {}: {e}", path.display())))?;

  let state: serde_json::Value = serde_json::from_str(&json)
    .map_err(|e| StepError::from(format!("parse storage state: {e}")))?;

  world
    .page()
    .set_storage_state(&state)
    .await
    .map_err(|e| StepError::from(format!("load storage state: {e}")))?;
}
