//! BDD JSON reporter: machine-readable results with step hierarchy.
//!
//! Implements `ferridriver_test::reporter::Reporter`.

use std::path::PathBuf;

use ferridriver_test::model::{StepCategory, TestStep};
use ferridriver_test::reporter::{Reporter, ReporterEvent};

pub struct BddJsonReporter {
  output_path: PathBuf,
  results: Vec<ScenarioEntry>,
  run_duration_ms: u64,
}

#[derive(serde::Serialize)]
struct ScenarioEntry {
  feature: String,
  scenario: String,
  status: String,
  duration_ms: u128,
  attempt: u32,
  steps: Vec<StepEntry>,
  error: Option<String>,
}

#[derive(serde::Serialize)]
struct StepEntry {
  title: String,
  status: String,
  duration_ms: u128,
  #[serde(skip_serializing_if = "Option::is_none")]
  error: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  metadata: Option<serde_json::Value>,
  #[serde(skip_serializing_if = "Vec::is_empty")]
  steps: Vec<StepEntry>,
}

#[derive(serde::Serialize)]
struct JsonOutput {
  total: usize,
  passed: usize,
  failed: usize,
  skipped: usize,
  duration_ms: u64,
  scenarios: Vec<ScenarioEntry>,
}

impl BddJsonReporter {
  pub fn new(output_path: PathBuf) -> Self {
    Self {
      output_path,
      results: Vec::new(),
      run_duration_ms: 0,
    }
  }
}

fn serialize_steps(steps: &[TestStep]) -> Vec<StepEntry> {
  steps
    .iter()
    .filter(|s| s.category == StepCategory::TestStep)
    .map(|s| StepEntry {
      title: s.title.clone(),
      status: format!("{:?}", s.status),
      duration_ms: s.duration.as_millis(),
      error: s.error.clone(),
      metadata: s.metadata.clone(),
      steps: serialize_steps(&s.steps),
    })
    .collect()
}

#[async_trait::async_trait]
impl Reporter for BddJsonReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    match event {
      ReporterEvent::TestFinished { test_id, outcome } => {
        self.results.push(ScenarioEntry {
          feature: test_id.suite.clone().unwrap_or_default(),
          scenario: test_id.name.clone(),
          status: outcome.status.to_string(),
          duration_ms: outcome.duration.as_millis(),
          attempt: outcome.attempt,
          steps: serialize_steps(&outcome.steps),
          error: outcome.error.as_ref().map(|e| e.message.clone()),
        });
      }
      ReporterEvent::RunFinished { duration, .. } => {
        self.run_duration_ms = duration.as_millis() as u64;
      }
      _ => {}
    }
  }

  async fn finalize(&mut self) -> Result<(), String> {
    let passed = self.results.iter().filter(|r| r.status == "passed").count();
    let failed = self.results.iter().filter(|r| r.status == "failed" || r.status == "timed out").count();
    let skipped = self.results.iter().filter(|r| r.status == "skipped").count();

    let output = JsonOutput {
      total: self.results.len(),
      passed,
      failed,
      skipped,
      duration_ms: self.run_duration_ms,
      scenarios: std::mem::take(&mut self.results),
    };

    if let Some(parent) = self.output_path.parent() {
      let _ = std::fs::create_dir_all(parent);
    }
    let json = serde_json::to_string_pretty(&output).map_err(|e| format!("JSON serialize: {e}"))?;
    std::fs::write(&self.output_path, json)
      .map_err(|e| format!("write {}: {e}", self.output_path.display()))?;
    Ok(())
  }
}
