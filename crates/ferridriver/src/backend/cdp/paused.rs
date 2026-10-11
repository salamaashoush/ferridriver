//! Setup of a popup that attached paused on `waitForDebuggerOnStart`.
//!
//! A popup sharing its opener's process answers session commands while
//! paused. One in a new browsing instance (`noopener`, a COOP swap) has
//! no renderer yet and answers nothing until resumed, but it applies
//! everything sent before the resume, in order, ahead of its first
//! document. So a setup command is written and its answer left for the
//! resume to collect, the wire order Playwright's
//! `FrameSession._initialize` batch produces (`crPage.ts`).

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::task::Poll;

use rustc_hash::FxHashMap;

use crate::error::Result;

/// An answer still owed by a paused popup, and what to do with it.
pub(super) type PendingAnswer = Pin<Box<dyn Future<Output = Result<()>> + Send>>;

/// How long a command that needs its answer waits on a paused popup
/// before the popup is taken for one that answers nothing and resumed.
pub(super) const ANSWER_GRACE: std::time::Duration = std::time::Duration::from_secs(1);

#[derive(Default)]
pub(super) struct PausedSetup {
  paused: AtomicBool,
  answers: std::sync::Mutex<Vec<PendingAnswer>>,
  next_script: AtomicU64,
  /// The protocol identifier of each init script registered while
  /// paused, by the identifier handed out before the answer came.
  scripts: std::sync::Mutex<FxHashMap<String, String>>,
  /// The main frame's URL as the frame tree written during the pause
  /// reported it.
  main_frame_url: std::sync::OnceLock<String>,
}

impl PausedSetup {
  pub(super) fn paused() -> Self {
    Self {
      paused: AtomicBool::new(true),
      ..Self::default()
    }
  }

  pub(super) fn is_paused(&self) -> bool {
    self.paused.load(Ordering::Acquire)
  }

  /// Write `command` now and keep its answer for the resume. The first
  /// poll is what writes it, so the wire keeps the order of the calls.
  pub(super) async fn write(&self, mut command: PendingAnswer) -> Result<()> {
    match futures::poll!(command.as_mut()) {
      Poll::Ready(result) => result,
      Poll::Pending => {
        self
          .answers
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner)
          .push(command);
        Ok(())
      },
    }
  }

  /// End the pause, handing back the answers still owed.
  pub(super) fn resume(&self) -> Vec<PendingAnswer> {
    self.paused.store(false, Ordering::Release);
    std::mem::take(&mut *self.answers.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
  }

  pub(super) fn main_frame_url_answered(&self, url: &str) {
    let _ = self.main_frame_url.set(url.to_string());
  }

  /// Whether the popup may still be on the empty document it starts
  /// with, which Chromium reports with an empty URL (`:` before
  /// r1651606). A popup opened for `about:blank` in a new browsing
  /// instance is past it by its first answer and commits nothing more.
  pub(super) fn on_initial_document(&self) -> bool {
    self.main_frame_url.get().is_none_or(|url| url.is_empty() || url == ":")
  }

  /// An identifier for an init script whose protocol one is still owed.
  pub(super) fn script_identifier(&self) -> String {
    format!("paused-{}", self.next_script.fetch_add(1, Ordering::Relaxed))
  }

  pub(super) fn script_answered(&self, identifier: String, protocol: String) {
    self
      .scripts
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .insert(identifier, protocol);
  }

  /// The identifier the protocol knows `identifier` by.
  pub(super) fn protocol_script(&self, identifier: &str) -> String {
    self
      .scripts
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .remove(identifier)
      .unwrap_or_else(|| identifier.to_string())
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::error::FerriError;

  #[tokio::test]
  async fn an_unanswered_command_is_written_and_left_for_the_resume() {
    let setup = PausedSetup::paused();
    let (written, was_written) = tokio::sync::oneshot::channel();
    let (answer, answered) = tokio::sync::oneshot::channel::<()>();
    setup
      .write(Box::pin(async move {
        let _ = written.send(());
        answered.await.map_err(|_| FerriError::backend("dropped"))
      }))
      .await
      .unwrap();
    was_written.await.unwrap();
    assert!(setup.is_paused());

    let owed = setup.resume();
    assert!(!setup.is_paused());
    assert_eq!(owed.len(), 1);
    answer.send(()).unwrap();
    for answer in owed {
      answer.await.unwrap();
    }
  }

  #[tokio::test]
  async fn an_answered_command_reports_at_once() {
    let setup = PausedSetup::paused();
    setup.write(Box::pin(async { Ok(()) })).await.unwrap();
    let error = setup
      .write(Box::pin(async { Err(FerriError::backend("refused")) }))
      .await
      .unwrap_err();
    assert!(error.to_string().contains("refused"));
    assert!(setup.resume().is_empty());
  }

  #[test]
  fn a_popup_is_on_its_initial_document_until_its_frame_tree_names_another() {
    for (url, initial) in [
      ("", true),
      (":", true),
      ("about:blank", false),
      ("https://example.com/", false),
    ] {
      let setup = PausedSetup::paused();
      assert!(setup.on_initial_document());
      setup.main_frame_url_answered(url);
      assert_eq!(setup.on_initial_document(), initial, "{url:?}");
    }
  }

  #[test]
  fn a_script_keeps_its_identifier_until_the_protocol_names_it() {
    let setup = PausedSetup::paused();
    let ours = setup.script_identifier();
    assert_ne!(ours, setup.script_identifier());
    assert_eq!(setup.protocol_script(&ours), ours);
    setup.script_answered(ours.clone(), "7".to_string());
    assert_eq!(setup.protocol_script(&ours), "7");
  }
}
