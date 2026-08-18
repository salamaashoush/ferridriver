//! Cucumber JSON reporter: the document CI dashboards read.
//!
//! Field-for-field against `playwright-bdd`'s
//! `src/reporter/cucumber/json.ts`, which is itself a port of
//! cucumber-js's `json_formatter`. A consumer keyed on `elements[].id`,
//! on a step's `arguments`, or on a hook step's `hidden` flag works
//! unchanged.
//!
//! The Gherkin facts a document quotes — keywords, descriptions, the
//! line a tag was written on — reach here as the scenario's opaque
//! [`crate::model::TestOutcome::case_metadata`], written by the BDD
//! translator. A test with none is still reported, with the fields it
//! cannot know left at their empty defaults.

use std::path::PathBuf;

use base64::Engine;

use crate::model::{AttachmentBody, StepCategory, StepStatus, TestOutcome, TestStep};
use crate::reporter::{Reporter, ReporterEvent};

/// Playwright's title separator, which `playwright-bdd` uses to join a
/// project name onto a feature name.
const PROJECT_SEPARATOR: &str = " \u{203a} ";

pub struct CucumberJsonReporter {
  output_path: PathBuf,
  features: Vec<CucumberFeature>,
  /// `(project, uri)` of each feature entry, in the order they were
  /// first seen. The KEY is not the feature name: two projects running
  /// one feature file are two entries, and two files may share a name.
  keys: Vec<(String, String)>,
  options: Options,
}

#[derive(Debug, Clone, Default)]
struct Options {
  /// `playwright-bdd`'s `skipAttachments`, default true: attachment
  /// bodies are large and some JSON parsers choke on them.
  skip_attachments: bool,
  /// `addProjectToFeatureName`: the emitted `name` carries the project.
  add_project_to_feature_name: bool,
  /// `addMetadata`: `'object'` or `'list'`.
  add_metadata: Option<MetadataShape>,
  /// The run's browser, for `addMetadata`.
  browser: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MetadataShape {
  Object,
  List,
}

// ── Document ──

#[derive(serde::Serialize)]
struct CucumberFeature {
  description: String,
  elements: Vec<CucumberScenario>,
  id: String,
  keyword: String,
  line: usize,
  name: String,
  tags: Vec<CucumberTag>,
  uri: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  metadata: Option<serde_json::Value>,
}

#[derive(serde::Serialize)]
struct CucumberScenario {
  description: String,
  id: String,
  keyword: String,
  line: usize,
  name: String,
  steps: Vec<CucumberStep>,
  tags: Vec<CucumberTag>,
  #[serde(rename = "type")]
  scenario_type: String,
}

#[derive(serde::Serialize)]
struct CucumberStep {
  #[serde(skip_serializing_if = "Option::is_none")]
  arguments: Option<serde_json::Value>,
  #[serde(skip_serializing_if = "Option::is_none")]
  embeddings: Option<Vec<CucumberEmbedding>>,
  #[serde(skip_serializing_if = "Option::is_none")]
  hidden: Option<bool>,
  #[serde(skip_serializing_if = "Option::is_none")]
  keyword: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  line: Option<usize>,
  #[serde(skip_serializing_if = "Option::is_none")]
  name: Option<String>,
  result: CucumberStepResult,
}

#[derive(serde::Serialize)]
struct CucumberEmbedding {
  data: String,
  mime_type: String,
}

#[derive(serde::Serialize)]
struct CucumberStepResult {
  status: String,
  duration: u64,
  #[serde(skip_serializing_if = "Option::is_none")]
  error_message: Option<String>,
}

#[derive(serde::Serialize)]
struct CucumberTag {
  name: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  line: Option<usize>,
}

/// The Gherkin source of one scenario, as the translator wrote it.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Source {
  feature_keyword: String,
  feature_name: String,
  feature_description: String,
  feature_line: usize,
  feature_tags: Vec<SourceTag>,
  rule_name: Option<String>,
  scenario_keyword: String,
  scenario_description: String,
  scenario_line: usize,
  tags: Vec<SourceTag>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
struct SourceTag {
  name: String,
  line: usize,
}

impl SourceTag {
  fn into_tag(self) -> CucumberTag {
    CucumberTag {
      name: self.name,
      // Cucumber leaves `line` undefined for a tag it cannot place.
      line: (self.line > 0).then_some(self.line),
    }
  }
}

/// `playwright-bdd`'s `convertNameToId`: lowercase, spaces to dashes.
fn name_to_id(name: &str) -> String {
  name.replace(' ', "-").to_lowercase()
}

/// The feature name a project qualifies, `playwright-bdd`'s
/// `getFeatureNameWithProject`.
fn name_with_project(project: &str, feature: &str) -> String {
  if project.is_empty() {
    feature.to_string()
  } else {
    format!("{project}{PROJECT_SEPARATOR}{feature}")
  }
}

impl CucumberJsonReporter {
  pub fn new(output_path: PathBuf) -> Self {
    Self {
      output_path,
      features: Vec::new(),
      keys: Vec::new(),
      options: Options {
        skip_attachments: true,
        ..Options::default()
      },
    }
  }

  /// The reporter entry's options, plus the run's browser for
  /// `addMetadata`.
  #[must_use]
  pub fn with_options(
    mut self,
    options: &std::collections::BTreeMap<String, serde_json::Value>,
    config: &crate::config::TestConfig,
  ) -> Self {
    let flag = |key: &str, default: bool| options.get(key).and_then(serde_json::Value::as_bool).unwrap_or(default);
    self.options = Options {
      skip_attachments: flag("skipAttachments", true),
      add_project_to_feature_name: flag("addProjectToFeatureName", false),
      add_metadata: match options.get("addMetadata").and_then(serde_json::Value::as_str) {
        Some("object") => Some(MetadataShape::Object),
        Some("list") => Some(MetadataShape::List),
        _ => None,
      },
      browser: config.browser.browser.clone(),
    };
    self
  }

  /// The feature entry for this outcome, created on first sight.
  fn feature_for(&mut self, outcome: &TestOutcome, source: &Source) -> usize {
    let project = outcome.project_name.clone();
    let uri = outcome.test_id.file.clone();
    let key = (project.clone(), uri.clone());
    if let Some(index) = self.keys.iter().position(|existing| *existing == key) {
      return index;
    }
    let feature_name = if source.feature_name.is_empty() {
      outcome.test_id.suite.clone().unwrap_or_default()
    } else {
      source.feature_name.clone()
    };
    let qualified = name_with_project(&project, &feature_name);
    self.keys.push(key);
    self.features.push(CucumberFeature {
      description: source.feature_description.clone(),
      elements: Vec::new(),
      id: name_to_id(&qualified),
      keyword: if source.feature_keyword.is_empty() {
        "Feature".to_string()
      } else {
        source.feature_keyword.clone()
      },
      line: source.feature_line,
      name: if self.options.add_project_to_feature_name {
        qualified
      } else {
        feature_name
      },
      tags: source.feature_tags.iter().cloned().map(SourceTag::into_tag).collect(),
      uri,
      metadata: self.metadata(&project),
    });
    self.features.len() - 1
  }

  fn metadata(&self, project: &str) -> Option<serde_json::Value> {
    match self.options.add_metadata? {
      MetadataShape::Object => Some(serde_json::json!({
        "Project": project,
        "Browser": self.options.browser,
      })),
      MetadataShape::List => Some(serde_json::json!([
        { "name": "Project", "value": project },
        { "name": "Browser", "value": self.options.browser },
      ])),
    }
  }

  /// The element id: the feature's, the rule's when there is one, and
  /// the scenario's, joined by `;`. `playwright-bdd`'s
  /// `formatScenarioId` builds the feature part from the RAW feature
  /// name, unlike the feature's own `id`.
  fn element_id(&self, source: &Source, name: &str) -> String {
    let mut parts = vec![name_to_id(&source.feature_name)];
    if let Some(rule) = &source.rule_name {
      parts.push(name_to_id(rule));
    }
    parts.push(name_to_id(name));
    parts.join(";")
  }

  fn steps(&self, outcome: &TestOutcome) -> Vec<CucumberStep> {
    let mut steps = Vec::new();
    // Cucumber calls every hook before the first real step "Before" and
    // every one after it "After".
    let mut before_hooks = true;
    for step in &outcome.steps {
      match step.category {
        StepCategory::TestStep => {
          before_hooks = false;
          steps.push(self.visible_step(step, outcome));
        },
        StepCategory::Hook => steps.push(self.hook_step(step, before_hooks, outcome)),
        _ => {},
      }
    }
    steps
  }

  fn visible_step(&self, step: &TestStep, outcome: &TestOutcome) -> CucumberStep {
    CucumberStep {
      arguments: Some(meta_value(step, "bdd_arguments").unwrap_or_else(|| serde_json::json!([]))),
      embeddings: self.embeddings(step, outcome),
      hidden: None,
      keyword: Some(extract_keyword(step)),
      line: meta_value(step, "bdd_line")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize),
      name: Some(extract_text(step)),
      result: step_result(step),
    }
  }

  fn hook_step(&self, step: &TestStep, before: bool, outcome: &TestOutcome) -> CucumberStep {
    CucumberStep {
      arguments: None,
      embeddings: self.embeddings(step, outcome),
      hidden: Some(true),
      keyword: Some(if before {
        "Before".to_string()
      } else {
        "After".to_string()
      }),
      line: None,
      name: None,
      result: step_result(step),
    }
  }

  /// The attachments this step produced, base64 as Cucumber embeds
  /// them. Skipped by default, as `playwright-bdd` skips them.
  fn embeddings(&self, step: &TestStep, outcome: &TestOutcome) -> Option<Vec<CucumberEmbedding>> {
    if self.options.skip_attachments {
      return None;
    }
    let embeddings: Vec<CucumberEmbedding> = outcome
      .attachments
      .iter()
      .filter(|attachment| attachment.step_id.as_deref() == Some(step.step_id.as_str()))
      .filter_map(|attachment| {
        let bytes = match &attachment.body {
          AttachmentBody::Bytes(bytes) => bytes.clone(),
          AttachmentBody::Path(path) => std::fs::read(path).ok()?,
        };
        Some(CucumberEmbedding {
          data: base64::engine::general_purpose::STANDARD.encode(bytes),
          mime_type: attachment.content_type.clone(),
        })
      })
      .collect();
    (!embeddings.is_empty()).then_some(embeddings)
  }
}

fn step_result(step: &TestStep) -> CucumberStepResult {
  let status = match step.status {
    StepStatus::Passed => "passed",
    StepStatus::Failed => "failed",
    StepStatus::Skipped => "skipped",
    StepStatus::Pending => "pending",
  };
  CucumberStepResult {
    status: status.to_string(),
    duration: u64::try_from(step.duration.as_nanos()).unwrap_or(u64::MAX),
    error_message: step.error.clone(),
  }
}

fn meta_value(step: &TestStep, key: &str) -> Option<serde_json::Value> {
  step.metadata.as_ref()?.get(key).cloned()
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
    let ReporterEvent::TestFinished { outcome } = event else {
      return;
    };
    let source: Source = outcome
      .case_metadata
      .clone()
      .and_then(|value| serde_json::from_value(value).ok())
      .unwrap_or_default();
    let steps = self.steps(outcome);
    let name = outcome.test_id.name.clone();
    let element = CucumberScenario {
      description: source.scenario_description.clone(),
      id: self.element_id(&source, &name),
      keyword: if source.scenario_keyword.is_empty() {
        "Scenario".to_string()
      } else {
        source.scenario_keyword.clone()
      },
      line: if source.scenario_line > 0 {
        source.scenario_line
      } else {
        outcome.test_id.line.unwrap_or_default()
      },
      name,
      steps,
      tags: source.tags.iter().cloned().map(SourceTag::into_tag).collect(),
      scenario_type: "scenario".to_string(),
    };
    let index = self.feature_for(outcome, &source);
    self.features[index].elements.push(element);
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
  use crate::model::{Attachment, StepCategory, TestId, TestStatus};

  struct ScopedDir(PathBuf);
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
      title: format!("{keyword} {text}"),
      category: StepCategory::TestStep,
      duration: Duration::from_millis(12),
      status,
      error: error.map(ToString::to_string),
      location: None,
      annotations: Vec::new(),
      parent_step_id: None,
      metadata: Some(serde_json::json!({
        "bdd_keyword": keyword,
        "bdd_text": text,
        "bdd_line": 7,
        "bdd_arguments": [],
      })),
      steps: Vec::new(),
    }
  }

  fn hook(title: &str) -> TestStep {
    TestStep {
      step_id: format!("h-{title}"),
      title: title.to_string(),
      category: StepCategory::Hook,
      duration: Duration::from_millis(3),
      status: StepStatus::Passed,
      error: None,
      location: None,
      annotations: Vec::new(),
      parent_step_id: None,
      metadata: None,
      steps: Vec::new(),
    }
  }

  fn source() -> serde_json::Value {
    serde_json::json!({
      "featureKeyword": "Feature",
      "featureName": "Login",
      "featureDescription": "How a user signs in.",
      "featureLine": 2,
      "featureTags": [{ "name": "@auth", "line": 1 }],
      "ruleName": null,
      "scenarioKeyword": "Scenario",
      "scenarioDescription": "The happy path.",
      "scenarioLine": 5,
      "tags": [{ "name": "@auth", "line": 1 }, { "name": "@smoke", "line": 4 }],
    })
  }

  fn scenario(name: &str, project: &str, steps: Vec<TestStep>, source: serde_json::Value) -> Arc<TestOutcome> {
    Arc::new(TestOutcome {
      test_id: TestId {
        file: "features/login.feature".into(),
        suite: Some("Login".into()),
        name: name.into(),
        line: Some(5),
        column: None,
      },
      status: TestStatus::Passed,
      duration: Duration::from_millis(60),
      max_attempts: 1,
      steps,
      project_name: project.to_string(),
      case_metadata: Some(source),
      ..Default::default()
    })
  }

  async fn write(reporter: &mut CucumberJsonReporter, outcomes: Vec<Arc<TestOutcome>>) -> serde_json::Value {
    for outcome in outcomes {
      reporter.on_event(&ReporterEvent::TestFinished { outcome }).await;
    }
    reporter.finalize().await.expect("finalize");
    let text = std::fs::read_to_string(&reporter.output_path).expect("read");
    serde_json::from_str(&text).expect("parse")
  }

  #[tokio::test]
  async fn the_document_carries_every_field_cucumber_defines() {
    let dir = scoped("cucumber-doc");
    let mut reporter = CucumberJsonReporter::new(dir.0.join("cucumber.json"));
    let doc = write(
      &mut reporter,
      vec![scenario(
        "signs in",
        "chromium",
        vec![
          step("Given", "a registered user", StepStatus::Passed, None),
          step("When", "they sign in", StepStatus::Passed, None),
        ],
        source(),
      )],
    )
    .await;

    let feature = &doc[0];
    assert_eq!(feature["keyword"], "Feature");
    assert_eq!(feature["name"], "Login", "the project is not in the name by default");
    assert_eq!(
      feature["id"], "chromium-›-login",
      "the id IS project-qualified, as upstream: {feature}"
    );
    assert_eq!(feature["line"], 2);
    assert_eq!(feature["description"], "How a user signs in.");
    assert_eq!(feature["uri"], "features/login.feature");
    assert_eq!(feature["tags"], serde_json::json!([{ "name": "@auth", "line": 1 }]));

    let element = &feature["elements"][0];
    assert_eq!(element["name"], "signs in");
    assert_eq!(element["type"], "scenario");
    assert_eq!(element["keyword"], "Scenario");
    assert_eq!(element["line"], 5);
    assert_eq!(element["description"], "The happy path.");
    assert_eq!(
      element["id"], "login;signs-in",
      "feature id and scenario id, joined by `;`"
    );
    assert_eq!(
      element["tags"],
      serde_json::json!([{ "name": "@auth", "line": 1 }, { "name": "@smoke", "line": 4 }]),
      "every scenario tag appears, each on the line it was written",
    );

    let first = &element["steps"][0];
    assert_eq!(first["keyword"], "Given ");
    assert_eq!(first["name"], "a registered user");
    assert_eq!(first["line"], 7);
    assert_eq!(first["arguments"], serde_json::json!([]));
    assert_eq!(first["result"]["status"], "passed");
    assert_eq!(
      first["result"]["duration"], 12_000_000_u64,
      "cucumber durations are nanoseconds"
    );
    assert!(first.get("hidden").is_none(), "a real step is not hidden");
  }

  #[tokio::test]
  async fn a_rule_is_part_of_the_element_id() {
    let dir = scoped("cucumber-rule");
    let mut reporter = CucumberJsonReporter::new(dir.0.join("cucumber.json"));
    let mut with_rule = source();
    with_rule["ruleName"] = "Page structure".into();
    let doc = write(
      &mut reporter,
      vec![scenario(
        "has a heading",
        "chromium",
        vec![step("Then", "it is visible", StepStatus::Passed, None)],
        with_rule,
      )],
    )
    .await;
    assert_eq!(doc[0]["elements"][0]["id"], "login;page-structure;has-a-heading");
  }

  #[tokio::test]
  async fn hooks_are_hidden_steps_named_before_and_after() {
    let dir = scoped("cucumber-hooks");
    let mut reporter = CucumberJsonReporter::new(dir.0.join("cucumber.json"));
    let doc = write(
      &mut reporter,
      vec![scenario(
        "signs in",
        String::new().as_str(),
        vec![
          hook("Before hook"),
          step("Given", "a user", StepStatus::Passed, None),
          hook("After hook"),
        ],
        source(),
      )],
    )
    .await;
    let steps = &doc[0]["elements"][0]["steps"];
    assert_eq!(steps[0]["keyword"], "Before");
    assert_eq!(steps[0]["hidden"], true);
    assert!(steps[0].get("name").is_none(), "a hook step has no name");
    assert_eq!(steps[1]["keyword"], "Given ");
    assert_eq!(steps[2]["keyword"], "After");
    assert_eq!(steps[2]["hidden"], true);
    assert_eq!(
      doc[0]["id"], "login",
      "an unnamed project leaves the feature id unqualified"
    );
  }

  #[tokio::test]
  async fn two_projects_running_one_file_are_two_features() {
    let dir = scoped("cucumber-projects");
    let mut reporter = CucumberJsonReporter::new(dir.0.join("cucumber.json"));
    let doc = write(
      &mut reporter,
      vec![
        scenario(
          "signs in",
          "chromium",
          vec![step("Given", "a user", StepStatus::Passed, None)],
          source(),
        ),
        scenario(
          "signs in",
          "firefox",
          vec![step("Given", "a user", StepStatus::Passed, None)],
          source(),
        ),
      ],
    )
    .await;
    assert_eq!(
      doc.as_array().expect("features").len(),
      2,
      "the key is (uri, project), not the feature name: {doc}",
    );
    assert_eq!(doc[0]["id"], "chromium-›-login");
    assert_eq!(doc[1]["id"], "firefox-›-login");
  }

  #[tokio::test]
  async fn a_step_carries_its_data_table_as_arguments() {
    let dir = scoped("cucumber-args");
    let mut reporter = CucumberJsonReporter::new(dir.0.join("cucumber.json"));
    let mut with_table = step("Given", "these users", StepStatus::Passed, None);
    with_table.metadata = Some(serde_json::json!({
      "bdd_keyword": "Given",
      "bdd_text": "these users",
      "bdd_line": 7,
      "bdd_arguments": [{ "rows": [{ "cells": ["name"] }, { "cells": ["Ada"] }] }],
    }));
    let doc = write(
      &mut reporter,
      vec![scenario("signs in", "chromium", vec![with_table], source())],
    )
    .await;
    assert_eq!(
      doc[0]["elements"][0]["steps"][0]["arguments"],
      serde_json::json!([{ "rows": [{ "cells": ["name"] }, { "cells": ["Ada"] }] }]),
    );
  }

  #[tokio::test]
  async fn attachments_are_skipped_unless_asked_for() {
    let dir = scoped("cucumber-embed");
    let attachment = Attachment {
      name: "screenshot".to_string(),
      content_type: "image/png".to_string(),
      body: AttachmentBody::Bytes(vec![1, 2, 3, 4]),
      step_id: Some("s-a user".to_string()),
    };

    let mut default = CucumberJsonReporter::new(dir.0.join("default.json"));
    let mut outcome = (*scenario(
      "signs in",
      "chromium",
      vec![step("Given", "a user", StepStatus::Passed, None)],
      source(),
    ))
    .clone();
    outcome.attachments = vec![attachment.clone()];
    let doc = write(&mut default, vec![Arc::new(outcome.clone())]).await;
    assert!(
      doc[0]["elements"][0]["steps"][0].get("embeddings").is_none(),
      "attachments are skipped by default, as upstream: {doc}",
    );

    let mut config = crate::config::TestConfig::default();
    config.browser.browser = "chromium".to_string();
    let mut asked = CucumberJsonReporter::new(dir.0.join("asked.json")).with_options(
      &[("skipAttachments".to_string(), serde_json::Value::Bool(false))]
        .into_iter()
        .collect(),
      &config,
    );
    let doc = write(&mut asked, vec![Arc::new(outcome)]).await;
    let embedding = &doc[0]["elements"][0]["steps"][0]["embeddings"][0];
    assert_eq!(embedding["mime_type"], "image/png");
    assert_eq!(embedding["data"], "AQIDBA==");
  }

  #[tokio::test]
  async fn a_failing_step_carries_its_message() {
    let dir = scoped("cucumber-fail");
    let mut reporter = CucumberJsonReporter::new(dir.0.join("cucumber.json"));
    let doc = write(
      &mut reporter,
      vec![scenario(
        "signs in",
        "chromium",
        vec![step(
          "Then",
          "they see the dashboard",
          StepStatus::Failed,
          Some("no dashboard"),
        )],
        source(),
      )],
    )
    .await;
    let result = &doc[0]["elements"][0]["steps"][0]["result"];
    assert_eq!(result["status"], "failed");
    assert_eq!(result["error_message"], "no dashboard");
  }

  #[tokio::test]
  async fn a_test_with_no_gherkin_source_is_still_reported() {
    let dir = scoped("cucumber-bare");
    let mut reporter = CucumberJsonReporter::new(dir.0.join("cucumber.json"));
    let mut outcome = (*scenario(
      "signs in",
      "chromium",
      vec![step("Given", "a user", StepStatus::Passed, None)],
      source(),
    ))
    .clone();
    outcome.case_metadata = None;
    let doc = write(&mut reporter, vec![Arc::new(outcome)]).await;
    assert_eq!(doc[0]["name"], "Login", "the feature name falls back to the suite");
    assert_eq!(doc[0]["keyword"], "Feature");
    assert_eq!(doc[0]["elements"][0]["keyword"], "Scenario");
    assert_eq!(doc[0]["elements"][0]["line"], 5, "and the line to the test's own");
  }
}
