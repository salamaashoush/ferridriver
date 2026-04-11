//! TUI reporter: translates ReporterEvents into TuiMessages for the dashboard.
//!
//! Converts the raw event stream into state-update messages that the WatchTui
//! dashboard uses to update test entries in-place (pending -> running -> passed/failed).

use tokio::sync::mpsc;

use crate::model::TestStatus;
use crate::reporter::ReporterEvent;
use crate::tui::{EntryStatus, TestEntry, TuiMessage};

pub struct TuiReporter {
  tx: mpsc::UnboundedSender<TuiMessage>,
  has_bdd: bool,
  /// Accumulated test names from discovery (built during RunStarted).
  pending_names: Vec<TestEntry>,
}

impl TuiReporter {
  pub fn new(tx: mpsc::UnboundedSender<TuiMessage>, has_bdd: bool) -> Self {
    Self {
      tx,
      has_bdd,
      pending_names: Vec::new(),
    }
  }

  fn send(&self, msg: TuiMessage) {
    let _ = self.tx.send(msg);
  }

  /// Format a test name for display. BDD tests (identified by suite starting
  /// with a feature path) get "Scenario: " prefix.
  fn display_name(&self, test_id: &crate::model::TestId) -> String {
    if self.has_bdd && test_id.suite.as_ref().is_some_and(|s| s.ends_with(".feature")) {
      format!("Scenario: {}", test_id.name)
    } else {
      test_id.full_name()
    }
  }
}

#[async_trait::async_trait]
impl crate::reporter::Reporter for TuiReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    match event {
      ReporterEvent::RunStarted {
        total_tests,
        num_workers,
        ..
      } => {
        // We don't have the test names yet at RunStarted — they arrive via TestStarted.
        // Pre-populate with placeholders that will be updated.
        self.pending_names.clear();
        self.send(TuiMessage::RunStarted {
          total: *total_tests,
          workers: *num_workers,
          names: Vec::new(), // Empty — tests will appear as they start.
        });
      },

      ReporterEvent::TestStarted { test_id, .. } => {
        let name = self.display_name(test_id);
        self.send(TuiMessage::TestStarted { name });
      },

      ReporterEvent::StepStarted(step) => {
        if step.category.is_visible() || self.has_bdd {
          let test_name = self.display_name(&step.test_id);
          self.send(TuiMessage::StepUpdate {
            test_name,
            step_title: step.title.clone(),
            status: EntryStatus::Running,
            duration_ms: None,
          });
        }
      },

      ReporterEvent::StepFinished(step) => {
        if step.category.is_visible() || self.has_bdd {
          let test_name = self.display_name(&step.test_id);
          let status = if step.error.is_some() {
            EntryStatus::Failed
          } else {
            EntryStatus::Passed
          };
          self.send(TuiMessage::StepUpdate {
            test_name,
            step_title: step.title.clone(),
            status,
            duration_ms: Some(step.duration.as_millis() as u64),
          });
        }
      },

      ReporterEvent::TestFinished { test_id, outcome } => {
        let name = self.display_name(test_id);
        let status = match outcome.status {
          TestStatus::Passed => EntryStatus::Passed,
          TestStatus::Failed | TestStatus::TimedOut => EntryStatus::Failed,
          TestStatus::Skipped => EntryStatus::Skipped,
          TestStatus::Flaky => EntryStatus::Flaky,
          TestStatus::Interrupted => EntryStatus::Failed,
        };
        self.send(TuiMessage::TestFinished {
          name,
          status,
          duration: outcome.duration,
          error: outcome.error.as_ref().map(|e| e.message.clone()),
        });
      },

      ReporterEvent::RunFinished {
        passed,
        failed,
        skipped,
        flaky,
        duration,
        ..
      } => {
        self.send(TuiMessage::RunFinished {
          passed: *passed,
          failed: *failed,
          skipped: *skipped,
          flaky: *flaky,
          duration: *duration,
        });
      },

      _ => {},
    }
  }

  async fn finalize(&mut self) -> Result<(), String> {
    Ok(())
  }
}
