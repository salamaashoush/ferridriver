//! BDD terminal reporter: Gherkin-formatted output with Feature > Scenario > Step hierarchy.
//!
//! Implements `ferridriver_test::reporter::Reporter`.

use std::time::Duration;

use console::Style;

use ferridriver_test::model::{StepCategory, TestStatus};
use ferridriver_test::reporter::{Reporter, ReporterEvent};

pub struct BddTerminalReporter {
  current_suite: Option<String>,
  pass_style: Style,
  fail_style: Style,
  skip_style: Style,
  dim_style: Style,
  bold_style: Style,
}

impl BddTerminalReporter {
  pub fn new() -> Self {
    Self {
      current_suite: None,
      pass_style: Style::new().green(),
      fail_style: Style::new().red().bold(),
      skip_style: Style::new().yellow(),
      dim_style: Style::new().dim(),
      bold_style: Style::new().bold(),
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

impl Default for BddTerminalReporter {
  fn default() -> Self {
    Self::new()
  }
}

#[async_trait::async_trait]
impl Reporter for BddTerminalReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    match event {
      ReporterEvent::RunStarted {
        total_tests,
        num_workers,
      } => {
        println!();
        println!(
          "  {}",
          self.bold_style
            .apply_to(format!("Running {total_tests} scenario(s) with {num_workers} worker(s)"))
        );
        println!();
      }

      ReporterEvent::TestStarted { test_id, attempt } => {
        if self.current_suite.as_ref() != test_id.suite.as_ref() {
          if self.current_suite.is_some() {
            println!();
          }
          if let Some(suite) = &test_id.suite {
            println!("  {}", self.bold_style.apply_to(format!("Feature: {suite}")));
          }
          self.current_suite = test_id.suite.clone();
        }

        let retry = if *attempt > 0 {
          format!(" (retry #{})", attempt)
        } else {
          String::new()
        };
        println!("    Scenario: {}{}", test_id.name, retry);
      }

      ReporterEvent::StepFinished(ev) => {
        if ev.category != StepCategory::TestStep {
          return;
        }

        let dur = Self::format_duration(ev.duration);

        if ev.error.is_some() {
          println!(
            "      {} {} {}",
            self.fail_style.apply_to("x"),
            self.fail_style.apply_to(&ev.title),
            self.dim_style.apply_to(format!("({dur})"))
          );
          if let Some(err) = &ev.error {
            for line in err.lines() {
              println!("        {}", self.fail_style.apply_to(line));
            }
          }
        } else {
          println!(
            "      {} {} {}",
            self.pass_style.apply_to("v"),
            &ev.title,
            self.dim_style.apply_to(format!("({dur})"))
          );
        }
      }

      ReporterEvent::TestFinished { outcome, .. } => {
        if outcome.status == TestStatus::Skipped {
          println!("      {}", self.skip_style.apply_to("- skipped"));
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
        let mut parts = Vec::new();
        if *passed > 0 {
          parts.push(format!("{}", self.pass_style.apply_to(format!("{passed} passed"))));
        }
        if *flaky > 0 {
          parts.push(format!(
            "{}",
            Style::new().yellow().bold().apply_to(format!("{flaky} flaky"))
          ));
        }
        if *failed > 0 {
          parts.push(format!("{}", self.fail_style.apply_to(format!("{failed} failed"))));
        }
        if *skipped > 0 {
          parts.push(format!("{}", self.skip_style.apply_to(format!("{skipped} skipped"))));
        }
        let dur = Self::format_duration(*duration);
        println!(
          "  {} scenario(s): {} {}",
          self.bold_style.apply_to(total.to_string()),
          parts.join(", "),
          self.dim_style.apply_to(format!("({dur})"))
        );
        println!();
      }

      _ => {}
    }
  }
}
