//! Markdown summary reporter.
//!
//! Writes a run summary suitable for pasting into a PR or appending to
//! `$GITHUB_STEP_SUMMARY` — counts, a per-file table, and a collapsed
//! block per failure carrying the error and the step trail.
//!
//! When `GITHUB_STEP_SUMMARY` is set the same text is appended there as
//! well, so `--reporter markdown` in a workflow renders the summary on
//! the job page with no extra step.

use std::fmt::Write as _;
use std::path::PathBuf;

use crate::model::{TestOutcomeKind, TestStep};
use crate::reporter::base::{self, ResultCollector, Screen, TestRecord};
use crate::reporter::{Reporter, ReporterEvent};

pub struct MarkdownReporter {
  output_path: PathBuf,
  collector: ResultCollector,
  title: String,
}

impl MarkdownReporter {
  #[must_use]
  pub fn new(output_path: PathBuf) -> Self {
    Self {
      output_path,
      collector: ResultCollector::new(),
      title: "Test results".to_string(),
    }
  }

  #[must_use]
  pub fn with_title(mut self, title: String) -> Self {
    self.title = title;
    self
  }

  fn render(&self) -> String {
    let counts = self.collector.counts();
    let mut out = String::new();
    let _ = writeln!(out, "## {}\n", self.title);
    let _ = writeln!(
      out,
      "| Passed | Failed | Flaky | Skipped | Duration |\n|---:|---:|---:|---:|---:|"
    );
    let _ = writeln!(
      out,
      "| {} | {} | {} | {} | {} |\n",
      counts.expected,
      counts.unexpected,
      counts.flaky,
      counts.skipped,
      base::ms_to_string(self.collector.run.duration)
    );

    if !self.collector.errors.is_empty() {
      let _ = writeln!(out, "### Errors outside any test\n");
      for error in &self.collector.errors {
        let _ = writeln!(out, "```\n{}\n```\n", base::strip_ansi(&error.message));
      }
    }

    let failures = self.collector.failures_to_print();
    if !failures.is_empty() {
      let _ = writeln!(out, "### Failures\n");
      for record in &failures {
        out.push_str(&self.render_failure(record));
      }
    }

    let _ = writeln!(out, "### All tests\n");
    for (file, records) in self.collector.by_file() {
      let _ = writeln!(
        out,
        "<details><summary>{} ({} tests)</summary>\n",
        escape(&file),
        records.len()
      );
      let _ = writeln!(out, "| | Test | Project | Time |\n|:-:|---|---|---:|");
      for record in records {
        let _ = writeln!(
          out,
          "| {} | {} | {} | {} |",
          icon(record.outcome_kind()),
          escape(&record.id().name),
          escape(&record.key.project),
          base::ms_to_string(record.total_duration()),
        );
      }
      let _ = writeln!(out, "\n</details>\n");
    }
    out
  }

  fn render_failure(&self, record: &TestRecord) -> String {
    let mut out = String::new();
    let title = base::format_test_title(record.last());
    let _ = writeln!(
      out,
      "<details><summary>{} {}</summary>\n",
      icon(record.outcome_kind()),
      escape(&title)
    );
    for attempt in &record.attempts {
      let errors = base::attempt_errors(attempt);
      if errors.is_empty() {
        continue;
      }
      if attempt.attempt > 1 {
        let _ = writeln!(out, "**Retry #{}**\n", attempt.attempt - 1);
      }
      for error in errors {
        let _ = writeln!(out, "```\n{}\n```\n", base::format_error(Screen::plain(), error));
      }
      let steps = step_trail(&attempt.steps, 0);
      if !steps.is_empty() {
        let _ = writeln!(out, "```\n{steps}```\n");
      }
    }
    let _ = writeln!(out, "</details>\n");
    out
  }
}

fn icon(kind: TestOutcomeKind) -> &'static str {
  match kind {
    TestOutcomeKind::Expected => "pass",
    TestOutcomeKind::Unexpected => "**FAIL**",
    TestOutcomeKind::Flaky => "flaky",
    TestOutcomeKind::Skipped => "skip",
  }
}

fn step_trail(steps: &[TestStep], depth: usize) -> String {
  let mut out = String::new();
  for step in steps.iter().filter(|s| s.category.is_visible()) {
    let mark = if step.error.is_some() { "x" } else { "v" };
    let _ = writeln!(
      out,
      "{}{mark} {} ({}ms)",
      "  ".repeat(depth),
      step.title,
      step.duration.as_millis()
    );
    out.push_str(&step_trail(&step.steps, depth + 1));
  }
  out
}

/// A test title lands inside a markdown table cell; `|` would split it
/// and a stray tag would be parsed as HTML.
fn escape(text: &str) -> String {
  base::strip_ansi(text)
    .replace('|', "\\|")
    .replace('<', "&lt;")
    .replace('>', "&gt;")
    .replace(['\r', '\n'], " ")
}

#[async_trait::async_trait]
impl Reporter for MarkdownReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    self.collector.observe(event);
  }

  async fn finalize(&mut self) -> ferridriver::error::Result<()> {
    let text = self.render();
    if let Some(parent) = self.output_path.parent() {
      std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&self.output_path, &text)?;

    // Appended, not written: a workflow step may already have put
    // something on the summary page.
    if let Ok(summary) = std::env::var("GITHUB_STEP_SUMMARY")
      && !summary.is_empty()
    {
      use std::io::Write as _;
      match std::fs::OpenOptions::new().create(true).append(true).open(&summary) {
        Ok(mut file) => {
          if let Err(e) = file.write_all(text.as_bytes()) {
            tracing::warn!("could not append to GITHUB_STEP_SUMMARY: {e}");
          }
        },
        Err(e) => tracing::warn!("could not open GITHUB_STEP_SUMMARY: {e}"),
      }
    }

    tracing::info!("Markdown report written to {}", self.output_path.display());
    Ok(())
  }
}
