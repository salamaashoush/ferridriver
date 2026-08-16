//! `line` reporter — one self-rewriting status line for the whole run,
//! with failures printed above it as they happen. Mirrors Playwright's
//! `/tmp/playwright/packages/playwright/src/reporters/line.ts`.
//!
//! On a TTY the status line is erased and redrawn in place, so a run of
//! ten thousand tests scrolls nothing until something fails. Without a
//! TTY (CI logs, a pipe) each update is simply appended — the cursor
//! movement would render as garbage.

use std::fmt::Write as _;

use crate::config::ReportSlowTestsConfig;
use crate::model::{TestOutcomeKind, TestStatus};
use crate::reporter::base::{self, Out, ResultCollector, Screen};
use crate::reporter::{Reporter, ReporterEvent};

/// Move up one line and clear it — how the status line is replaced.
const ERASE_LINE: &str = "\u{1b}[1A\u{1b}[2K";

pub struct LineReporter {
  /// Where this reporter writes. Redirectable so a test can read
  /// back exactly what a run would have printed.
  out: Out,
  screen: Screen,
  collector: ResultCollector,
  slow_tests_config: Option<ReportSlowTestsConfig>,
  total: usize,
  current: usize,
  failures: usize,
  /// Whether a status line is on screen and must be erased before
  /// anything else is written.
  line_pending: bool,
  tty: bool,
  /// Test the last streamed output chunk belonged to, so consecutive
  /// chunks from one test share a header.
  last_output_test: Option<String>,
}

impl LineReporter {
  #[must_use]
  pub fn new() -> Self {
    Self {
      screen: Screen::detect(),
      collector: ResultCollector::new(),
      slow_tests_config: Some(ReportSlowTestsConfig::default()),
      total: 0,
      current: 0,
      failures: 0,
      line_pending: false,
      tty: console::Term::stdout().is_term(),
      out: Out::default(),
      last_output_test: None,
    }
  }

  #[must_use]
  pub fn with_slow_tests_config(mut self, config: Option<ReportSlowTestsConfig>) -> Self {
    self.slow_tests_config = config;
    self
  }

  /// Render as if writing to a pipe: no colors, no cursor movement.
  #[must_use]
  pub fn with_plain_screen(mut self) -> Self {
    self.screen = Screen::plain();
    self.tty = false;
    self
  }

  /// The escape that takes back the status line, when there is one to
  /// take back and a terminal that understands it.
  fn erase(&mut self) -> &'static str {
    if self.tty && self.line_pending {
      self.line_pending = false;
      ERASE_LINE
    } else {
      ""
    }
  }

  fn status_line(&mut self, title: &str, retry: u32) -> String {
    let erase = self.erase();
    let retries = if retry > 0 { " (retries)" } else { "" };
    let prefix = format!("[{}/{}]{retries} ", self.current, self.total);
    let suffix = if retry > 0 {
      self.screen.yellow(&format!(" (retry #{retry})"))
    } else {
      String::new()
    };
    let width = self.screen.width.saturating_sub(prefix.chars().count());
    let fitted = base::truncate_chars(title, width.max(10));
    self.line_pending = true;
    format!("{erase}{prefix}{fitted}{suffix}\n")
  }
}

impl LineReporter {
  /// Send this reporter's output to `out` instead of stdout.
  #[must_use]
  pub fn with_output(mut self, out: Out) -> Self {
    self.out = out;
    self
  }
}

impl Default for LineReporter {
  fn default() -> Self {
    Self::new()
  }
}

#[async_trait::async_trait]
impl Reporter for LineReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    self.collector.observe(event);
    let screen = self.screen;

    match event {
      ReporterEvent::RunStarted {
        total_tests,
        num_workers,
        ..
      } => {
        self.total = *total_tests;
        self.out.write(&format!(
          "{}\n",
          base::starting_message(screen, *total_tests, *num_workers)
        ));
      },

      ReporterEvent::TestStarted { test_id, attempt, .. } => {
        self.current += 1;
        let title = test_id.full_name();
        let line = self.status_line(&title, attempt.saturating_sub(1));
        self.out.write_raw(&line);
      },

      ReporterEvent::TestOutput(output) => {
        let erase = self.erase().to_string();
        let mut block = erase;
        let name = output.test_id.full_name();
        if self.last_output_test.as_deref() != Some(name.as_str()) {
          let _ = writeln!(block, "{}", screen.dim(&name));
          self.last_output_test = Some(name);
        }
        block.push_str(&output.text);
        if !block.ends_with('\n') {
          block.push('\n');
        }
        self.out.write_raw(&block);
      },

      ReporterEvent::RunError { error } => {
        let erase = self.erase().to_string();
        self
          .out
          .write(&format!("{erase}{}\n", screen.red(&base::format_error(screen, error))));
      },

      ReporterEvent::TestFinished { outcome } => {
        // A failure only prints once the test is done retrying —
        // otherwise every attempt of a flaky test claims a number.
        let will_retry = outcome.status.is_failure() && outcome.attempt < outcome.max_attempts;
        if will_retry {
          return;
        }
        let Some(record) = self.collector.record_of(outcome) else {
          return;
        };
        let kind = record.outcome_kind();
        if !matches!(kind, TestOutcomeKind::Unexpected | TestOutcomeKind::Flaky)
          && outcome.status != TestStatus::Interrupted
        {
          return;
        }
        self.failures += 1;
        let body = base::format_failure(screen, record, Some(self.failures));
        let erase = self.erase().to_string();
        self.out.write(&format!("{erase}{body}"));
      },

      ReporterEvent::RunFinished { .. } => {
        let erase = self.erase().to_string();
        // The failure bodies already went out above the status line as
        // each test ended; the epilogue only re-states the counts.
        self.out.write(&format!(
          "{erase}{}",
          base::epilogue(screen, &self.collector, self.slow_tests_config.as_ref(), false)
        ));
      },

      _ => {},
    }
  }
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;
  use std::time::Duration;

  use super::*;
  use crate::model::{TestFailure, TestId, TestOutcome};
  use crate::reporter::{RunStatus, TestOutputEvent};

  fn id(name: &str) -> TestId {
    TestId {
      file: "tests/pay.spec.ts".into(),
      suite: None,
      name: name.into(),
      line: Some(11),
      column: Some(1),
    }
  }

  fn passing(name: &str) -> Arc<TestOutcome> {
    Arc::new(TestOutcome {
      test_id: id(name),
      duration: Duration::from_millis(40),
      ..Default::default()
    })
  }

  fn failing(name: &str) -> Arc<TestOutcome> {
    let failure = TestFailure {
      message: "card declined".into(),
      stack: None,
      diff: None,
      screenshot: None,
    };
    Arc::new(TestOutcome {
      test_id: id(name),
      status: TestStatus::Failed,
      errors: vec![failure.clone()],
      error: Some(failure),
      ..Default::default()
    })
  }

  async fn run(events: Vec<ReporterEvent>) -> String {
    let (out, held) = Out::buffer();
    let mut reporter = LineReporter::new().with_plain_screen().with_output(out);
    reporter
      .on_event(&ReporterEvent::RunStarted {
        total_tests: 2,
        num_workers: 1,
        metadata: serde_json::Value::Null,
        start_time: std::time::SystemTime::UNIX_EPOCH,
      })
      .await;
    for event in &events {
      reporter.on_event(event).await;
    }
    reporter
      .on_event(&ReporterEvent::RunFinished {
        total: 2,
        passed: 1,
        failed: 1,
        skipped: 0,
        flaky: 0,
        duration: Duration::from_millis(500),
        status: RunStatus::Failed,
      })
      .await;
    held.lock().expect("buffer").clone()
  }

  #[tokio::test]
  async fn the_status_line_counts_through_the_run() {
    let text = run(vec![
      ReporterEvent::TestStarted {
        test_id: id("charges"),
        attempt: 1,
        worker_id: 0,
      },
      ReporterEvent::TestFinished {
        outcome: passing("charges"),
      },
      ReporterEvent::TestStarted {
        test_id: id("refunds"),
        attempt: 1,
        worker_id: 0,
      },
      ReporterEvent::TestFinished {
        outcome: passing("refunds"),
      },
    ])
    .await;

    assert!(text.contains("[1/2] tests/pay.spec.ts > charges"), "{text}");
    assert!(text.contains("[2/2] tests/pay.spec.ts > refunds"), "{text}");
    // Without a TTY there is no cursor movement to garble a CI log.
    assert!(!text.contains('\u{1b}'), "no escapes off a terminal: {text:?}");
  }

  #[tokio::test]
  async fn a_retry_is_labelled_on_the_status_line() {
    let text = run(vec![ReporterEvent::TestStarted {
      test_id: id("charges"),
      attempt: 2,
      worker_id: 0,
    }])
    .await;
    assert!(text.contains("(retries)"), "{text}");
    assert!(text.contains("(retry #1)"), "{text}");
  }

  #[tokio::test]
  async fn a_failure_prints_a_numbered_body_as_it_happens() {
    let text = run(vec![ReporterEvent::TestFinished {
      outcome: failing("charges"),
    }])
    .await;
    assert!(text.contains("1) tests/pay.spec.ts:11:1 › charges"), "{text}");
    assert!(text.contains("card declined"), "{text}");
    assert!(
      text.contains("1 failed"),
      "the epilogue still states the counts: {text}"
    );
  }

  #[tokio::test]
  async fn a_test_still_being_retried_does_not_claim_a_number() {
    let mut first = (*failing("charges")).clone();
    first.attempt = 1;
    first.max_attempts = 2;
    let text = run(vec![ReporterEvent::TestFinished {
      outcome: Arc::new(first),
    }])
    .await;
    assert!(
      !text.contains("1) tests/pay.spec.ts"),
      "the failure body waits for the last attempt: {text}"
    );
  }

  #[tokio::test]
  async fn streamed_output_is_printed_under_its_test() {
    let text = run(vec![
      ReporterEvent::TestOutput(Arc::new(TestOutputEvent {
        test_id: id("charges"),
        stderr: false,
        text: "talking to the gateway\n".into(),
      })),
      ReporterEvent::TestOutput(Arc::new(TestOutputEvent {
        test_id: id("charges"),
        stderr: false,
        text: "still going\n".into(),
      })),
    ])
    .await;
    assert!(text.contains("talking to the gateway"), "{text}");
    assert!(text.contains("still going"), "{text}");
    assert_eq!(
      text.matches("tests/pay.spec.ts > charges").count(),
      1,
      "consecutive chunks share one header: {text}"
    );
  }
}
