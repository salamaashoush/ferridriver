//! `list` reporter: one line per finished test, then the failure
//! epilogue. Unified output for E2E and BDD tests.
//!
//! Automatically detects BDD tests by checking step metadata for `bdd_keyword`.
//! E2E tests show as flat results. BDD tests show Feature > Scenario > Step hierarchy
//! with keyword coloring.
//!
//! The failure bodies, summary counts and slow-file warning come from
//! [`crate::reporter::base`], so `list`, `line`, `dot`, `progress` and
//! `github` all end a run the same way.

use std::fmt::Write as _;

use crate::config::ReportSlowTestsConfig;
use crate::model::{StepCategory, StepStatus, TestOutcomeKind, TestStatus, TestStep};
use crate::reporter::base::{self, Out, ResultCollector, Screen};
use crate::reporter::{Reporter, ReporterEvent};

pub struct TerminalReporter {
  /// Where this reporter writes. Redirectable so a test can read
  /// back exactly what a run would have printed.
  out: Out,
  screen: Screen,
  collector: ResultCollector,
  slow_tests_config: Option<ReportSlowTestsConfig>,
  /// Current BDD feature/suite — used to print Feature headers when suite changes.
  current_suite: Option<String>,
}

impl TerminalReporter {
  pub fn new() -> Self {
    Self {
      screen: Screen::detect(),
      collector: ResultCollector::new(),
      slow_tests_config: Some(ReportSlowTestsConfig::default()),
      out: Out::default(),
      current_suite: None,
    }
  }

  pub fn with_slow_tests_config(mut self, config: Option<ReportSlowTestsConfig>) -> Self {
    self.slow_tests_config = config;
    self
  }

  /// Render without ANSI escapes and at a fixed width — for tests and
  /// for output captured into a file.
  #[must_use]
  pub fn with_plain_screen(mut self) -> Self {
    self.screen = Screen::plain();
    self
  }
}

impl TerminalReporter {
  /// Send this reporter's output to `out` instead of stdout.
  #[must_use]
  pub fn with_output(mut self, out: Out) -> Self {
    self.out = out;
    self
  }
}

impl Default for TerminalReporter {
  fn default() -> Self {
    Self::new()
  }
}

const PASS_MARK: &str = "\u{2713}";
const FAIL_MARK: &str = "\u{2717}";
const SKIP_MARK: &str = "\u{2212}";
const FLAKY_MARK: &str = "\u{25ce}";

fn step_icon(status: StepStatus) -> &'static str {
  match status {
    StepStatus::Passed => "\u{2713}",
    StepStatus::Failed => "\u{2717}",
    StepStatus::Skipped => "\u{2212}",
    StepStatus::Pending => "\u{25cb}",
  }
}

/// Check if a test outcome has BDD steps (any step with bdd_keyword metadata).
fn is_bdd_test(steps: &[TestStep]) -> bool {
  steps
    .iter()
    .any(|s| s.metadata.as_ref().is_some_and(|m| m.get("bdd_keyword").is_some()) || is_bdd_test(&s.steps))
}

fn write_steps(out: &mut String, screen: Screen, steps: &[&TestStep], indent: usize) {
  let pad = " ".repeat(indent);
  for step in steps {
    if step.category == StepCategory::Hook {
      let failed = step.error.is_some();
      let icon = if failed { "\u{2717}" } else { "\u{2713}" };
      let icon = if failed { screen.red(icon) } else { screen.dim(icon) };
      let _ = writeln!(
        out,
        "{pad}{icon} {} {}",
        screen.dim(&format!("[{}]", step.title)),
        screen.dim(&format!("({})", base::ms_to_string(step.duration))),
      );
      if let Some(err) = &step.error {
        for line in err.lines() {
          let _ = writeln!(out, "{pad}  {}", screen.red(line));
        }
      }
      continue;
    }

    let icon = step_icon(step.status);
    let dur = screen.dim(&format!("({})", base::ms_to_string(step.duration)));

    // BDD steps: color the keyword part in cyan.
    let keyword = step
      .metadata
      .as_ref()
      .and_then(|m| m.get("bdd_keyword"))
      .and_then(|v| v.as_str())
      .map(str::trim);

    match step.status {
      StepStatus::Passed => {
        let icon = screen.green(icon);
        match keyword {
          Some(kw) => {
            let rest = step.title.strip_prefix(kw).unwrap_or(&step.title);
            let _ = writeln!(out, "{pad}{icon} {}{rest} {dur}", screen.bold(kw));
          },
          None => {
            let _ = writeln!(out, "{pad}{icon} {} {dur}", step.title);
          },
        }
      },
      StepStatus::Failed => {
        let _ = writeln!(out, "{pad}{} {} {dur}", screen.red(icon), screen.red(&step.title));
        if let Some(err) = &step.error {
          for line in err.lines() {
            let _ = writeln!(out, "{pad}  {}", screen.red(line));
          }
        }
      },
      StepStatus::Skipped | StepStatus::Pending => {
        let _ = writeln!(out, "{pad}{} {}", screen.dim(icon), screen.dim(&step.title));
      },
    }

    let nested: Vec<&TestStep> = step.steps.iter().filter(|s| s.category.is_visible()).collect();
    if !nested.is_empty() {
      write_steps(out, screen, &nested, indent + 2);
    }
  }
}

#[async_trait::async_trait]
impl Reporter for TerminalReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    self.collector.observe(event);
    let screen = self.screen;

    match event {
      ReporterEvent::RunStarted {
        total_tests,
        num_workers,
        ..
      } => {
        self.out.write(&format!(
          "{}\n",
          base::starting_message(screen, *total_tests, *num_workers)
        ));
      },

      ReporterEvent::RunError { error } => {
        let mut out = String::new();
        let _ = writeln!(out, "\n{}", screen.red(&base::format_error(screen, error)));
        self.out.write(&out);
      },

      ReporterEvent::TestFinished { outcome } => {
        let test_id = &outcome.test_id;
        // One block per test: parallel workers finish concurrently, and
        // a line-at-a-time write lets two tests interleave mid-report.
        let mut out = String::new();
        let bdd = is_bdd_test(&outcome.steps);

        // BDD: print Feature header when suite changes.
        if bdd && self.current_suite.as_ref() != test_id.suite.as_ref() {
          if self.current_suite.is_some() {
            out.push('\n');
          }
          if let Some(suite) = &test_id.suite {
            let _ = writeln!(out, "  {} {}", screen.bold("Feature:"), screen.bold(suite));
          }
          self.current_suite.clone_from(&test_id.suite);
        }

        // The mark describes the test, not the raw status: a `test.fail()`
        // test that duly failed reads as a pass, the way Playwright's list
        // reporter draws it.
        let kind = self
          .collector
          .record_of(outcome)
          .map_or(TestOutcomeKind::Expected, base::TestRecord::outcome_kind);
        let duration = base::ms_to_string(outcome.duration);
        // Every project runs the same titles; without its name the lines
        // are indistinguishable (Playwright prefixes them the same way).
        let title = if outcome.project_name.is_empty() {
          test_id.full_name()
        } else {
          format!("[{}] {}", outcome.project_name, test_id.full_name())
        };

        match kind {
          TestOutcomeKind::Expected => {
            let _ = writeln!(
              out,
              "  {} {title} {}",
              screen.green(PASS_MARK),
              screen.dim(&format!("({duration})"))
            );
          },
          TestOutcomeKind::Unexpected if outcome.status == TestStatus::Interrupted => {
            let _ = writeln!(out, "  {} {title}", screen.red("!"));
          },
          TestOutcomeKind::Unexpected => {
            let _ = writeln!(
              out,
              "  {} {} {}",
              screen.red(FAIL_MARK),
              screen.red(&title),
              screen.dim(&format!("({duration})"))
            );
          },
          TestOutcomeKind::Skipped => {
            let _ = writeln!(out, "  {} {}", screen.dim(SKIP_MARK), screen.dim(&title));
          },
          TestOutcomeKind::Flaky => {
            let _ = writeln!(
              out,
              "  {} {} {}",
              screen.yellow(FLAKY_MARK),
              screen.yellow(&title),
              screen.dim(&format!("({duration}) [flaky]"))
            );
          },
        }

        // Only show step details for failed/timed-out tests. Passing tests
        // (including expected-failure @fail tests whose outcome was inverted)
        // don't need step-level output. Matches Playwright's list reporter
        // which hides steps by default.
        if kind == TestOutcomeKind::Unexpected {
          let user_steps: Vec<&TestStep> = outcome.steps.iter().filter(|s| s.category.is_visible()).collect();
          if !user_steps.is_empty() {
            write_steps(&mut out, screen, &user_steps, 4);
          }
        }
        self.out.write(&out);
      },

      ReporterEvent::RunFinished { .. } => {
        self.out.write(&base::epilogue(
          screen,
          &self.collector,
          self.slow_tests_config.as_ref(),
          true,
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
  use crate::reporter::{RunStatus, TestOutputEvent};

  fn id(name: &str) -> TestId {
    TestId {
      file: "tests/cart.spec.ts".into(),
      suite: None,
      name: name.into(),
      line: Some(7),
      column: Some(2),
    }
  }

  fn outcome(name: &str, status: TestStatus) -> Arc<TestOutcome> {
    Arc::new(TestOutcome {
      test_id: id(name),
      status,
      duration: Duration::from_millis(120),
      ..Default::default()
    })
  }

  fn failing(name: &str, message: &str) -> Arc<TestOutcome> {
    let failure = TestFailure {
      message: message.into(),
      stack: Some("    at check (tests/cart.spec.ts:19:5)".into()),
      diff: None,
      screenshot: None,
    };
    Arc::new(TestOutcome {
      test_id: id(name),
      status: TestStatus::Failed,
      duration: Duration::from_millis(30),
      errors: vec![failure.clone()],
      error: Some(failure),
      stderr: "something went wrong\n".into(),
      ..Default::default()
    })
  }

  /// Drive a whole run and return everything the reporter printed.
  async fn run(events: Vec<ReporterEvent>) -> String {
    let (out, held) = Out::buffer();
    let mut reporter = TerminalReporter::new().with_plain_screen().with_output(out);
    reporter
      .on_event(&ReporterEvent::RunStarted {
        total_tests: events.len(),
        num_workers: 2,
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
        duration: Duration::from_millis(900),
        status: RunStatus::Failed,
      })
      .await;
    held.lock().expect("buffer").clone()
  }

  #[tokio::test]
  async fn a_run_prints_a_line_per_test_then_the_failure_epilogue() {
    let text = run(vec![
      ReporterEvent::TestFinished {
        outcome: outcome("adds an item", TestStatus::Passed),
      },
      ReporterEvent::TestFinished {
        outcome: failing("removes an item", "expect(cart).toBeEmpty() failed"),
      },
      ReporterEvent::TestFinished {
        outcome: outcome("skipped one", TestStatus::Skipped),
      },
    ])
    .await;

    assert!(text.contains("Running 3 tests using 2 workers"), "{text}");
    assert!(
      text.contains("\u{2713} tests/cart.spec.ts > adds an item (120ms)"),
      "{text}"
    );
    assert!(text.contains("\u{2717} tests/cart.spec.ts > removes an item"), "{text}");
    assert!(text.contains("\u{2212} tests/cart.spec.ts > skipped one"), "{text}");

    // The epilogue: numbered body, the error, the captured stderr, counts.
    assert!(
      text.contains("1) tests/cart.spec.ts:7:2 › removes an item"),
      "numbered failure header missing: {text}"
    );
    assert!(text.contains("expect(cart).toBeEmpty() failed"), "{text}");
    assert!(text.contains("something went wrong"), "captured stderr missing: {text}");
    assert!(text.contains("1 failed"), "{text}");
    assert!(text.contains("1 skipped"), "{text}");
    assert!(text.contains("1 passed"), "{text}");
  }

  #[tokio::test]
  async fn a_declared_failure_reads_as_a_pass() {
    let mut known = (*failing("known bug", "boom")).clone();
    known.expected_status = ExpectedStatus::Fail;
    let text = run(vec![ReporterEvent::TestFinished {
      outcome: Arc::new(known),
    }])
    .await;

    assert!(
      text.contains("\u{2713} tests/cart.spec.ts > known bug"),
      "a test.fail() test that failed is a pass: {text}"
    );
    assert!(!text.contains("1 failed"), "and is not counted as a failure: {text}");
    assert!(text.contains("1 passed"), "{text}");
  }

  #[tokio::test]
  async fn a_flaky_test_is_marked_once_with_both_attempts_kept() {
    let mut first = (*failing("wobbles", "boom")).clone();
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

    assert!(text.contains("1 flaky"), "{text}");
    assert!(!text.contains("1 failed"), "flaky is not failed: {text}");
  }

  #[tokio::test]
  async fn a_bdd_run_prints_a_feature_header_and_keyword_coloured_steps() {
    let mut scenario = (*outcome("signs in", TestStatus::Passed)).clone();
    scenario.test_id.file = "features/login.feature".into();
    scenario.test_id.suite = Some("Login".into());
    scenario.steps = vec![TestStep {
      step_id: "s1".into(),
      title: "Given a user".into(),
      category: StepCategory::TestStep,
      duration: Duration::from_millis(4),
      status: StepStatus::Passed,
      error: None,
      location: None,
      annotations: Vec::new(),
      parent_step_id: None,
      metadata: Some(serde_json::json!({ "bdd_keyword": "Given" })),
      steps: Vec::new(),
    }];

    let text = run(vec![ReporterEvent::TestFinished {
      outcome: Arc::new(scenario),
    }])
    .await;
    assert!(text.contains("Feature: Login"), "{text}");
  }

  #[tokio::test]
  async fn a_run_error_is_printed_and_counted() {
    let text = run(vec![ReporterEvent::RunError {
      error: Box::new(TestFailure {
        message: "global setup failed: boom".into(),
        stack: None,
        diff: None,
        screenshot: None,
      }),
    }])
    .await;
    assert!(text.contains("global setup failed: boom"), "{text}");
    assert!(
      text.contains("1 error was not a part of any test"),
      "the summary accounts for it: {text}"
    );
  }

  #[tokio::test]
  async fn live_output_does_not_disturb_the_per_test_lines() {
    // `list` buffers output onto the outcome rather than streaming it;
    // the live event must not produce a second copy.
    let text = run(vec![
      ReporterEvent::TestOutput(Arc::new(TestOutputEvent {
        test_id: id("adds an item"),
        stderr: false,
        text: "chatter\n".into(),
      })),
      ReporterEvent::TestFinished {
        outcome: outcome("adds an item", TestStatus::Passed),
      },
    ])
    .await;
    assert!(!text.contains("chatter"), "list does not stream output: {text}");
  }
}
