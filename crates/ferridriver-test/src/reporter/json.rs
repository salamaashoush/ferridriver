//! JSON reporter: writes machine-readable results to a file.
//!
//! Includes step hierarchy in output (filtered to user-defined steps only,
//! matching Playwright's JSON reporter behavior).

use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;

use crate::model::TestStep;
use crate::reporter::{Reporter, ReporterEvent};

pub struct JsonReporter {
  output_path: PathBuf,
  results: Vec<JsonTestResult>,
  total: usize,
  duration: Duration,
}

#[derive(Serialize)]
struct JsonReport {
  total: usize,
  passed: usize,
  failed: usize,
  skipped: usize,
  flaky: usize,
  duration_ms: u128,
  tests: Vec<JsonTestResult>,
}

#[derive(Serialize, Clone)]
struct JsonTestResult {
  name: String,
  file: String,
  suite: Option<String>,
  status: String,
  duration_ms: u128,
  attempt: u32,
  error: Option<String>,
  /// Step hierarchy (only user-defined steps, matching Playwright's JSON reporter).
  #[serde(skip_serializing_if = "Vec::is_empty")]
  steps: Vec<JsonStep>,
  /// Metadata from config/project.
  #[serde(skip_serializing_if = "serde_json::Value::is_null")]
  metadata: serde_json::Value,
  /// Artifact attachments (screenshots, videos, traces).
  #[serde(skip_serializing_if = "Vec::is_empty")]
  attachments: Vec<JsonAttachment>,
}

#[derive(Serialize, Clone)]
struct JsonAttachment {
  name: String,
  content_type: String,
  /// File path (for file-based attachments) or null (for inline bytes).
  #[serde(skip_serializing_if = "Option::is_none")]
  path: Option<String>,
}

#[derive(Serialize, Clone)]
struct JsonStep {
  title: String,
  duration_ms: u128,
  status: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  error: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  metadata: Option<serde_json::Value>,
  #[serde(skip_serializing_if = "Vec::is_empty")]
  steps: Vec<JsonStep>,
}

impl JsonReporter {
  pub fn new(output_path: PathBuf) -> Self {
    Self {
      output_path,
      results: Vec::new(),
      total: 0,
      duration: Duration::ZERO,
    }
  }
}

fn serialize_steps(steps: &[TestStep]) -> Vec<JsonStep> {
  steps
    .iter()
    .filter(|s| s.category.is_visible())
    .map(|s| JsonStep {
      title: s.title.clone(),
      duration_ms: s.duration.as_millis(),
      status: format!("{:?}", s.status),
      error: s.error.clone(),
      metadata: s.metadata.clone(),
      steps: serialize_steps(&s.steps),
    })
    .collect()
}

#[async_trait::async_trait]
impl Reporter for JsonReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    match event {
      ReporterEvent::TestFinished { test_id, outcome } => {
        self.results.push(JsonTestResult {
          name: test_id.name.clone(),
          file: test_id.file.clone(),
          suite: test_id.suite.clone(),
          status: outcome.status.to_string(),
          duration_ms: outcome.duration.as_millis(),
          attempt: outcome.attempt,
          error: outcome.error.as_ref().map(|e| e.message.clone()),
          steps: serialize_steps(&outcome.steps),
          metadata: outcome.metadata.clone(),
          attachments: outcome
            .attachments
            .iter()
            .map(|a| JsonAttachment {
              name: a.name.clone(),
              content_type: a.content_type.clone(),
              path: match &a.body {
                crate::model::AttachmentBody::Path(p) => Some(p.display().to_string()),
                crate::model::AttachmentBody::Bytes(_) => None,
              },
            })
            .collect(),
        });
      },
      ReporterEvent::RunFinished { total, duration, .. } => {
        self.total = *total;
        self.duration = *duration;
      },
      _ => {},
    }
  }

  async fn finalize(&mut self) -> Result<(), String> {
    let passed = self.results.iter().filter(|r| r.status == "passed").count();
    let failed = self
      .results
      .iter()
      .filter(|r| r.status == "failed" || r.status == "timed out")
      .count();
    let skipped = self.results.iter().filter(|r| r.status == "skipped").count();
    let flaky = self.results.iter().filter(|r| r.status == "flaky").count();

    let report = JsonReport {
      total: self.total,
      passed,
      failed,
      skipped,
      flaky,
      duration_ms: self.duration.as_millis(),
      tests: self.results.clone(),
    };

    let json = serde_json::to_string_pretty(&report).map_err(|e| format!("JSON serialize error: {e}"))?;

    if let Some(parent) = self.output_path.parent() {
      std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&self.output_path, json).map_err(|e| format!("cannot write {}: {e}", self.output_path.display()))?;

    tracing::info!("JSON report written to {}", self.output_path.display());
    Ok(())
  }
}
