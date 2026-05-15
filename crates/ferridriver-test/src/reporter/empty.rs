//! `null` reporter — drops every event. Useful for tests that want
//! the runner to drive a single TS-side reporter without competing
//! terminal output. Mirrors Playwright's
//! `/tmp/playwright/packages/playwright/src/reporters/empty.ts`.

use async_trait::async_trait;

use super::{Reporter, ReporterEvent};

/// Reporter that produces no output and writes no files.
pub struct EmptyReporter;

#[async_trait]
impl Reporter for EmptyReporter {
  async fn on_event(&mut self, _event: &ReporterEvent) {}

  async fn finalize(&mut self) -> ferridriver::error::Result<()> {
    Ok(())
  }
}
