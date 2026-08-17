//! Cucumber Messages reporter: NDJSON event stream per the Cucumber Messages protocol.

use std::io::Write;
use std::path::PathBuf;

use crate::reporter::{Reporter, ReporterEvent};

pub struct CucumberMessagesReporter {
  output_path: PathBuf,
  messages: Vec<serde_json::Value>,
}

impl CucumberMessagesReporter {
  pub fn new(output_path: PathBuf) -> Self {
    Self {
      output_path,
      messages: Vec::new(),
    }
  }
}

#[async_trait::async_trait]
impl Reporter for CucumberMessagesReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    match event {
      ReporterEvent::TestStarted { test_id, attempt, .. } => {
        self.messages.push(serde_json::json!({
          "testCaseStarted": {
            "id": test_id.full_name(),
            "testCaseId": test_id.full_name(),
            "attempt": attempt,
            "timestamp": timestamp_now(),
          }
        }));
      },
      ReporterEvent::StepFinished(event) => {
        if !event.category.is_visible() {
          return;
        }
        let status = if event.error.is_some() { "FAILED" } else { "PASSED" };
        self.messages.push(serde_json::json!({
          "testStepFinished": {
            "testStepId": event.step_id,
            "testCaseStartedId": event.test_id.full_name(),
            "testStepResult": {
              "status": status,
              "duration": { "seconds": event.duration.as_secs(), "nanos": event.duration.subsec_nanos() },
              "message": event.error,
            },
            "timestamp": timestamp_now(),
          }
        }));
      },
      ReporterEvent::TestFinished { outcome } => {
        let test_id = &outcome.test_id;
        // `willBeRetried` is about *this* attempt: only a failure with
        // attempts left is retried. Reporting it for a passing test made
        // every scenario look flaky to a Cucumber consumer.
        let will_be_retried = outcome.status.is_failure() && outcome.attempt < outcome.max_attempts;
        self.messages.push(serde_json::json!({
          "testCaseFinished": {
            "testCaseStartedId": test_id.full_name(),
            "timestamp": timestamp_now(),
            "willBeRetried": will_be_retried,
          }
        }));
      },
      ReporterEvent::RunStarted { .. } => {
        self
          .messages
          .push(serde_json::json!({ "testRunStarted": { "timestamp": timestamp_now() } }));
      },
      ReporterEvent::RunFinished { failed, status, .. } => {
        self.messages.push(serde_json::json!({
          "testRunFinished": {
            "timestamp": timestamp_now(),
            "success": *failed == 0 && *status == crate::reporter::RunStatus::Passed,
          }
        }));
      },
      _ => {},
    }
  }

  async fn finalize(&mut self) -> ferridriver::error::Result<()> {
    if let Some(parent) = self.output_path.parent() {
      std::fs::create_dir_all(parent).ok();
    }
    let mut file = std::fs::File::create(&self.output_path)?;
    for msg in &self.messages {
      serde_json::to_writer(&mut file, msg)?;
      writeln!(file)?;
    }
    tracing::info!("Cucumber Messages written to {}", self.output_path.display());
    Ok(())
  }
}

fn timestamp_now() -> serde_json::Value {
  let d = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or_default();
  serde_json::json!({ "seconds": d.as_secs(), "nanos": d.subsec_nanos() })
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;
  use std::time::Duration;

  use super::*;
  use crate::model::{StepCategory, TestId, TestOutcome, TestStatus, TestStep};
  use crate::reporter::RunStatus;

  struct ScopedDir(std::path::PathBuf);
  impl Drop for ScopedDir {
    fn drop(&mut self) {
      let _ = std::fs::remove_dir_all(&self.0);
    }
  }

  fn scoped(name: &str) -> ScopedDir {
    let path = std::env::temp_dir().join(format!("ferri-bdd-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("temp dir");
    ScopedDir(path)
  }

  fn scenario(name: &str, status: TestStatus, steps: Vec<TestStep>) -> Arc<TestOutcome> {
    Arc::new(TestOutcome {
      test_id: TestId {
        file: "features/login.feature".into(),
        suite: Some("Login".into()),
        name: name.into(),
        line: Some(4),
        column: None,
      },
      status,
      duration: Duration::from_millis(60),
      max_attempts: 1,
      steps,
      ..Default::default()
    })
  }

  fn run_finished(failed: usize, status: RunStatus) -> ReporterEvent {
    ReporterEvent::RunFinished {
      total: 1,
      passed: usize::from(failed == 0),
      failed,
      skipped: 0,
      flaky: 0,
      duration: Duration::from_millis(90),
      status,
    }
  }

  fn messages_of(path: &std::path::Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
      .expect("read ndjson")
      .lines()
      .filter(|line| !line.trim().is_empty())
      .map(|line| serde_json::from_str(line).expect("parse ndjson line"))
      .collect()
  }

  #[tokio::test]
  async fn a_passing_scenario_is_not_marked_for_retry() {
    // The bug this guards: `willBeRetried` was `attempt < max_attempts`,
    // so every passing scenario in a suite with retries claimed it would
    // be retried.
    let dir = scoped("messages-retry");
    let path = dir.0.join("m.ndjson");
    let mut reporter = CucumberMessagesReporter::new(path.clone());
    let mut passed = (*scenario("signs in", TestStatus::Passed, Vec::new())).clone();
    passed.attempt = 1;
    passed.max_attempts = 3;

    reporter
      .on_event(&ReporterEvent::TestFinished {
        outcome: Arc::new(passed),
      })
      .await;
    reporter.finalize().await.expect("finalize");

    let finished = messages_of(&path)
      .into_iter()
      .find(|m| m.get("testCaseFinished").is_some())
      .expect("testCaseFinished");
    assert_eq!(finished["testCaseFinished"]["willBeRetried"], false);
  }

  #[tokio::test]
  async fn a_failing_attempt_with_retries_left_is_marked() {
    let dir = scoped("messages-willretry");
    let path = dir.0.join("m.ndjson");
    let mut reporter = CucumberMessagesReporter::new(path.clone());
    let mut failed = (*scenario("signs in", TestStatus::Failed, Vec::new())).clone();
    failed.attempt = 1;
    failed.max_attempts = 2;

    reporter
      .on_event(&ReporterEvent::TestFinished {
        outcome: Arc::new(failed),
      })
      .await;
    reporter.finalize().await.expect("finalize");

    let finished = messages_of(&path)
      .into_iter()
      .find(|m| m.get("testCaseFinished").is_some())
      .expect("testCaseFinished");
    assert_eq!(finished["testCaseFinished"]["willBeRetried"], true);
  }

  #[tokio::test]
  async fn the_run_reports_whether_it_actually_succeeded() {
    // `success` used to be hardcoded true, so a failing run told every
    // Cucumber consumer it had passed.
    for (failed, status, expected) in [(0_usize, RunStatus::Passed, true), (1, RunStatus::Failed, false)] {
      let dir = scoped(&format!("messages-success-{failed}"));
      let path = dir.0.join("m.ndjson");
      let mut reporter = CucumberMessagesReporter::new(path.clone());
      reporter.on_event(&run_finished(failed, status)).await;
      reporter.finalize().await.expect("finalize");

      let run = messages_of(&path)
        .into_iter()
        .find(|m| m.get("testRunFinished").is_some())
        .expect("testRunFinished");
      assert_eq!(run["testRunFinished"]["success"], expected, "failed={failed}");
    }
  }

  #[tokio::test]
  async fn a_step_result_records_its_status_and_duration() {
    let dir = scoped("messages-steps");
    let path = dir.0.join("m.ndjson");
    let mut reporter = CucumberMessagesReporter::new(path.clone());
    reporter
      .on_event(&ReporterEvent::StepFinished(Arc::new(
        crate::reporter::StepFinishedEvent {
          test_id: TestId {
            file: "features/login.feature".into(),
            suite: Some("Login".into()),
            name: "signs in".into(),
            line: Some(4),
            column: None,
          },
          step_id: "s1".into(),
          title: "Given a user".into(),
          category: StepCategory::TestStep,
          duration: Duration::from_millis(1_500),
          error: None,
          metadata: None,
          annotations: Vec::new(),
        },
      )))
      .await;
    reporter.finalize().await.expect("finalize");

    let step = messages_of(&path)
      .into_iter()
      .find(|m| m.get("testStepFinished").is_some())
      .expect("testStepFinished");
    let result = &step["testStepFinished"]["testStepResult"];
    assert_eq!(result["status"], "PASSED");
    assert_eq!(result["duration"]["seconds"], 1);
    assert_eq!(result["duration"]["nanos"], 500_000_000_u64);
  }
}
