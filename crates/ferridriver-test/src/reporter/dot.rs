//! Dot reporter — single-character status per test, line-wrapped at
//! 80 columns. Mirrors Playwright's `dot` reporter at
//! `/tmp/playwright/packages/playwright/src/reporters/dot.ts`.

use async_trait::async_trait;

use super::{Reporter, ReporterEvent};
use crate::model::TestStatus;

/// Renders one character per finished test:
///   `·` pass, `F` fail, `T` timeout, `S` skip, `±` flaky.
/// Line-wraps at 80 characters. Prints a final newline + summary on
/// `RunFinished`.
pub struct DotReporter {
  counter: usize,
  total: usize,
}

impl DotReporter {
  #[must_use]
  pub fn new() -> Self {
    Self { counter: 0, total: 0 }
  }
}

impl Default for DotReporter {
  fn default() -> Self {
    Self::new()
  }
}

#[async_trait]
impl Reporter for DotReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    match event {
      ReporterEvent::RunStarted { total_tests, .. } => {
        self.total = *total_tests;
      },
      ReporterEvent::TestFinished { outcome, .. } => {
        if self.counter == 80 {
          println!();
          self.counter = 0;
        }
        self.counter += 1;
        let glyph = match outcome.status {
          TestStatus::Passed => "·",
          TestStatus::Failed => "F",
          TestStatus::TimedOut => "T",
          TestStatus::Skipped => "S",
          TestStatus::Flaky => "±",
          TestStatus::Interrupted => "I",
        };
        print!("{glyph}");
        use std::io::Write;
        let _ = std::io::stdout().flush();
      },
      ReporterEvent::RunFinished {
        total,
        passed,
        failed,
        skipped,
        flaky,
        duration,
      } => {
        println!();
        println!(
          "{passed} passed · {failed} failed · {skipped} skipped · {flaky} flaky · {total} total · {:?}",
          duration
        );
      },
      _ => {},
    }
  }

  async fn finalize(&mut self) -> ferridriver::error::Result<()> {
    Ok(())
  }
}
