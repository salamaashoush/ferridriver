//! Screenshot step definitions.

use crate::step::StepError;
use crate::world::BrowserWorld;
use ferridriver_bdd_macros::step;

#[step("I take a screenshot")]
async fn take_screenshot(world: &mut BrowserWorld) {
  world
    .page()
    .screenshot(ferridriver::options::ScreenshotOptions::default())
    .await
    .map_err(|e| StepError::from(format!("screenshot: {e}")))?;
}

#[step("I take a screenshot of {string}")]
async fn take_screenshot_of(world: &mut BrowserWorld, selector: String) {
  world
    .page()
    .locator(&selector)
    .screenshot()
    .await
    .map_err(|e| StepError::from(format!("screenshot of \"{selector}\": {e}")))?;
}

#[step("I take a snapshot")]
async fn take_snapshot(world: &mut BrowserWorld) {
  world
    .page()
    .snapshot_for_ai(ferridriver::snapshot::SnapshotOptions {
      depth: None,
      track: None,
    })
    .await
    .map_err(|e| StepError::from(format!("snapshot: {e}")))?;
}
