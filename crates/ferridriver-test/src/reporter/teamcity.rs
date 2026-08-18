//! TeamCity reporter — JetBrains service messages, streamed live.
//!
//! TeamCity and the IntelliJ/WebStorm test runners build their test tree
//! from `##teamcity[...]` lines on stdout as they arrive, so this
//! reporter emits per event rather than at finalize: the tree fills in
//! while the run is still going, and a killed run keeps whatever it
//! already reported.
//!
//! Protocol: <https://www.jetbrains.com/help/teamcity/service-messages.html>

use std::fmt::Write as _;

use crate::model::{TestStatus, TestStep};
use crate::reporter::base::{self, Out, ResultCollector, Screen};
use crate::reporter::{Reporter, ReporterEvent};

pub struct TeamCityReporter {
  /// Where this reporter writes. Redirectable so a test can read
  /// back exactly what a run would have printed.
  out: Out,
  collector: ResultCollector,
  /// Suite currently open, so a file change closes the previous one.
  open_suite: Option<String>,
  /// `flowId` keeps parallel workers' messages from interleaving into
  /// one another's test in the TeamCity tree.
  screen: Screen,
}

impl TeamCityReporter {
  #[must_use]
  pub fn new() -> Self {
    Self {
      collector: ResultCollector::new(),
      open_suite: None,
      out: Out::default(),
      screen: Screen::plain(),
    }
  }
}

impl TeamCityReporter {
  /// Send this reporter's output to `out` instead of stdout.
  #[must_use]
  pub fn with_output(mut self, out: Out) -> Self {
    self.out = out;
    self
  }
}

impl Default for TeamCityReporter {
  fn default() -> Self {
    Self::new()
  }
}

/// Service-message escaping: the five characters TeamCity reserves.
fn tc(value: &str) -> String {
  let mut out = String::with_capacity(value.len());
  for c in base::strip_ansi(value).chars() {
    match c {
      '\'' => out.push_str("|'"),
      '\n' => out.push_str("|n"),
      '\r' => out.push_str("|r"),
      '|' => out.push_str("||"),
      '[' => out.push_str("|["),
      ']' => out.push_str("|]"),
      // Non-BMP characters are escaped as `|0xXXXX` of the first unit.
      c if (c as u32) > 0xffff => {
        let _ = write!(out, "|0x{:04x}", c as u32 & 0xffff);
      },
      c => out.push(c),
    }
  }
  out
}

fn message(name: &str, attrs: &[(&str, String)]) -> String {
  let body = attrs
    .iter()
    .filter(|(_, value)| !value.is_empty())
    .map(|(key, value)| format!("{key}='{}'", tc(value)))
    .collect::<Vec<_>>()
    .join(" ");
  format!("##teamcity[{name} {body}]")
}

/// Flat `test.step` titles, reported as TeamCity's stdout stream so the
/// step trail shows under a failing test.
fn step_lines(steps: &[TestStep], depth: usize, out: &mut String) {
  for step in steps.iter().filter(|s| s.category.is_visible()) {
    let mark = if step.error.is_some() { "x" } else { "v" };
    let _ = writeln!(
      out,
      "{}{mark} {} ({}ms)",
      "  ".repeat(depth),
      step.title,
      step.duration.as_millis()
    );
    step_lines(&step.steps, depth + 1, out);
  }
}

#[async_trait::async_trait]
impl Reporter for TeamCityReporter {
  fn prints_to_stdio(&self) -> bool {
    true
  }

  async fn on_event(&mut self, event: &ReporterEvent) {
    self.collector.observe(event);
    match event {
      ReporterEvent::TestStarted { test_id, .. } => {
        let mut block = String::new();
        if self.open_suite.as_deref() != Some(test_id.file.as_str()) {
          if let Some(open) = self.open_suite.take() {
            let _ = writeln!(block, "{}", message("testSuiteFinished", &[("name", open)]));
          }
          let _ = writeln!(
            block,
            "{}",
            message("testSuiteStarted", &[("name", test_id.file.clone())])
          );
          self.open_suite = Some(test_id.file.clone());
        }
        let _ = writeln!(
          block,
          "{}",
          message(
            "testStarted",
            &[
              ("name", test_id.full_name()),
              ("captureStandardOutput", "false".to_string()),
            ],
          )
        );
        self.out.write_raw(&block);
      },

      ReporterEvent::TestFinished { outcome } => {
        let name = outcome.test_id.full_name();
        let mut block = String::new();

        if outcome.status == TestStatus::Skipped {
          let reason = outcome
            .error
            .as_ref()
            .map(|e| e.message.clone())
            .unwrap_or_else(|| "skipped".to_string());
          let _ = writeln!(
            block,
            "{}",
            message("testIgnored", &[("name", name.clone()), ("message", reason)])
          );
        } else {
          let mut stdout = outcome.stdout.clone();
          if !outcome.steps.is_empty() {
            step_lines(&outcome.steps, 0, &mut stdout);
          }
          if !stdout.is_empty() {
            let _ = writeln!(
              block,
              "{}",
              message("testStdOut", &[("name", name.clone()), ("out", stdout)])
            );
          }
          if !outcome.stderr.is_empty() {
            let _ = writeln!(
              block,
              "{}",
              message("testStdErr", &[("name", name.clone()), ("out", outcome.stderr.clone())])
            );
          }
          if let Some(error) = base::attempt_errors(outcome).into_iter().next() {
            let mut attrs = vec![
              ("name", name.clone()),
              ("message", error.message.lines().next().unwrap_or_default().to_string()),
              ("details", base::format_error(self.screen, error)),
            ];
            // A diff turns the failure into a TeamCity comparison
            // failure, which renders side-by-side in the UI.
            if let Some((expected, actual)) = comparison(error) {
              attrs.push(("type", "comparisonFailure".to_string()));
              attrs.push(("expected", expected));
              attrs.push(("actual", actual));
            }
            let _ = writeln!(block, "{}", message("testFailed", &attrs));
          }
        }

        let _ = writeln!(
          block,
          "{}",
          message(
            "testFinished",
            &[("name", name), ("duration", outcome.duration.as_millis().to_string()),],
          )
        );
        self.out.write_raw(&block);
      },

      ReporterEvent::RunError { error } => {
        self.out.write(&message(
          "message",
          &[
            ("text", error.message.clone()),
            ("errorDetails", error.stack.clone().unwrap_or_default()),
            ("status", "ERROR".to_string()),
          ],
        ));
      },

      ReporterEvent::RunFinished { .. } => {
        let mut block = String::new();
        if let Some(open) = self.open_suite.take() {
          let _ = writeln!(block, "{}", message("testSuiteFinished", &[("name", open)]));
        }
        let counts = self.collector.counts();
        let _ = writeln!(
          block,
          "{}",
          message(
            "message",
            &[(
              "text",
              format!(
                "{} passed, {} failed, {} flaky, {} skipped in {}",
                counts.expected,
                counts.unexpected,
                counts.flaky,
                counts.skipped,
                base::ms_to_string(self.collector.run.duration)
              )
            )],
          )
        );
        self.out.write_raw(&block);
      },

      _ => {},
    }
  }
}

/// The `Expected:` / `Received:` pair out of a rendered assertion body,
/// so TeamCity can show its diff view.
fn comparison(failure: &crate::model::TestFailure) -> Option<(String, String)> {
  // Matchers render the pair into the diff when there is one and inline
  // into the message otherwise; both spellings should light up the
  // side-by-side view.
  let body = failure.diff.as_deref().unwrap_or(&failure.message);
  let plain = base::strip_ansi(body);
  let mut expected: Option<String> = None;
  let mut actual: Option<String> = None;
  for line in plain.lines() {
    let trimmed = line.trim();
    if let Some(rest) = trimmed.strip_prefix("Expected:") {
      expected = Some(rest.trim().to_string());
    } else if let Some(rest) = trimmed.strip_prefix("Received:") {
      actual = Some(rest.trim().to_string());
    }
  }
  Some((expected?, actual?))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn reserved_characters_are_escaped() {
    assert_eq!(tc("a'b|c[d]e\nf"), "a|'b||c|[d|]e|nf");
  }

  #[test]
  fn a_message_drops_empty_attributes() {
    let out = message("testFailed", &[("name", "t".into()), ("details", String::new())]);
    assert_eq!(out, "##teamcity[testFailed name='t']");
  }
}
