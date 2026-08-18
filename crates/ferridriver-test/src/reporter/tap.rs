//! TAP reporter — Test Anything Protocol version 13.
//!
//! Two shapes, both valid TAP13:
//!
//! - `tap` (nested) emits a subtest block per file, the way `node:test`
//!   and Mocha's TAP reporter group a suite.
//! - `tap-flat` emits one flat plan of every test, which is what most
//!   TAP consumers (`tap-spec`, `faucet`, Jenkins' TAP plugin) parse
//!   most reliably.
//!
//! Failure detail rides in the YAML diagnostic block, which is where a
//! TAP consumer looks for `message`, `severity` and `at`.

use std::fmt::Write as _;

use crate::model::{TestOutcomeKind, TestStatus};
use crate::reporter::base::{self, Out, ResultCollector, TestRecord};
use crate::reporter::{Reporter, ReporterEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TapStyle {
  /// A subtest block per file.
  Nested,
  /// One flat plan for the whole run.
  Flat,
}

pub struct TapReporter {
  /// Where this reporter writes. Redirectable so a test can read
  /// back exactly what a run would have printed.
  out: Out,
  style: TapStyle,
  collector: ResultCollector,
}

impl TapReporter {
  #[must_use]
  pub fn new(style: TapStyle) -> Self {
    Self {
      style,
      out: Out::default(),
      collector: ResultCollector::new(),
    }
  }

  fn render(&self) -> String {
    let mut out = String::from("TAP version 13\n");
    match self.style {
      TapStyle::Flat => {
        let records = self.collector.records();
        let _ = writeln!(out, "1..{}", records.len());
        for (i, record) in records.iter().enumerate() {
          out.push_str(&point(record, i + 1, ""));
        }
      },
      TapStyle::Nested => {
        let files = self.collector.by_file();
        let _ = writeln!(out, "1..{}", files.len());
        for (i, (file, records)) in files.iter().enumerate() {
          let _ = writeln!(out, "# Subtest: {file}");
          let _ = writeln!(out, "    1..{}", records.len());
          for (j, record) in records.iter().enumerate() {
            out.push_str(&point(record, j + 1, "    "));
          }
          let failed = records.iter().any(|r| !r.ok());
          let status = if failed { "not ok" } else { "ok" };
          let _ = writeln!(out, "{status} {} - {file}", i + 1);
        }
      },
    }

    let counts = self.collector.counts();
    let total = counts.expected + counts.unexpected + counts.flaky + counts.skipped;
    let _ = writeln!(out, "# tests {total}");
    let _ = writeln!(out, "# pass {}", counts.expected + counts.flaky);
    let _ = writeln!(out, "# fail {}", counts.unexpected);
    let _ = writeln!(out, "# skip {}", counts.skipped);
    let _ = writeln!(out, "# flaky {}", counts.flaky);
    let _ = writeln!(out, "# duration_ms {}", self.collector.run.duration.as_millis());
    for error in &self.collector.errors {
      let _ = writeln!(out, "Bail out! {}", one_line(&error.message));
    }
    out
  }
}

/// One TAP test point plus its diagnostic block.
fn point(record: &TestRecord, number: usize, pad: &str) -> String {
  let last = record.last();
  let kind = record.outcome_kind();
  let ok = if kind == TestOutcomeKind::Unexpected {
    "not ok"
  } else {
    "ok"
  };
  let title = base::format_test_title(last);
  let directive = match kind {
    TestOutcomeKind::Skipped => " # SKIP".to_string(),
    // A flaky test passed, but silently dropping that fact loses the
    // only signal a TAP consumer has for it.
    TestOutcomeKind::Flaky => format!(" # TODO flaky, passed on attempt {}", last.attempt),
    _ => String::new(),
  };
  let mut out = format!("{pad}{ok} {number} - {}{directive}\n", one_line(&title));

  if kind == TestOutcomeKind::Unexpected {
    let error = base::attempt_errors(last).into_iter().next();
    let location = error.map(|e| base::failure_location(e, &last.test_id));
    let _ = writeln!(out, "{pad}  ---");
    let _ = writeln!(
      out,
      "{pad}  message: {}",
      yaml_scalar(&base::strip_ansi(error.map_or("test failed", |e| e.message.as_str())))
    );
    let _ = writeln!(out, "{pad}  severity: fail");
    if let Some(location) = location {
      let _ = writeln!(out, "{pad}  at:");
      let _ = writeln!(out, "{pad}    file: {}", yaml_scalar(&location.file));
      let _ = writeln!(out, "{pad}    line: {}", location.line);
      let _ = writeln!(out, "{pad}    column: {}", location.column);
    }
    if last.status == TestStatus::TimedOut {
      let _ = writeln!(out, "{pad}  timeout_ms: {}", last.timeout.as_millis());
    }
    if let Some(stack) = error
      .and_then(|e| e.stack.as_deref())
      .filter(|stack| !stack.trim().is_empty())
    {
      let _ = writeln!(out, "{pad}  stack: |-");
      for line in base::strip_ansi(stack).lines() {
        let _ = writeln!(out, "{pad}    {line}");
      }
    }
    let _ = writeln!(out, "{pad}  duration_ms: {}", last.duration.as_millis());
    let _ = writeln!(out, "{pad}  ...");
  }
  out
}

/// TAP descriptions are single-line: a `#` would start a directive and a
/// newline would end the point.
fn one_line(text: &str) -> String {
  base::strip_ansi(text)
    .replace(['\r', '\n'], " ")
    .replace('#', "\u{ff03}")
}

/// A double-quoted YAML scalar, which needs no block-style analysis.
fn yaml_scalar(text: &str) -> String {
  format!(
    "\"{}\"",
    text.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
  )
}

#[async_trait::async_trait]
impl Reporter for TapReporter {
  fn prints_to_stdio(&self) -> bool {
    true
  }

  async fn on_event(&mut self, event: &ReporterEvent) {
    self.collector.observe(event);
    // A bail-out has to reach the consumer immediately: a run that dies
    // never reaches `finalize`.
    if let ReporterEvent::RunError { error } = event {
      self.out.write(&format!("Bail out! {}", one_line(&error.message)));
    }
  }

  async fn finalize(&mut self) -> ferridriver::error::Result<()> {
    self.out.write(&self.render());
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;

  use super::*;
  use crate::model::{TestFailure, TestId, TestOutcome};

  fn finished(name: &str, status: TestStatus, error: Option<&str>) -> ReporterEvent {
    ReporterEvent::TestFinished {
      outcome: Arc::new(TestOutcome {
        test_id: TestId {
          file: "spec.ts".into(),
          suite: None,
          name: name.into(),
          line: Some(7),
          column: Some(1),
        },
        status,
        error: error.map(|message| TestFailure {
          message: message.into(),
          stack: Some("at spec.ts:7:1".into()),
          diff: None,
          screenshot: None,
        }),
        ..Default::default()
      }),
    }
  }

  #[tokio::test]
  async fn a_failure_carries_a_yaml_block_naming_the_line() {
    let mut reporter = TapReporter::new(TapStyle::Flat);
    reporter.on_event(&finished("passes", TestStatus::Passed, None)).await;
    reporter
      .on_event(&finished("breaks", TestStatus::Failed, Some("boom")))
      .await;
    let out = reporter.render();
    assert!(out.starts_with("TAP version 13\n1..2\n"), "{out}");
    assert!(out.contains("ok 1 - spec.ts:7:1 › passes"), "{out}");
    assert!(out.contains("not ok 2 - spec.ts:7:1 › breaks"), "{out}");
    assert!(out.contains("  message: \"boom\""), "{out}");
    assert!(out.contains("    line: 7"), "{out}");
    assert!(out.contains("# fail 1"), "{out}");
  }

  #[tokio::test]
  async fn a_skip_gets_the_tap_directive() {
    let mut reporter = TapReporter::new(TapStyle::Flat);
    reporter.on_event(&finished("gone", TestStatus::Skipped, None)).await;
    assert!(reporter.render().contains("ok 1 - spec.ts:7:1 › gone # SKIP"));
  }

  #[tokio::test]
  async fn nested_style_wraps_each_file_in_a_subtest() {
    let mut reporter = TapReporter::new(TapStyle::Nested);
    reporter.on_event(&finished("a", TestStatus::Passed, None)).await;
    reporter.on_event(&finished("b", TestStatus::Passed, None)).await;
    let out = reporter.render();
    assert!(out.contains("# Subtest: spec.ts"), "{out}");
    assert!(out.contains("    1..2"), "{out}");
    assert!(out.contains("ok 1 - spec.ts"), "{out}");
  }
}
