//! Dialog handling step definitions.
//!
//! ferridriver uses handler-based dialogs: you set a handler *before* the action
//! that triggers the dialog. These steps configure the dialog handler and track
//! dialog events via typed state in BrowserWorld.

use std::sync::{Arc, Mutex};

use crate::step::StepError;
use crate::world::BrowserWorld;
use ferridriver::events::{DialogAction, PendingDialog};
use ferridriver_bdd_macros::{given, then};
use ferridriver_test::expect::expect_poll;
use ferridriver_test::model::TestFailure;

fn to_step_err(e: TestFailure) -> StepError {
  StepError {
    message: e.message,
    diff: e.diff.map(|d| (d, String::new())),
    pending: false,
  }
}

/// Tracks dialog events for assertion steps.
#[derive(Clone, Default)]
struct DialogLog {
  entries: Arc<Mutex<Vec<PendingDialog>>>,
}

impl DialogLog {
  fn push(&self, dialog: PendingDialog) {
    self.entries.lock().unwrap_or_else(|e| e.into_inner()).push(dialog);
  }

  fn last_message(&self) -> Option<String> {
    self
      .entries
      .lock()
      .unwrap_or_else(|e| e.into_inner())
      .last()
      .map(|d| d.message.clone())
  }

  fn len(&self) -> usize {
    self.entries.lock().unwrap_or_else(|e| e.into_inner()).len()
  }
}

/// Ensure DialogLog exists in world state; return a clone for handler use.
fn ensure_dialog_log(world: &mut BrowserWorld) -> DialogLog {
  if world.get_state::<DialogLog>().is_none() {
    world.set_state(DialogLog::default());
  }
  world.get_state::<DialogLog>().cloned().unwrap_or_default()
}

#[given("I accept the dialog")]
async fn accept_dialog(world: &mut BrowserWorld) {
  let log = ensure_dialog_log(world);
  world
    .page()
    .set_dialog_handler(Arc::new(move |dialog: &PendingDialog| {
      log.push(dialog.clone());
      DialogAction::Accept(None)
    }))
    .await;
}

#[given("I dismiss the dialog")]
async fn dismiss_dialog(world: &mut BrowserWorld) {
  let log = ensure_dialog_log(world);
  world
    .page()
    .set_dialog_handler(Arc::new(move |dialog: &PendingDialog| {
      log.push(dialog.clone());
      DialogAction::Dismiss
    }))
    .await;
}

#[given("I type {string} in the dialog")]
async fn type_in_dialog(world: &mut BrowserWorld, text: String) {
  let log = ensure_dialog_log(world);
  world
    .page()
    .set_dialog_handler(Arc::new(move |dialog: &PendingDialog| {
      log.push(dialog.clone());
      DialogAction::Accept(Some(text.clone()))
    }))
    .await;
}

#[then("I should see dialog with text {string}")]
async fn should_see_dialog_text(world: &mut BrowserWorld, expected: String) {
  let log = world
    .get_state::<DialogLog>()
    .cloned()
    .ok_or_else(|| StepError::from("no dialog handler was set -- use 'I accept/dismiss the dialog' first"))?;

  let exp = expected.clone();
  expect_poll(
    move || {
      let l = log.clone();
      let e = exp.clone();
      async move { l.last_message().map_or(false, |m| m.contains(&e)) }
    },
    std::time::Duration::from_secs(5),
  )
  .to_equal(true)
  .await
  .map_err(to_step_err)?;
}

#[then("I should have seen {int} dialog(s)")]
async fn should_have_seen_dialog_count(world: &mut BrowserWorld, expected: i64) {
  let log = world
    .get_state::<DialogLog>()
    .cloned()
    .ok_or_else(|| StepError::from("no dialog handler was set -- use 'I accept/dismiss the dialog' first"))?;

  let expected_count = expected as usize;
  expect_poll(
    move || {
      let l = log.clone();
      async move { l.len() }
    },
    std::time::Duration::from_secs(5),
  )
  .to_equal(expected_count)
  .await
  .map_err(to_step_err)?;
}
