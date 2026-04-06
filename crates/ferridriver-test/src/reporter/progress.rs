//! Progress reporter: minimal dot-based output (one character per test).

use std::io::Write;
use std::time::Duration;

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

#[async_trait::async_trait]
impl Reporter for ProgressReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    match event {
      ReporterEvent::TestFinished { outcome, .. } => {
        let ch = match outcome.status {
          TestStatus::Passed => '.',
          TestStatus::Failed | TestStatus::TimedOut => 'F',
          TestStatus::Skipped => 'S',
          TestStatus::Flaky => '?',
          TestStatus::Interrupted => '!',
        };
        print!("{ch}");
        self.count += 1;
        // Line wrap every 80 chars.
        if self.count % 80 == 0 {
          println!();
        }
        let _ = std::io::stdout().flush();
      }
      ReporterEvent::RunFinished {
        total,
        passed,
        failed,
        skipped,
        flaky,
        duration,
      } => {
        if self.count % 80 != 0 {
          println!();
        }
        println!();
        println!(
          "{total} test(s): {passed} passed, {failed} failed, {skipped} skipped, {flaky} flaky ({})",
          format_duration(*duration)
        );
      }
      _ => {}
    }
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
