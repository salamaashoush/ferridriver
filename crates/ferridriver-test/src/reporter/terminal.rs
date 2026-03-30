//! Rich terminal reporter with colors and live progress.

use std::time::Duration;

use console::Style;

use crate::model::TestStatus;
use crate::reporter::{Reporter, ReporterEvent};

pub struct TerminalReporter {
  completed: usize,
  total: usize,
  pass_style: Style,
  fail_style: Style,
  skip_style: Style,
  flaky_style: Style,
  dim_style: Style,
  bold_style: Style,
}

impl TerminalReporter {
  pub fn new() -> Self {
    Self {
      completed: 0,
      total: 0,
      pass_style: Style::new().green(),
      fail_style: Style::new().red().bold(),
      skip_style: Style::new().yellow(),
      flaky_style: Style::new().yellow().bold(),
      dim_style: Style::new().dim(),
      bold_style: Style::new().bold(),
    }
  }

  fn status_icon(&self, status: &TestStatus) -> &'static str {
    match status {
      TestStatus::Passed => "✓",
      TestStatus::Failed => "✗",
      TestStatus::TimedOut => "⏱",
      TestStatus::Skipped => "−",
      TestStatus::Flaky => "⚠",
      TestStatus::Interrupted => "!",
    }
  }

  fn format_duration(d: Duration) -> String {
    let ms = d.as_millis();
    if ms < 1000 {
      format!("{ms}ms")
    } else {
      format!("{:.1}s", d.as_secs_f64())
    }
  }
}

impl Default for TerminalReporter {
  fn default() -> Self {
    Self::new()
  }
}

#[async_trait::async_trait]
impl Reporter for TerminalReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    match event {
      ReporterEvent::RunStarted {
        total_tests,
        num_workers,
      } => {
        self.total = *total_tests;
        println!(
          "\n{}",
          self
            .bold_style
            .apply_to(format!("Running {total_tests} test(s) with {num_workers} worker(s)"))
        );
        println!();
      }
      ReporterEvent::TestStarted { .. } => {}
      ReporterEvent::TestFinished { test_id, outcome } => {
        self.completed += 1;
        let icon = self.status_icon(&outcome.status);
        let duration = Self::format_duration(outcome.duration);
        let name = &test_id.name;

        let line = match outcome.status {
          TestStatus::Passed => {
            format!(
              "  {} {} {}",
              self.pass_style.apply_to(icon),
              name,
              self.dim_style.apply_to(format!("({duration})"))
            )
          }
          TestStatus::Failed | TestStatus::TimedOut => {
            format!(
              "  {} {} {}",
              self.fail_style.apply_to(icon),
              self.fail_style.apply_to(name),
              self.dim_style.apply_to(format!("({duration})"))
            )
          }
          TestStatus::Skipped => {
            format!(
              "  {} {}",
              self.skip_style.apply_to(icon),
              self.skip_style.apply_to(name)
            )
          }
          TestStatus::Flaky => {
            format!(
              "  {} {} {}",
              self.flaky_style.apply_to(icon),
              self.flaky_style.apply_to(name),
              self.dim_style.apply_to(format!("({duration}) [flaky]"))
            )
          }
          TestStatus::Interrupted => {
            format!("  {} {}", self.fail_style.apply_to(icon), name)
          }
        };
        println!("{line}");

        // Print error details for failures.
        if let Some(error) = &outcome.error {
          println!();
          println!("    {}", self.fail_style.apply_to(&error.message));
          if let Some(diff) = &error.diff {
            for line in diff.lines() {
              println!("    {line}");
            }
          }
          println!();
        }
      }
      ReporterEvent::RunFinished {
        total,
        passed,
        failed,
        skipped,
        flaky,
        duration,
      } => {
        println!();
        let duration_str = Self::format_duration(*duration);
        let mut parts = Vec::new();
        if *passed > 0 {
          parts.push(format!("{}", self.pass_style.apply_to(format!("{passed} passed"))));
        }
        if *failed > 0 {
          parts.push(format!("{}", self.fail_style.apply_to(format!("{failed} failed"))));
        }
        if *flaky > 0 {
          parts.push(format!("{}", self.flaky_style.apply_to(format!("{flaky} flaky"))));
        }
        if *skipped > 0 {
          parts.push(format!("{}", self.skip_style.apply_to(format!("{skipped} skipped"))));
        }
        println!(
          "  {} {} {}",
          self.bold_style.apply_to(format!("{total} test(s):")),
          parts.join(", "),
          self.dim_style.apply_to(format!("({duration_str})"))
        );
        println!();
      }
      _ => {}
    }
  }
}
