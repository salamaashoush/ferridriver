//! Cucumber JSON reporter: standard format for CI dashboards.

use std::path::PathBuf;

use crate::model::{StepStatus, TestStep};
use crate::reporter::{Reporter, ReporterEvent};

pub struct CucumberJsonReporter {
  output_path: PathBuf,
  features: Vec<CucumberFeature>,
  current_feature: Option<String>,
}

#[derive(serde::Serialize)]
struct CucumberFeature {
  keyword: String,
  name: String,
  uri: String,
  elements: Vec<CucumberScenario>,
}

#[derive(serde::Serialize)]
struct CucumberScenario {
  keyword: String,
  name: String,
  #[serde(rename = "type")]
  scenario_type: String,
  steps: Vec<CucumberStep>,
}

#[derive(serde::Serialize)]
struct CucumberStep {
  keyword: String,
  name: String,
  result: CucumberStepResult,
}

#[derive(serde::Serialize)]
struct CucumberStepResult {
  status: String,
  duration: u64,
  #[serde(skip_serializing_if = "Option::is_none")]
  error_message: Option<String>,
}

impl CucumberJsonReporter {
  pub fn new(output_path: PathBuf) -> Self {
    Self {
      output_path,
      features: Vec::new(),
      current_feature: None,
    }
  }

  fn ensure_feature(&mut self, name: &str, file: &str) {
    if self.current_feature.as_deref() != Some(name) {
      self.current_feature = Some(name.to_string());
      if !self.features.iter().any(|f| f.name == name) {
        self.features.push(CucumberFeature {
          keyword: "Feature".to_string(),
          name: name.to_string(),
          uri: file.to_string(),
          elements: Vec::new(),
        });
      }
    }
  }
}

fn extract_keyword(step: &TestStep) -> String {
  if let Some(meta) = &step.metadata {
    if let Some(kw) = meta.get("bdd_keyword").and_then(|v| v.as_str()) {
      return format!("{kw} ");
    }
  }
  step
    .title
    .split_whitespace()
    .next()
    .map(|w| format!("{w} "))
    .unwrap_or_default()
}

fn extract_text(step: &TestStep) -> String {
  if let Some(meta) = &step.metadata {
    if let Some(text) = meta.get("bdd_text").and_then(|v| v.as_str()) {
      return text.to_string();
    }
  }
  step.title.clone()
}

#[async_trait::async_trait]
impl Reporter for CucumberJsonReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    if let ReporterEvent::TestFinished { test_id, outcome } = event {
      let feature = test_id.suite.as_deref().unwrap_or("Unknown Feature");
      self.ensure_feature(feature, &test_id.file);

      let mut steps = Vec::new();
      for step in &outcome.steps {
        if !step.category.is_visible() {
          continue;
        }
        let status = match step.status {
          StepStatus::Passed => "passed",
          StepStatus::Failed => "failed",
          StepStatus::Skipped => "skipped",
          StepStatus::Pending => "pending",
        };
        steps.push(CucumberStep {
          keyword: extract_keyword(step),
          name: extract_text(step),
          result: CucumberStepResult {
            status: status.to_string(),
            duration: step.duration.as_nanos() as u64,
            error_message: step.error.clone(),
          },
        });
      }

      let scenario = CucumberScenario {
        keyword: "Scenario".to_string(),
        name: test_id.name.clone(),
        scenario_type: "scenario".to_string(),
        steps,
      };

      if let Some(f) = self.features.iter_mut().find(|f| f.name == feature) {
        f.elements.push(scenario);
      }
    }
  }

  async fn finalize(&mut self) -> Result<(), String> {
    if let Some(parent) = self.output_path.parent() {
      let _ = std::fs::create_dir_all(parent);
    }
    let json = serde_json::to_string_pretty(&self.features).map_err(|e| format!("JSON serialize: {e}"))?;
    std::fs::write(&self.output_path, json).map_err(|e| format!("write {}: {e}", self.output_path.display()))?;
    Ok(())
  }
}
