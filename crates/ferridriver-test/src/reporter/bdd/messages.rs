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
    Self { output_path, messages: Vec::new() }
  }
}

#[async_trait::async_trait]
impl Reporter for CucumberMessagesReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    match event {
      ReporterEvent::TestStarted { test_id, attempt } => {
        self.messages.push(serde_json::json!({
          "testCaseStarted": {
            "id": test_id.full_name(),
            "testCaseId": test_id.full_name(),
            "attempt": attempt,
            "timestamp": timestamp_now(),
          }
        }));
      }
      ReporterEvent::StepFinished(event) => {
        if !event.category.is_visible() { return; }
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
      }
      ReporterEvent::TestFinished { test_id, outcome } => {
        self.messages.push(serde_json::json!({
          "testCaseFinished": {
            "testCaseStartedId": test_id.full_name(),
            "timestamp": timestamp_now(),
            "willBeRetried": outcome.attempt < outcome.max_attempts,
          }
        }));
      }
      ReporterEvent::RunStarted { .. } => {
        self.messages.push(serde_json::json!({ "testRunStarted": { "timestamp": timestamp_now() } }));
      }
      ReporterEvent::RunFinished { .. } => {
        self.messages.push(serde_json::json!({ "testRunFinished": { "timestamp": timestamp_now(), "success": true } }));
      }
      _ => {}
    }
  }

  async fn finalize(&mut self) -> Result<(), String> {
    if let Some(parent) = self.output_path.parent() {
      std::fs::create_dir_all(parent).ok();
    }
    let mut file = std::fs::File::create(&self.output_path)
      .map_err(|e| format!("cannot create {}: {e}", self.output_path.display()))?;
    for msg in &self.messages {
      serde_json::to_writer(&mut file, msg).map_err(|e| format!("JSON write error: {e}"))?;
      writeln!(file).map_err(|e| format!("write error: {e}"))?;
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
