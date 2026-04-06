//! Scenario execution model: expansion, variable interpolation, results.

use std::path::PathBuf;
use std::time::Duration;

use rustc_hash::FxHashMap;

use crate::feature::{extract_tags, ParsedFeature};

/// A concrete scenario ready for execution (after Outline expansion).
#[derive(Debug, Clone)]
pub struct ScenarioExecution {
  /// Parent feature name.
  pub feature_name: String,
  /// Feature file path.
  pub feature_path: PathBuf,
  /// Scenario name (with example suffix for Outlines).
  pub name: String,
  /// Merged tags (feature + scenario + example tags).
  pub tags: Vec<String>,
  /// Steps to execute (Background steps prepended).
  pub steps: Vec<ScenarioStep>,
  /// Source location: `file:line`.
  pub location: String,
  /// Example values from Scenario Outline expansion.
  pub example_values: Option<FxHashMap<String, String>>,
}

/// A step within a scenario, extracted from the Gherkin AST.
#[derive(Debug, Clone)]
pub struct ScenarioStep {
  /// Keyword (Given, When, Then, And, But).
  pub keyword: String,
  /// Step text body (after keyword).
  pub text: String,
  /// Optional data table.
  pub table: Option<crate::data_table::DataTable>,
  /// Optional doc string.
  pub docstring: Option<String>,
  /// Line number in the feature file.
  pub line: usize,
}

/// Expand a parsed feature into concrete scenarios.
///
/// - Background steps are prepended to every scenario
/// - Scenario Outlines are expanded with each Examples row
/// - Tags are merged (feature + scenario + example)
pub fn expand_feature(feature: &ParsedFeature) -> Vec<ScenarioExecution> {
  let mut scenarios = Vec::new();
  let feature_tags = extract_tags(&feature.feature.tags);

  // Background steps.
  let background_steps: Vec<ScenarioStep> = feature
    .feature
    .background
    .as_ref()
    .map(|bg| bg.steps.iter().map(gherkin_step_to_scenario_step).collect())
    .unwrap_or_default();

  for scenario in &feature.feature.scenarios {
    let scenario_tags: Vec<String> = feature_tags
      .iter()
      .chain(extract_tags(&scenario.tags).iter())
      .cloned()
      .collect();

    if scenario.examples.is_empty() {
      // Regular scenario.
      let mut steps = background_steps.clone();
      steps.extend(scenario.steps.iter().map(gherkin_step_to_scenario_step));

      scenarios.push(ScenarioExecution {
        feature_name: feature.feature.name.clone(),
        feature_path: feature.path.clone(),
        name: scenario.name.clone(),
        tags: scenario_tags,
        steps,
        location: format!("{}:{}", feature.path.display(), scenario.position.line),
        example_values: None,
      });
    } else {
      // Scenario Outline: expand each example row.
      for example in &scenario.examples {
        let example_tags: Vec<String> = scenario_tags
          .iter()
          .chain(extract_tags(&example.tags).iter())
          .cloned()
          .collect();

        if let Some(table) = &example.table {
          if table.rows.len() < 2 {
            continue;
          }

          let headers = &table.rows[0];
          for (row_idx, row) in table.rows[1..].iter().enumerate() {
            let mut values: FxHashMap<String, String> = FxHashMap::default();
            for (i, header) in headers.iter().enumerate() {
              if let Some(val) = row.get(i) {
                values.insert(header.clone(), val.clone());
              }
            }

            // Substitute <placeholder> in step text.
            let mut steps = background_steps.clone();
            steps.extend(scenario.steps.iter().map(|s| {
              let mut step = gherkin_step_to_scenario_step(s);
              step.text = substitute_placeholders(&step.text, &values);
              // Also substitute in table cells and docstrings.
              if let Some(table) = &mut step.table {
                for row in table.iter_mut() {
                  for cell in row.iter_mut() {
                    *cell = substitute_placeholders(cell, &values);
                  }
                }
              }
              if let Some(ds) = &mut step.docstring {
                *ds = substitute_placeholders(ds, &values);
              }
              step
            }));

            scenarios.push(ScenarioExecution {
              feature_name: feature.feature.name.clone(),
              feature_path: feature.path.clone(),
              name: if let Some(ref ex_name) = example.name {
                format!("{} ({} #{})", scenario.name, ex_name, row_idx + 1)
              } else {
                format!("{} (Example #{})", scenario.name, row_idx + 1)
              },
              tags: example_tags.clone(),
              steps,
              location: format!(
                "{}:{}",
                feature.path.display(),
                scenario.position.line
              ),
              example_values: Some(values),
            });
          }
        }
      }
    }
  }

  // Also expand scenarios within Rules.
  for rule in &feature.feature.rules {
    let rule_background: Vec<ScenarioStep> = rule
      .background
      .as_ref()
      .map(|bg| bg.steps.iter().map(gherkin_step_to_scenario_step).collect())
      .unwrap_or_default();

    for scenario in &rule.scenarios {
      let scenario_tags: Vec<String> = feature_tags
        .iter()
        .chain(extract_tags(&scenario.tags).iter())
        .cloned()
        .collect();

      let mut steps = background_steps.clone();
      steps.extend(rule_background.clone());
      steps.extend(scenario.steps.iter().map(gherkin_step_to_scenario_step));

      scenarios.push(ScenarioExecution {
        feature_name: feature.feature.name.clone(),
        feature_path: feature.path.clone(),
        name: format!("{} > {}", rule.name, scenario.name),
        tags: scenario_tags,
        steps,
        location: format!("{}:{}", feature.path.display(), scenario.position.line),
        example_values: None,
      });
    }
  }

  scenarios
}

fn gherkin_step_to_scenario_step(step: &gherkin::Step) -> ScenarioStep {
  ScenarioStep {
    keyword: step.keyword.clone(),
    text: step.value.clone(),
    table: step
      .table
      .as_ref()
      .map(crate::feature::table_to_vec),
    docstring: step.docstring.clone(),
    line: step.position.line,
  }
}

fn substitute_placeholders(text: &str, values: &FxHashMap<String, String>) -> String {
  let mut result = text.to_string();
  for (key, val) in values {
    result = result.replace(&format!("<{key}>"), val);
  }
  result
}

// ── Scenario result types ──

/// Status of a single step execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum StepStatus {
  Passed,
  Failed,
  Skipped,
  Undefined,
  Pending,
}

/// Status of a scenario execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum ScenarioStatus {
  Passed,
  Failed,
  Skipped,
  Undefined,
}

/// Result of executing a single step.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StepResult {
  pub keyword: String,
  pub text: String,
  pub status: StepStatus,
  pub duration: Duration,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub error: Option<String>,
}

/// Result of executing an entire scenario.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScenarioResult {
  pub feature_name: String,
  pub feature_path: String,
  pub scenario_name: String,
  pub status: ScenarioStatus,
  pub steps: Vec<StepResult>,
  pub duration: Duration,
  pub attempt: u32,
  pub tags: Vec<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub error: Option<String>,
  #[serde(skip)]
  pub failure_screenshot: Option<Vec<u8>>,
}

impl ScenarioResult {
  /// Whether this scenario should be retried.
  pub fn should_retry(&self, max_retries: u32) -> bool {
    self.status == ScenarioStatus::Failed && self.attempt < max_retries
  }
}
