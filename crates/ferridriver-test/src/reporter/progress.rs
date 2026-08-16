//! Progress reporter: compact per-test glyphs plus a live `[done/total]`
//! counter, then the full failure epilogue.
//!
//! `dot` mirrors Playwright's glyph set exactly; this one adds the
//! running count, which is what a long CI job actually wants to see.

use std::fmt::Write as _;

use crate::config::ReportSlowTestsConfig;
use crate::model::{TestOutcomeKind, TestStatus};
use crate::reporter::base::{self, Out, ResultCollector, Screen};
use crate::reporter::{Reporter, ReporterEvent};

pub struct ProgressReporter {
  /// Where this reporter writes. Redirectable so a test can read
  /// back exactly what a run would have printed.
  out: Out,
  screen: Screen,
  collector: ResultCollector,
  slow_tests_config: Option<ReportSlowTestsConfig>,
  count: usize,
  total: usize,
}

impl ProgressReporter {
  #[must_use]
  pub fn new() -> Self {
    Self {
      screen: Screen::detect(),
      collector: ResultCollector::new(),
      slow_tests_config: Some(ReportSlowTestsConfig::default()),
      count: 0,
      out: Out::default(),
      total: 0,
    }
  }

  #[must_use]
  pub fn with_slow_tests_config(mut self, config: Option<ReportSlowTestsConfig>) -> Self {
    self.slow_tests_config = config;
    self
  }

  #[must_use]
  pub fn with_plain_screen(mut self) -> Self {
    self.screen = Screen::plain();
    self
  }
}

impl ProgressReporter {
  /// Send this reporter's output to `out` instead of stdout.
  #[must_use]
  pub fn with_output(mut self, out: Out) -> Self {
    self.out = out;
    self
  }
}

impl Default for ProgressReporter {
  fn default() -> Self {
    Self::new()
  }
}

#[async_trait::async_trait]
impl Reporter for ProgressReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    self.collector.observe(event);
    let screen = self.screen;

    match event {
      ReporterEvent::RunStarted { total_tests, .. } => {
        self.total = *total_tests;
      },
      ReporterEvent::RunError { error } => {
        self
          .out
          .write(&format!("\n{}", screen.red(&base::format_error(screen, error))));
        self.count = 0;
      },
      ReporterEvent::TestFinished { outcome } => {
        if outcome.status.is_failure() && outcome.attempt < outcome.max_attempts {
          return;
        }
        let kind = self
          .collector
          .record_of(outcome)
          .map_or(TestOutcomeKind::Expected, base::TestRecord::outcome_kind);
        let glyph = match kind {
          TestOutcomeKind::Expected => screen.green("."),
          TestOutcomeKind::Skipped => screen.dim("S"),
          TestOutcomeKind::Flaky => screen.yellow("~"),
          TestOutcomeKind::Unexpected => {
            if outcome.status == TestStatus::Interrupted {
              screen.red("!")
            } else {
              screen.red("F")
            }
          },
        };
        self.count += 1;
        let mut block = glyph;
        if self.count.is_multiple_of(80) {
          let _ = writeln!(block, " {}", screen.dim(&format!("{}/{}", self.count, self.total)));
        }
        self.out.write_raw(&block);
      },
      ReporterEvent::RunFinished { .. } => {
        let lead = if self.count.is_multiple_of(80) { "\n" } else { "\n\n" };
        self.out.write(&format!(
          "{lead}{}",
          base::epilogue(screen, &self.collector, self.slow_tests_config.as_ref(), true)
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
  use crate::model::{ExpectedStatus, TestFailure, TestId, TestOutcome};
  use crate::reporter::RunStatus;

  fn outcome(name: &str, status: TestStatus) -> Arc<TestOutcome> {
    let failure = (status == TestStatus::Failed || status == TestStatus::TimedOut).then(|| TestFailure {
      message: format!("{name} broke"),
      stack: None,
      diff: None,
      screenshot: None,
    });
    Arc::new(TestOutcome {
      test_id: TestId {
        file: "tests/a.spec.ts".into(),
        suite: None,
        name: name.into(),
        line: Some(3),
        column: Some(1),
      },
      status,
      errors: failure.iter().cloned().collect(),
      error: failure,
      duration: Duration::from_millis(10),
      ..Default::default()
    })
  }

  async fn run(events: Vec<ReporterEvent>) -> String {
    let (out, held) = Out::buffer();
    let mut reporter = ProgressReporter::new().with_plain_screen().with_output(out);
    reporter
      .on_event(&ReporterEvent::RunStarted {
        total_tests: events.len(),
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
        total: events.len(),
        passed: 0,
        failed: 0,
        skipped: 0,
        flaky: 0,
        duration: Duration::from_millis(300),
        status: RunStatus::Failed,
      })
      .await;
    held.lock().expect("buffer").clone()
  }

  /// The glyph row: the first non-empty line that is not the banner.
  /// `dot` prints a `Running N tests` header, `progress` does not.
  fn glyph_row(text: &str) -> String {
    text
      .lines()
      .find(|line| !line.trim().is_empty() && !line.contains("Running "))
      .unwrap_or_default()
      .to_string()
  }

  #[tokio::test]
  async fn one_glyph_per_test_in_outcome_order() {
    let text = run(vec![
      ReporterEvent::TestFinished {
        outcome: outcome("ok", TestStatus::Passed),
      },
      ReporterEvent::TestFinished {
        outcome: outcome("bad", TestStatus::Failed),
      },
      ReporterEvent::TestFinished {
        outcome: outcome("gone", TestStatus::Skipped),
      },
    ])
    .await;
    assert_eq!(glyph_row(&text), ".FS", "glyph row: {text:?}");
  }

  #[tokio::test]
  async fn a_failing_run_still_prints_the_failure_body() {
    // The whole point of the fix: a `dot`/`progress` run used to end with
    // counts and no diagnostics at all.
    let text = run(vec![ReporterEvent::TestFinished {
      outcome: outcome("bad", TestStatus::Failed),
    }])
    .await;
    assert!(text.contains("1) tests/a.spec.ts:3:1 › bad"), "{text}");
    assert!(text.contains("bad broke"), "the error body reaches the user: {text}");
    assert!(text.contains("1 failed"), "{text}");
  }

  #[tokio::test]
  async fn a_declared_failure_is_not_drawn_as_one() {
    let mut known = (*outcome("known", TestStatus::Failed)).clone();
    known.expected_status = ExpectedStatus::Fail;
    let text = run(vec![ReporterEvent::TestFinished {
      outcome: Arc::new(known),
    }])
    .await;
    assert_eq!(glyph_row(&text), ".", "{text:?}");
    assert!(!text.contains("1 failed"), "{text}");
  }

  #[tokio::test]
  async fn a_retrying_attempt_does_not_get_its_own_glyph() {
    let mut first = (*outcome("wobbles", TestStatus::Failed)).clone();
    first.attempt = 1;
    first.max_attempts = 2;
    let mut second = (*outcome("wobbles", TestStatus::Passed)).clone();
    second.attempt = 2;
    second.max_attempts = 2;
    let text = run(vec![
      ReporterEvent::TestFinished {
        outcome: Arc::new(first),
      },
      ReporterEvent::TestFinished {
        outcome: Arc::new(second),
      },
    ])
    .await;
    assert_eq!(glyph_row(&text).chars().count(), 1, "one test, one glyph: {text:?}");
    assert!(text.contains("1 flaky"), "{text}");
  }
}
