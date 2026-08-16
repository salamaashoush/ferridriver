//! Rerun reporter: writes failed test locations to `@rerun.txt` for re-execution.

use std::path::PathBuf;

use crate::model::TestOutcomeKind;
use crate::reporter::{Reporter, ReporterEvent};

pub struct RerunReporter {
  output_path: PathBuf,
  failed: Vec<String>,
}

impl RerunReporter {
  pub fn new(output_path: PathBuf) -> Self {
    Self {
      output_path,
      failed: Vec::new(),
    }
  }
}

#[async_trait::async_trait]
impl Reporter for RerunReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    // Against the declared expectation, not the raw status: a
    // `test.fail()` test that failed did what it was told to and must not
    // come back on the next `--last-failed`.
    if let ReporterEvent::TestFinished { outcome } = event
      && crate::model::outcome_kind(&[outcome.status], outcome.expected_status) == TestOutcomeKind::Unexpected
    {
      self.failed.push(outcome.test_id.file_location());
    }
  }

  async fn finalize(&mut self) -> ferridriver::error::Result<()> {
    if self.failed.is_empty() {
      return Ok(());
    }

    self.failed.sort();
    self.failed.dedup();

    let content = self.failed.join("\n") + "\n";

    if let Some(parent) = self.output_path.parent() {
      std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&self.output_path, content)?;

    tracing::info!(
      "Rerun file written to {} ({} failed)",
      self.output_path.display(),
      self.failed.len()
    );
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;

  use std::path::Path;

  use super::*;
  use crate::model::{ExpectedStatus, TestId, TestOutcome, TestStatus};

  struct ScopedDir(PathBuf);
  impl Drop for ScopedDir {
    fn drop(&mut self) {
      let _ = std::fs::remove_dir_all(&self.0);
    }
  }

  fn scoped(name: &str) -> ScopedDir {
    let path = std::env::temp_dir().join(format!("ferri-rerun-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("temp dir");
    ScopedDir(path)
  }

  fn outcome(name: &str, line: usize, status: TestStatus) -> Arc<TestOutcome> {
    Arc::new(TestOutcome {
      test_id: TestId {
        file: "tests/a.spec.ts".into(),
        suite: None,
        name: name.into(),
        line: Some(line),
        column: None,
      },
      status,
      ..Default::default()
    })
  }

  async fn write(dir: &Path, outcomes: Vec<Arc<TestOutcome>>) -> Option<String> {
    let path = dir.join("@rerun.txt");
    let mut reporter = RerunReporter::new(path.clone());
    for outcome in outcomes {
      reporter.on_event(&ReporterEvent::TestFinished { outcome }).await;
    }
    reporter.finalize().await.expect("finalize");
    std::fs::read_to_string(&path).ok()
  }

  #[tokio::test]
  async fn only_failures_are_listed_by_location() {
    let dir = scoped("basic");
    let text = write(
      &dir.0,
      vec![
        outcome("ok", 3, TestStatus::Passed),
        outcome("bad", 9, TestStatus::Failed),
        outcome("slow", 12, TestStatus::TimedOut),
        outcome("gone", 15, TestStatus::Skipped),
      ],
    )
    .await
    .expect("@rerun.txt");

    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines, vec!["tests/a.spec.ts:12", "tests/a.spec.ts:9"], "{text}");
  }

  #[tokio::test]
  async fn a_declared_failure_never_comes_back() {
    // The bug this guards: `--last-failed` used to re-run a `test.fail()`
    // test forever, because it matched on the raw status.
    let dir = scoped("expected");
    let mut known = (*outcome("known bug", 9, TestStatus::Failed)).clone();
    known.expected_status = ExpectedStatus::Fail;
    let text = write(&dir.0, vec![Arc::new(known)]).await;
    assert!(text.is_none(), "nothing failed, so no file: {text:?}");
  }

  #[tokio::test]
  async fn a_declared_failure_that_passes_is_listed() {
    let dir = scoped("unexpected-pass");
    let mut wrong = (*outcome("known bug", 9, TestStatus::Passed)).clone();
    wrong.expected_status = ExpectedStatus::Fail;
    let text = write(&dir.0, vec![Arc::new(wrong)]).await.expect("@rerun.txt");
    assert_eq!(text.trim(), "tests/a.spec.ts:9");
  }

  #[tokio::test]
  async fn repeated_attempts_of_one_test_are_written_once() {
    let dir = scoped("dedup");
    let text = write(
      &dir.0,
      vec![
        outcome("bad", 9, TestStatus::Failed),
        outcome("bad", 9, TestStatus::Failed),
      ],
    )
    .await
    .expect("@rerun.txt");
    assert_eq!(text.lines().count(), 1, "{text}");
  }

  #[tokio::test]
  async fn a_clean_run_leaves_no_file_behind() {
    let dir = scoped("clean");
    assert!(
      write(&dir.0, vec![outcome("ok", 3, TestStatus::Passed)])
        .await
        .is_none()
    );
  }
}
