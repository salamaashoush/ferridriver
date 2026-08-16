//! Screenshot step definitions.

use crate::step::StepError;
use crate::world::BrowserWorld;
use ferridriver_bdd_macros::step;

const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

/// A capture that silently returned nothing still satisfies a step that only
/// awaits it, so every screenshot step checks the bytes it got back.
fn check_png(what: &str, bytes: &[u8]) -> Result<(), StepError> {
  if !bytes.starts_with(PNG_MAGIC) {
    return Err(StepError::from(format!(
      "{what} returned {} bytes that are not a PNG",
      bytes.len()
    )));
  }
  Ok(())
}

#[step("I take a screenshot")]
async fn take_screenshot(world: &mut BrowserWorld) {
  let bytes = world
    .page()
    .screenshot()
    .await
    .map_err(|e| StepError::wrap("screenshot", e))?;
  check_png("screenshot", &bytes)?;
}

#[step("I take a full page screenshot")]
async fn take_full_page_screenshot(world: &mut BrowserWorld) {
  let bytes = world
    .page()
    .screenshot()
    .full_page(true)
    .await
    .map_err(|e| StepError::wrap("full page screenshot", e))?;
  check_png("full page screenshot", &bytes)?;
}

#[step("I take a screenshot of {string}")]
async fn take_screenshot_of(world: &mut BrowserWorld, selector: String) {
  let bytes = world
    .page()
    .locator(&selector)
    .screenshot()
    .await
    .map_err(|e| StepError::wrap(format!("screenshot of \"{selector}\""), e))?;
  check_png(&format!("screenshot of \"{selector}\""), &bytes)?;
}

#[step("I take a snapshot")]
async fn take_snapshot(world: &mut BrowserWorld) {
  let snapshot = world
    .page()
    .snapshot_for_ai()
    .await
    .map_err(|e| StepError::wrap("snapshot", e))?;
  if snapshot.full.trim().is_empty() {
    return Err(StepError::from(
      "snapshot returned an empty accessibility tree".to_string(),
    ));
  }
}
