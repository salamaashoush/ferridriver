//! `github` reporter — GitHub Actions workflow annotations for every
//! failure, plus a run-summary notice and slow-file warnings. Mirrors
//! `/tmp/playwright/packages/playwright/src/reporters/github.ts`.
//!
//! Annotations point at the line the error actually came from (parsed
//! out of the stack), not at the test's declaration — a failure ten
//! frames deep annotates the frame that threw, which is what makes the
//! inline PR comment useful.

use async_trait::async_trait;

use super::base::{self, Out, ResultCollector, Screen};
use super::{Reporter, ReporterEvent};
use crate::config::ReportSlowTestsConfig;
use crate::model::{TestOutcomeKind, TestStatus};

/// GitHub Actions reporter. Wraps a delegate (typically the terminal
/// reporter) and additionally emits workflow commands so failures show
/// up as inline annotations on the PR.
///
/// The delegate is preserved so users get human-readable output AND
/// CI annotations from the same `--reporter github` flag.
pub struct GithubReporter {
  /// Where the annotations go. Redirectable so a test can read them back.
  out: Out,
  delegate: Box<dyn Reporter>,
  collector: ResultCollector,
  slow_tests_config: Option<ReportSlowTestsConfig>,
  enabled: bool,
  failed_count: usize,
  /// Annotations are read by a machine, never by a terminal.
  screen: Screen,
}

impl GithubReporter {
  /// Wrap a delegate reporter. `enabled` is read from the
  /// `GITHUB_ACTIONS` env var at construction time — outside of CI
  /// the reporter is a transparent passthrough so local runs aren't
  /// polluted with annotation lines.
  #[must_use]
  pub fn new(delegate: Box<dyn Reporter>) -> Self {
    let enabled = std::env::var("GITHUB_ACTIONS").is_ok();
    Self {
      out: Out::default(),
      delegate,
      collector: ResultCollector::new(),
      slow_tests_config: Some(ReportSlowTestsConfig::default()),
      enabled,
      failed_count: 0,
      screen: Screen::plain(),
    }
  }

  /// Send the annotations to `out` instead of stdout.
  #[must_use]
  pub fn with_output(mut self, out: Out) -> Self {
    self.out = out;
    self
  }

  /// Force the annotations on/off — for tests.
  pub fn with_enabled(mut self, enabled: bool) -> Self {
    self.enabled = enabled;
    self
  }

  #[must_use]
  pub fn with_slow_tests_config(mut self, config: Option<ReportSlowTestsConfig>) -> Self {
    self.slow_tests_config = config;
    self
  }

  /// One workflow command, as the line that goes on stdout. Built
  /// rather than printed so the shape is testable — the escaping is
  /// what makes an annotation land on the right file and line.
  #[must_use]
  fn command(kind: &str, message: &str, options: &[(&str, String)]) -> String {
    let config = options
      .iter()
      .filter(|(_, value)| !value.is_empty())
      .map(|(key, value)| format!("{key}={}", escape_property(value)))
      .collect::<Vec<_>>()
      .join(",");
    let message = escape_data(base::strip_ansi(message).as_ref());
    format!("::{kind} {config}::{message}")
  }

  fn log(&self, kind: &str, message: &str, options: &[(&str, String)]) {
    self.out.write(&Self::command(kind, message, options));
  }

  /// Path as the workflow sees it: relative to `GITHUB_WORKSPACE` when
  /// the file sits under it, so the annotation binds to a file in the
  /// checkout rather than an absolute runner path.
  fn workspace_path(file: &str) -> String {
    let Ok(workspace) = std::env::var("GITHUB_WORKSPACE") else {
      return file.to_string();
    };
    std::path::Path::new(file)
      .strip_prefix(&workspace)
      .map_or_else(|_| file.to_string(), |p| p.display().to_string())
  }

  fn annotate_failures(&mut self, key: &base::TestKey) {
    for line in self.failure_commands(key) {
      self.out.write(&line);
    }
  }

  /// The `::error` lines one finished test contributes — one per error
  /// of each attempt, pointing at the frame that threw.
  fn failure_commands(&mut self, key: &base::TestKey) -> Vec<String> {
    let Some(record) = self.collector.record_by_key(key) else {
      return Vec::new();
    };
    match record.outcome_kind() {
      TestOutcomeKind::Unexpected | TestOutcomeKind::Flaky => {},
      TestOutcomeKind::Skipped
        if record
          .attempts
          .iter()
          .any(|a| a.status == TestStatus::Interrupted && a.error.is_some()) => {},
      _ => return Vec::new(),
    }

    self.failed_count += 1;
    let index = self.failed_count;
    let title = base::format_test_title(record.last());
    let header = base::format_test_header(self.screen, record, "  ", Some(index), true);
    let mut annotations: Vec<(String, String, usize, usize)> = Vec::new();
    for attempt in &record.attempts {
      for error in base::attempt_errors(attempt) {
        let location = base::failure_location(error, &attempt.test_id);
        let retry = if attempt.attempt > 1 {
          format!("\n    Retry #{}", attempt.attempt - 1)
        } else {
          String::new()
        };
        let body = format!("{header}{retry}\n{}", base::format_error(self.screen, error));
        annotations.push((body, location.file, location.line, location.column));
      }
    }
    annotations
      .into_iter()
      .map(|(body, file, line, column)| {
        Self::command(
          "error",
          &body,
          &[
            ("file", Self::workspace_path(&file)),
            ("title", title.clone()),
            ("line", line.to_string()),
            ("col", column.to_string()),
          ],
        )
      })
      .collect()
  }

  fn annotate_summary(&self) {
    for line in base::slow_test_lines(self.screen, &self.collector, self.slow_tests_config.as_ref()) {
      let trimmed = line.trim();
      if let Some(rest) = trimmed.strip_prefix("Slow test file: ") {
        let file = rest.split(" (").next().unwrap_or(rest).to_string();
        self.log(
          "warning",
          trimmed,
          &[
            ("title", "Slow Test".to_string()),
            ("file", Self::workspace_path(&file)),
          ],
        );
      }
    }
    let summary = base::summary_message(self.screen, &self.collector);
    if !summary.trim().is_empty() {
      self.log("notice", &summary, &[("title", "Ferridriver Run Summary".to_string())]);
    }
  }
}

#[async_trait]
impl Reporter for GithubReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    if self.enabled {
      self.collector.observe(event);
      match event {
        ReporterEvent::TestFinished { outcome } => {
          // Wait until the test is done retrying, so a flaky test
          // produces one annotation group and not one per attempt.
          if !(outcome.status.is_failure() && outcome.attempt < outcome.max_attempts) {
            let key = base::TestKey::of(outcome);
            self.annotate_failures(&key);
          }
        },
        ReporterEvent::RunError { error } => {
          let message = base::format_error(self.screen, error);
          let location = base::failure_location(error, &crate::model::TestId::default());
          self.log(
            "error",
            &message,
            &[
              ("file", Self::workspace_path(&location.file)),
              ("line", location.line.to_string()),
            ],
          );
        },
        ReporterEvent::RunFinished { .. } => self.annotate_summary(),
        _ => {},
      }
    }
    self.delegate.on_event(event).await;
  }

  async fn finalize(&mut self) -> ferridriver::error::Result<()> {
    self.delegate.finalize().await
  }
}

/// Escaping for a workflow command's message body, per
/// <https://docs.github.com/actions/reference/workflow-commands-for-github-actions>.
fn escape_data(s: &str) -> String {
  s.replace('%', "%25").replace('\r', "%0D").replace('\n', "%0A")
}

/// Property values escape two more characters than message bodies do,
/// because `,` and `:` delimit the property list itself.
fn escape_property(s: &str) -> String {
  escape_data(s).replace(':', "%3A").replace(',', "%2C")
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;

  use super::*;
  use crate::model::{TestFailure, TestId, TestOutcome};
  use crate::reporter::empty::EmptyReporter;

  fn failing() -> Arc<TestOutcome> {
    let failure = TestFailure {
      message: "Error: expect(received).toBe(expected)\n100% off".into(),
      stack: Some("    at check (tests/pay.spec.ts:88:13)".into()),
      diff: None,
      screenshot: None,
    };
    Arc::new(TestOutcome {
      test_id: TestId {
        file: "tests/pay.spec.ts".into(),
        suite: None,
        name: "charges the card".into(),
        line: Some(80),
        column: Some(1),
      },
      status: TestStatus::Failed,
      errors: vec![failure.clone()],
      error: Some(failure),
      project_name: "chromium".into(),
      ..Default::default()
    })
  }

  #[tokio::test]
  async fn an_annotation_points_at_the_throwing_frame() {
    let mut reporter = GithubReporter::new(Box::new(EmptyReporter)).with_enabled(true);
    let outcome = failing();
    reporter
      .on_event(&ReporterEvent::TestFinished {
        outcome: Arc::clone(&outcome),
      })
      .await;
    let lines = reporter.failure_commands(&base::TestKey::of(&outcome));

    assert_eq!(lines.len(), 1, "{lines:?}");
    let line = &lines[0];
    assert!(line.starts_with("::error "), "{line}");
    assert!(
      line.contains("file=tests/pay.spec.ts"),
      "the error's file, not the test's: {line}"
    );
    assert!(
      line.contains("line=88"),
      "the frame that threw, not the declaration: {line}"
    );
    assert!(line.contains("col=13"), "{line}");
    assert!(
      line.contains("title=[chromium] › tests/pay.spec.ts%3A80%3A1 › charges the card"),
      "colons in a property value are escaped: {line}"
    );
    assert!(
      line.contains("%0A") && !line.contains('\n'),
      "the body is a single line with encoded newlines: {line}"
    );
    assert!(line.contains("100%25"), "a percent sign is escaped first: {line}");
  }

  #[tokio::test]
  async fn a_passing_test_is_not_annotated() {
    let mut reporter = GithubReporter::new(Box::new(EmptyReporter)).with_enabled(true);
    let outcome = Arc::new(TestOutcome {
      test_id: TestId {
        file: "tests/pay.spec.ts".into(),
        name: "works".into(),
        ..Default::default()
      },
      ..Default::default()
    });
    reporter
      .on_event(&ReporterEvent::TestFinished {
        outcome: Arc::clone(&outcome),
      })
      .await;
    assert!(reporter.failure_commands(&base::TestKey::of(&outcome)).is_empty());
  }

  #[test]
  fn property_values_escape_the_delimiters_a_message_does_not() {
    assert_eq!(escape_data("a%b\nc"), "a%25b%0Ac");
    assert_eq!(escape_property("a:b,c"), "a%3Ab%2Cc");
  }
}
