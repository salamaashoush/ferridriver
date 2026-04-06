//! Frame/iframe step definitions.
//!
//! Frames in ferridriver are accessed via `page.frame(name)` and `page.frames()`.
//! Frame context is stored in `BrowserWorld` typed state so subsequent steps
//! can create locators scoped to the active frame.

use crate::step::StepError;
use ferridriver::frame::Frame;
use ferridriver_bdd_macros::{step, then, when};
use ferridriver_test::expect::{expect_poll, DEFAULT_EXPECT_TIMEOUT};
use ferridriver_test::model::TestFailure;

fn to_step_err(e: TestFailure) -> StepError {
  StepError {
    message: e.message,
    diff: e.diff.map(|d| (d, String::new())),
  }
}

/// Stored in BrowserWorld typed state to track the currently active frame.
#[derive(Clone)]
struct ActiveFrame(Frame);

#[when("I switch to frame {string}")]
async fn switch_to_frame(world: &mut BrowserWorld, name_or_url: String) {
  let frame = world
    .page()
    .frame(&name_or_url)
    .await
    .map_err(|e| StepError::from(format!("switch to frame \"{name_or_url}\": {e}")))?
    .ok_or_else(|| StepError::from(format!("frame \"{name_or_url}\" not found")))?;
  world.set_state(ActiveFrame(frame));
}

#[when("I switch to main frame")]
async fn switch_to_main_frame(world: &mut BrowserWorld) {
  let frame = world
    .page()
    .main_frame()
    .await
    .map_err(|e| StepError::from(format!("switch to main frame: {e}")))?;
  world.set_state(ActiveFrame(frame));
}

#[then("I should see {int} frame(s)")]
async fn should_see_frame_count(world: &mut BrowserWorld, expected: i64) {
  let page = world.page().clone();
  let expected_count = expected as usize;
  expect_poll(
    || {
      let p = page.clone();
      async move { p.frames().await.map(|f| f.len()).unwrap_or(0) }
    },
    DEFAULT_EXPECT_TIMEOUT,
  )
  .to_equal(expected_count)
  .await
  .map_err(to_step_err)?;
}

#[then("the frame {string} should exist")]
async fn frame_should_exist(world: &mut BrowserWorld, name_or_url: String) {
  let page = world.page().clone();
  let name = name_or_url.clone();
  expect_poll(
    || {
      let p = page.clone();
      let n = name.clone();
      async move { p.frame(&n).await.map(|f| f.is_some()).unwrap_or(false) }
    },
    DEFAULT_EXPECT_TIMEOUT,
  )
  .to_equal(true)
  .await
  .map_err(to_step_err)?;
}

#[step("I evaluate {string} in the active frame")]
async fn evaluate_in_frame(world: &mut BrowserWorld, expression: String) {
  let frame = world
    .get_state::<ActiveFrame>()
    .ok_or_else(|| StepError::from("no active frame -- use 'I switch to frame' first"))?;
  frame
    .0
    .evaluate(&expression)
    .await
    .map_err(|e| StepError::from(format!("evaluate in frame: {e}")))?;
}
