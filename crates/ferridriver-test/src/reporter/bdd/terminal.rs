//! BDD terminal reporter: Gherkin-formatted output with Feature > Scenario > Step hierarchy.
//!
//! Uses proper Unicode icons, colored status indicators, and clear visual hierarchy.

use std::time::Duration;

use console::Style;

use crate::model::{StepCategory, TestStatus};
use crate::reporter::{Reporter, ReporterEvent};

pub struct BddTerminalReporter {
  current_suite: Option<String>,
}

impl BddTerminalReporter {
  pub fn new() -> Self {
    Self { current_suite: None }
  }
}

impl Default for BddTerminalReporter {
  fn default() -> Self {
    Self::new()
  }
}

// ── Styles ──

fn s_pass() -> Style { Style::new().green() }
fn s_fail() -> Style { Style::new().red().bold() }
fn s_skip() -> Style { Style::new().dim() }
fn s_dim() -> Style { Style::new().dim() }
fn s_bold() -> Style { Style::new().bold() }
fn s_cyan() -> Style { Style::new().cyan().bold() }
fn s_feature() -> Style { Style::new().magenta().bold() }

fn format_duration(d: Duration) -> String {
  let ms = d.as_millis();
  if ms < 1000 { format!("{ms}ms") } else { format!("{:.1}s", d.as_secs_f64()) }
}

#[async_trait::async_trait]
impl Reporter for BddTerminalReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    match event {
      ReporterEvent::RunStarted { total_tests, num_workers } => {
        println!();
        println!("  {} Running {} scenario(s) with {} worker(s)",
          s_cyan().apply_to("\u{25b6}"), // play icon
          s_bold().apply_to(total_tests),
          num_workers,
        );
        println!();
      }

      ReporterEvent::TestStarted { test_id, attempt } => {
        // Feature header — print when suite changes.
        if self.current_suite.as_ref() != test_id.suite.as_ref() {
          if self.current_suite.is_some() {
            println!();
          }
          if let Some(suite) = &test_id.suite {
            println!("  {} {}",
              s_feature().apply_to("Feature:"),
              s_bold().apply_to(suite),
            );
          }
          self.current_suite = test_id.suite.clone();
        }

        let retry = if *attempt > 1 {
          format!(" {}", s_dim().apply_to(format!("(retry #{})", attempt)))
        } else {
          String::new()
        };
        println!("    {} {}{}",
          s_dim().apply_to("\u{25cf}"), // filled circle (running indicator)
          &test_id.name,
          retry,
        );
      }

      ReporterEvent::StepFinished(ev) => {
        if !ev.category.is_visible() {
          return;
        }
        let dur = format_duration(ev.duration);

        // Hook steps get a distinct style.
        if ev.category == StepCategory::Hook {
          let icon = if ev.error.is_some() { "\u{2717}" } else { "\u{2713}" };
          let style = if ev.error.is_some() { s_fail() } else { s_dim() };
          println!("      {} {} {}",
            style.apply_to(icon),
            s_dim().apply_to(format!("[{}]", ev.title)),
            s_dim().apply_to(format!("({dur})")),
          );
          if let Some(err) = &ev.error {
            for line in err.lines() {
              println!("        {}", s_fail().apply_to(line));
            }
          }
          return;
        }

        // BDD step: extract keyword from metadata for coloring.
        let keyword = ev.metadata.as_ref()
          .and_then(|m| m.get("bdd_keyword"))
          .and_then(|v| v.as_str())
          .map(|k| k.trim().to_string());

        if ev.error.is_some() {
          println!("      {} {} {}",
            s_fail().apply_to("\u{2717}"),
            s_fail().apply_to(&ev.title),
            s_dim().apply_to(format!("({dur})")),
          );
          if let Some(err) = &ev.error {
            for line in err.lines() {
              println!("        {}", s_fail().apply_to(line));
            }
          }
        } else if let Some(kw) = &keyword {
          // Color the keyword part, rest in default.
          let rest = ev.title.strip_prefix(kw.as_str()).unwrap_or(&ev.title);
          println!("      {} {}{} {}",
            s_pass().apply_to("\u{2713}"),
            s_cyan().apply_to(kw),
            rest,
            s_dim().apply_to(format!("({dur})")),
          );
        } else {
          println!("      {} {} {}",
            s_pass().apply_to("\u{2713}"),
            &ev.title,
            s_dim().apply_to(format!("({dur})")),
          );
        }
      }

      ReporterEvent::TestFinished { outcome, .. } => {
        // Re-print the scenario line with final status (overwrite the running indicator).
        // Move cursor up past the steps + the original scenario line, then reprint.
        // Actually, for simplicity in a streaming terminal, we just print the skipped indicator.
        if outcome.status == TestStatus::Skipped {
          // For skipped scenarios that had no steps printed.
          println!("      {} {}",
            s_skip().apply_to("\u{2212}"), // minus
            s_skip().apply_to("skipped"),
          );
        }
      }

      ReporterEvent::RunFinished { total, passed, failed, skipped, flaky, duration } => {
        let dur = format_duration(*duration);
        println!();

        // Summary line with colored counts and pipe separators.
        let mut parts = Vec::new();
        if *passed > 0 {
          parts.push(format!("{}", s_pass().apply_to(format!("{passed} passed"))));
        }
        if *failed > 0 {
          parts.push(format!("{}", s_fail().apply_to(format!("{failed} failed"))));
        }
        if *flaky > 0 {
          parts.push(format!("{}", Style::new().yellow().bold().apply_to(format!("{flaky} flaky"))));
        }
        if *skipped > 0 {
          parts.push(format!("{}", s_skip().apply_to(format!("{skipped} skipped"))));
        }

        println!("  {} {}: {} {}",
          s_bold().apply_to("Scenarios"),
          s_dim().apply_to(format!("{total} total")),
          parts.join(&format!("{}", s_dim().apply_to(" | "))),
          s_dim().apply_to(format!("({dur})")),
        );
        println!();
      }

      _ => {}
    }
  }
}
