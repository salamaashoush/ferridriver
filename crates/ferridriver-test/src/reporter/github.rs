//! `github` reporter — emits GitHub Actions `::error` annotations
//! for every failing test in addition to forwarding events to a
//! wrapped reporter. Mirrors Playwright's
//! `/tmp/playwright/packages/playwright/src/reporters/github.ts`.

use async_trait::async_trait;

use super::{Reporter, ReporterEvent};
use crate::model::TestStatus;

/// GitHub Actions reporter. Wraps a delegate (typically the terminal
/// reporter) and additionally emits
/// `::error file=...,line=...,title=...::message` lines so failures
/// show up as inline annotations on the PR.
///
/// The delegate is preserved so users get human-readable output AND
/// CI annotations from the same `--reporter github` flag.
pub struct GithubReporter {
  delegate: Box<dyn Reporter>,
  enabled: bool,
}

impl GithubReporter {
  /// Wrap a delegate reporter. `enabled` is read from the
  /// `GITHUB_ACTIONS` env var at construction time — outside of CI
  /// the reporter is a transparent passthrough so local runs aren't
  /// polluted with annotation lines.
  #[must_use]
  pub fn new(delegate: Box<dyn Reporter>) -> Self {
    let enabled = std::env::var("GITHUB_ACTIONS").is_ok();
    Self { delegate, enabled }
  }

  /// Force the annotations on/off — for tests.
  pub fn with_enabled(mut self, enabled: bool) -> Self {
    self.enabled = enabled;
    self
  }
}

#[async_trait]
impl Reporter for GithubReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    if self.enabled {
      if let ReporterEvent::TestFinished { test_id, outcome } = event {
        if matches!(outcome.status, TestStatus::Failed | TestStatus::TimedOut) {
          let title = test_id.full_name().replace(['\r', '\n'], " ");
          let message = outcome
            .error
            .as_ref()
            .map(|e| escape(&e.message))
            .unwrap_or_else(|| "test failed".to_string());
          let file = test_id.file.replace(['\r', '\n'], " ");
          let line = test_id.line.unwrap_or(1);
          // GitHub Actions workflow command syntax:
          // ::error file={path},line={n},title={title}::{message}
          println!("::error file={file},line={line},title={title}::{message}");
        }
      }
    }
    self.delegate.on_event(event).await;
  }

  async fn finalize(&mut self) -> ferridriver::error::Result<()> {
    self.delegate.finalize().await
  }
}

fn escape(s: &str) -> String {
  // GitHub workflow commands escape `%`, `\r`, and `\n` per
  // https://docs.github.com/en/actions/learn-github-actions/workflow-commands-for-github-actions#example-create-a-warning-message
  s.replace('%', "%25").replace('\r', "%0D").replace('\n', "%0A")
}
