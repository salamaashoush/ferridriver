//! Dialog handling step definitions.
//!
//! ferridriver mirrors Playwright's event-based dialog model: you
//! register a `page.on('dialog', ...)` listener before the action that
//! triggers the dialog. These steps install such a listener and record
//! every dialog into a typed log for the assertion steps to read.

use std::sync::{Arc, Mutex};

use crate::step::StepError;
use crate::world::BrowserWorld;
use ferridriver::dialog::Dialog;
use ferridriver::events::PageEvent;
use ferridriver_bdd_macros::{given, then};
use ferridriver_test::expect::{AssertionFailure, expect_poll};

fn to_step_err(e: AssertionFailure) -> StepError {
  StepError {
    message: e.message,
    diff: e.diff.map(|d| (d, String::new())),
    pending: false,
  }
}

/// A recorded dialog snapshot: just the fields the step assertions
/// need. Keeping a pure-data snapshot avoids retaining the live
/// `Dialog` handle past its single-shot accept/dismiss lifetime.
#[derive(Clone, Debug)]
struct DialogRecord {
  message: String,
}

/// Tracks dialog events for assertion steps.
#[derive(Clone, Default)]
struct DialogLog {
  entries: Arc<Mutex<Vec<DialogRecord>>>,
}

impl DialogLog {
  fn push(&self, dialog: DialogRecord) {
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

/// Shared: register a `dialog` listener that records the dialog and
/// responds according to the given closure. The closure receives the
/// live [`Dialog`] and is responsible for calling `accept` or
/// `dismiss`. Each registered listener also pushes a [`DialogRecord`]
/// into the per-world [`DialogLog`] so the `then` steps can make
/// assertions.
fn install_dialog_listener<F>(world: &mut BrowserWorld, respond: F)
where
  F: Fn(Dialog) + Send + Sync + 'static,
{
  let log = ensure_dialog_log(world);
  let respond = Arc::new(respond);
  world.page().events().on(
    "dialog",
    Arc::new(move |event: PageEvent| {
      if let PageEvent::Dialog(dialog) = event {
        log.push(DialogRecord {
          message: dialog.message().to_string(),
        });
        let respond = respond.clone();
        // Dispatch the user's intended response on a tokio task so
        // the broadcast callback stays non-blocking.
        tokio::spawn(async move {
          (respond)(dialog);
        });
      }
    }),
  );
}

#[given("I accept the dialog")]
async fn accept_dialog(world: &mut BrowserWorld) {
  install_dialog_listener(world, |dialog| {
    tokio::spawn(async move {
      let _ = dialog.accept(None).await;
    });
  });
}

#[given("I dismiss the dialog")]
async fn dismiss_dialog(world: &mut BrowserWorld) {
  install_dialog_listener(world, |dialog| {
    tokio::spawn(async move {
      let _ = dialog.dismiss().await;
    });
  });
}

#[given("I type {string} in the dialog")]
async fn type_in_dialog(world: &mut BrowserWorld, text: String) {
  let text = Arc::new(text);
  install_dialog_listener(world, move |dialog| {
    let text = text.clone();
    tokio::spawn(async move {
      let _ = dialog.accept(Some((*text).clone())).await;
    });
  });
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
