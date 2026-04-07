//! Progress reporter: colored dot-based output (one character per test).
//!
//! Compact output for CI/pipelines: green dots for pass, red F for fail,
//! with a colored summary line at the end.

use std::io::Write;
use std::time::Duration;

use console::Style;

use crate::model::TestStatus;
use crate::reporter::{Reporter, ReporterEvent};

pub struct ProgressReporter {
  count: usize,
}

impl ProgressReporter {
  pub fn new() -> Self {
    Self { count: 0 }
  }
}

impl Default for ProgressReporter {
  fn default() -> Self {
    Self::new()
  }
}

fn s_pass() -> Style { Style::new().green() }
fn s_fail() -> Style { Style::new().red().bold() }
fn s_skip() -> Style { Style::new().dim() }
fn s_flaky() -> Style { Style::new().yellow() }
fn s_bold() -> Style { Style::new().bold() }
fn s_dim() -> Style { Style::new().dim() }

fn format_duration(d: Duration) -> String {
  let ms = d.as_millis();
  if ms < 1000 { format!("{ms}ms") } else { format!("{:.1}s", d.as_secs_f64()) }
}

#[async_trait::async_trait]
impl Reporter for ProgressReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    match event {
      ReporterEvent::TestFinished { outcome, .. } => {
        let styled = match outcome.status {
          TestStatus::Passed => s_pass().apply_to("."),
          TestStatus::Failed | TestStatus::TimedOut => s_fail().apply_to("F"),
          TestStatus::Skipped => s_skip().apply_to("S"),
          TestStatus::Flaky => s_flaky().apply_to("~"),
          TestStatus::Interrupted => s_fail().apply_to("!"),
        };
        print!("{styled}");
        self.count += 1;
        if self.count % 80 == 0 {
          println!();
        }
        let _ = std::io::stdout().flush();
      }
      ReporterEvent::RunFinished { total, passed, failed, skipped, flaky, duration } => {
        if self.count % 80 != 0 {
          println!();
        }
        let dur = format_duration(*duration);
        println!();

        let mut parts = Vec::new();
        if *passed > 0 {
          parts.push(format!("{}", s_pass().apply_to(format!("{passed} passed"))));
        }
        if *failed > 0 {
          parts.push(format!("{}", s_fail().apply_to(format!("{failed} failed"))));
        }
        if *flaky > 0 {
          parts.push(format!("{}", s_flaky().apply_to(format!("{flaky} flaky"))));
        }
        if *skipped > 0 {
          parts.push(format!("{}", s_skip().apply_to(format!("{skipped} skipped"))));
        }

        println!("{} {}: {} {}",
          s_bold().apply_to("Tests"),
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
