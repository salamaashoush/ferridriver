//! Window/tab management step definitions.
//!
//! Uses `context.new_page()`, `context.pages()`, `page.close()`, and
//! `page.bring_to_front()` for tab management. The active page index is
//! tracked via typed state in BrowserWorld.

use std::time::Duration;

use crate::step::StepError;
use crate::world::BrowserWorld;
use ferridriver_bdd_macros::{step, then, when};
use ferridriver_test::expect::expect_poll;
use ferridriver_test::model::TestFailure;

fn to_step_err(e: TestFailure) -> StepError {
  StepError {
    message: e.message,
    diff: e.diff.map(|d| (d, String::new())),
    pending: false,
  }
}

#[when("I open a new tab")]
async fn open_new_tab(world: &mut BrowserWorld) {
  let page = world
    .context()
    .new_page()
    .await
    .map_err(|e| StepError::from(format!("open new tab: {e}")))?;

  // Replace the active page with the newly opened tab.
  world.set_page(page);
}

#[when("I switch to tab {int}")]
async fn switch_to_tab(world: &mut BrowserWorld, index: i64) {
  let pages = world
    .context()
    .pages()
    .await
    .map_err(|e| StepError::from(format!("list tabs: {e}")))?;

  let idx = index as usize;
  let page = pages
    .into_iter()
    .nth(idx)
    .ok_or_else(|| StepError::from(format!("tab index {idx} out of range")))?;

  page
    .bring_to_front()
    .await
    .map_err(|e| StepError::from(format!("bring tab {idx} to front: {e}")))?;

  world.set_page(page);
}

#[when("I close the current tab")]
async fn close_current_tab(world: &mut BrowserWorld) {
  world
    .page()
    .close()
    .await
    .map_err(|e| StepError::from(format!("close tab: {e}")))?;

  // After closing, switch to the first remaining page if available.
  let pages = world
    .context()
    .pages()
    .await
    .map_err(|e| StepError::from(format!("list tabs after close: {e}")))?;

  if let Some(page) = pages.into_iter().next() {
    world.set_page(page);
  }
}

#[then("I should see {int} tab(s)")]
async fn should_see_tab_count(world: &mut BrowserWorld, expected: i64) {
  let ctx = world.context().clone();
  let expected_count = expected as usize;
  expect_poll(
    move || {
      let c = ctx.clone();
      async move { c.pages().await.map(|p| p.len()).unwrap_or(0) }
    },
    Duration::from_secs(5),
  )
  .to_equal(expected_count)
  .await
  .map_err(to_step_err)?;
}

#[step("I bring tab to front")]
async fn bring_to_front(world: &mut BrowserWorld) {
  world
    .page()
    .bring_to_front()
    .await
    .map_err(|e| StepError::from(format!("bring tab to front: {e}")))?;
}
