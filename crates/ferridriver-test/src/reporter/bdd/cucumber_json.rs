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
  if let Some(meta) = &step.metadata
    && let Some(kw) = meta.get("bdd_keyword").and_then(|v| v.as_str())
  {
    return format!("{kw} ");
  }
  step
    .title
    .split_whitespace()
    .next()
    .map(|w| format!("{w} "))
    .unwrap_or_default()
}

fn extract_text(step: &TestStep) -> String {
  if let Some(meta) = &step.metadata
    && let Some(text) = meta.get("bdd_text").and_then(|v| v.as_str())
  {
    return text.to_string();
  }
  step.title.clone()
}

#[async_trait::async_trait]
impl Reporter for CucumberJsonReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    if let ReporterEvent::TestFinished { outcome } = event {
      let test_id = &outcome.test_id;
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

  async fn finalize(&mut self) -> ferridriver::error::Result<()> {
    if let Some(parent) = self.output_path.parent() {
      let _ = std::fs::create_dir_all(parent);
    }
    let json = serde_json::to_string_pretty(&self.features)?;
    std::fs::write(&self.output_path, json)?;
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;
  use std::time::Duration;

  use super::*;
  use crate::model::{StepCategory, TestId, TestOutcome, TestStatus};

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

  fn step(keyword: &str, text: &str, status: StepStatus, error: Option<&str>) -> TestStep {
    TestStep {
      step_id: format!("s-{text}"),
      title: format!("{keyword}{text}"),
      category: StepCategory::TestStep,
      duration: Duration::from_millis(12),
      status,
      error: error.map(ToString::to_string),
      location: None,
      parent_step_id: None,
      // The translator stores the keyword trimmed (`translate.rs`:
      // `step.keyword.trim()`); the reporter re-adds the separating space.
      metadata: Some(serde_json::json!({ "bdd_keyword": keyword.trim(), "bdd_text": text })),
      steps: Vec::new(),
    }
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

  #[tokio::test]
  async fn a_scenario_becomes_a_feature_element_with_its_steps() {
    let dir = scoped("cucumber");
    let path = dir.0.join("cucumber.json");
    let mut reporter = CucumberJsonReporter::new(path.clone());
    reporter
      .on_event(&ReporterEvent::TestFinished {
        outcome: scenario(
          "signs in",
          TestStatus::Passed,
          vec![
            step("Given ", "a registered user", StepStatus::Passed, None),
            step("When ", "they sign in", StepStatus::Passed, None),
          ],
        ),
      })
      .await;
    reporter.finalize().await.expect("finalize");

    let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("parse");
    let feature = &doc[0];
    assert_eq!(feature["keyword"], "Feature");
    assert_eq!(feature["name"], "Login");
    assert_eq!(feature["uri"], "features/login.feature");
    let element = &feature["elements"][0];
    assert_eq!(element["name"], "signs in");
    assert_eq!(element["type"], "scenario");
    assert_eq!(element["steps"][0]["keyword"], "Given ");
    assert_eq!(element["steps"][0]["name"], "a registered user");
    assert_eq!(element["steps"][0]["result"]["status"], "passed");
    assert_eq!(
      element["steps"][0]["result"]["duration"], 12_000_000_u64,
      "cucumber durations are nanoseconds"
    );
  }

  #[tokio::test]
  async fn a_failing_step_carries_its_message() {
    let dir = scoped("cucumber-fail");
    let path = dir.0.join("cucumber.json");
    let mut reporter = CucumberJsonReporter::new(path.clone());
    reporter
      .on_event(&ReporterEvent::TestFinished {
        outcome: scenario(
          "signs in",
          TestStatus::Failed,
          vec![step(
            "Then ",
            "they see the dashboard",
            StepStatus::Failed,
            Some("no dashboard"),
          )],
        ),
      })
      .await;
    reporter.finalize().await.expect("finalize");

    let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("parse");
    let result = &doc[0]["elements"][0]["steps"][0]["result"];
    assert_eq!(result["status"], "failed");
    assert_eq!(result["error_message"], "no dashboard");
  }

  #[tokio::test]
  async fn two_scenarios_of_one_feature_share_a_feature_entry() {
    let dir = scoped("cucumber-group");
    let path = dir.0.join("cucumber.json");
    let mut reporter = CucumberJsonReporter::new(path.clone());
    for name in ["signs in", "signs out"] {
      reporter
        .on_event(&ReporterEvent::TestFinished {
          outcome: scenario(
            name,
            TestStatus::Passed,
            vec![step("Given ", "a user", StepStatus::Passed, None)],
          ),
        })
        .await;
    }
    reporter.finalize().await.expect("finalize");

    let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("parse");
    assert_eq!(doc.as_array().expect("features").len(), 1, "one feature");
    assert_eq!(doc[0]["elements"].as_array().expect("elements").len(), 2);
  }
}
