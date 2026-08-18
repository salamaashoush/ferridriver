//! JSON reporter — Playwright's `JSONReport` shape, byte-compatible
//! with `/tmp/playwright/packages/playwright/src/reporters/json.ts`.
//!
//! `{ config, suites, errors, stats }`, where a suite is a file, a spec
//! is a test title within it, a test is that spec under one project, and
//! a result is one attempt. Every consumer of Playwright's JSON report
//! (CI dashboards, `playwright-json-summary`, custom parsers) reads this
//! layout, so it is the layout we emit rather than a flatter one of our
//! own.

use std::path::PathBuf;

use base64::Engine;
use serde::Serialize;

use crate::config::TestConfig;
use crate::model::{AttachmentBody, StepCategory, TestAnnotation, TestOutcome, TestStep};
use crate::reporter::base::{self, ResultCollector, TestRecord};
use crate::reporter::{Reporter, ReporterEvent};

pub struct JsonReporter {
  output_path: PathBuf,
  collector: ResultCollector,
  config: Option<Box<TestConfig>>,
}

// ── Report shape ──

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonReport {
  config: serde_json::Value,
  suites: Vec<JsonSuite>,
  errors: Vec<JsonError>,
  stats: JsonStats,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonStats {
  start_time: String,
  duration: f64,
  expected: usize,
  skipped: usize,
  unexpected: usize,
  flaky: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonSuite {
  title: String,
  file: String,
  line: usize,
  column: usize,
  specs: Vec<JsonSpec>,
  #[serde(skip_serializing_if = "Option::is_none")]
  suites: Option<Vec<JsonSuite>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonSpec {
  title: String,
  ok: bool,
  tags: Vec<String>,
  tests: Vec<JsonTest>,
  id: String,
  file: String,
  line: usize,
  column: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonTest {
  timeout: u128,
  annotations: Vec<JsonAnnotation>,
  expected_status: &'static str,
  project_id: String,
  project_name: String,
  results: Vec<JsonTestResult>,
  status: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonAnnotation {
  #[serde(rename = "type")]
  kind: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  description: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonTestResult {
  worker_index: u32,
  parallel_index: u32,
  status: &'static str,
  duration: u128,
  #[serde(skip_serializing_if = "Option::is_none")]
  error: Option<JsonError>,
  errors: Vec<JsonError>,
  stdout: Vec<JsonStdio>,
  stderr: Vec<JsonStdio>,
  retry: u32,
  #[serde(skip_serializing_if = "Option::is_none")]
  steps: Option<Vec<JsonStep>>,
  start_time: String,
  annotations: Vec<JsonAnnotation>,
  attachments: Vec<JsonAttachment>,
  #[serde(skip_serializing_if = "Option::is_none")]
  error_location: Option<JsonLocation>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonError {
  message: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  stack: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  location: Option<JsonLocation>,
  #[serde(skip_serializing_if = "Option::is_none")]
  snippet: Option<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct JsonLocation {
  file: String,
  line: usize,
  column: usize,
}

#[derive(Serialize)]
#[serde(untagged)]
enum JsonStdio {
  Text { text: String },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonAttachment {
  name: String,
  content_type: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  path: Option<String>,
  /// Base64, the way Playwright serializes an inline attachment body.
  #[serde(skip_serializing_if = "Option::is_none")]
  body: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonStep {
  title: String,
  duration: u128,
  #[serde(skip_serializing_if = "Option::is_none")]
  error: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  steps: Option<Vec<JsonStep>>,
}

impl JsonReporter {
  pub fn new(output_path: PathBuf) -> Self {
    Self {
      output_path,
      collector: ResultCollector::new(),
      config: None,
    }
  }

  /// Carry the run's configuration into the report's `config` block.
  /// Without it the block is `{}` — valid, but a consumer that keys off
  /// `config.projects` sees nothing.
  #[must_use]
  pub fn with_config(mut self, config: &TestConfig) -> Self {
    self.config = Some(Box::new(config.clone()));
    self
  }

  fn serialize(&self) -> JsonReport {
    let counts = self.collector.counts();
    JsonReport {
      config: self
        .config
        .as_deref()
        .map_or_else(|| serde_json::json!({}), crate::reporter::api::full_config),
      suites: self.suites(),
      errors: self.collector.errors.iter().map(|e| json_error(e, None)).collect(),
      stats: JsonStats {
        start_time: self.collector.run.start_iso8601(),
        duration: self.collector.run.duration.as_secs_f64() * 1000.0,
        expected: counts.expected,
        skipped: counts.skipped,
        unexpected: counts.unexpected,
        flaky: counts.flaky,
      },
    }
  }

  /// One suite per file, with a spec per distinct test title. The same
  /// title run by several projects is one spec carrying several tests —
  /// Playwright's `_mergeSuites`.
  fn suites(&self) -> Vec<JsonSuite> {
    self
      .collector
      .by_file()
      .into_iter()
      .map(|(file, records)| {
        let mut specs: Vec<JsonSpec> = Vec::new();
        for record in records {
          let id = record.id();
          let title = id.name.clone();
          let line = id.line.unwrap_or(0);
          let column = id.column.unwrap_or(0);
          let test = json_test(record);
          match specs
            .iter_mut()
            .find(|s| s.title == title && s.line == line && s.column == column)
          {
            Some(spec) => {
              spec.ok = spec.ok && record.ok();
              spec.tests.push(test);
            },
            None => specs.push(JsonSpec {
              title,
              ok: record.ok(),
              tags: record.last().tags(),
              tests: vec![test],
              id: record.stable_id(),
              file: file.clone(),
              line,
              column,
            }),
          }
        }
        JsonSuite {
          title: file.clone(),
          file,
          line: 0,
          column: 0,
          specs,
          suites: None,
        }
      })
      .collect()
  }
}

fn json_test(record: &TestRecord) -> JsonTest {
  let last = record.last();
  JsonTest {
    timeout: last.timeout.as_millis(),
    annotations: annotations(&last.annotations),
    expected_status: base::expected_status_str(last.expected_status),
    project_id: record.key.project.clone(),
    project_name: record.key.project.clone(),
    results: record.attempts.iter().map(|a| json_result(a)).collect(),
    status: record.outcome_kind().as_str(),
  }
}

fn json_result(outcome: &TestOutcome) -> JsonTestResult {
  let errors: Vec<JsonError> = base::attempt_errors(outcome)
    .into_iter()
    .map(|e| json_error(e, Some(&outcome.test_id)))
    .collect();
  let steps = visible_steps(&outcome.steps);
  JsonTestResult {
    worker_index: outcome.worker_index,
    parallel_index: outcome.parallel_index,
    status: outcome.status.as_str(),
    duration: outcome.duration.as_millis(),
    error: outcome.error.as_ref().map(|e| json_error(e, Some(&outcome.test_id))),
    error_location: errors.first().and_then(|e| e.location.clone()),
    errors,
    stdout: stdio(&outcome.stdout),
    stderr: stdio(&outcome.stderr),
    retry: outcome.attempt.saturating_sub(1),
    steps: (!steps.is_empty()).then_some(steps),
    start_time: outcome.start_iso8601(),
    annotations: annotations(&outcome.annotations),
    attachments: outcome
      .attachments
      .iter()
      .map(|a| JsonAttachment {
        name: a.name.clone(),
        content_type: a.content_type.clone(),
        path: match &a.body {
          AttachmentBody::Path(p) => Some(p.display().to_string()),
          AttachmentBody::Bytes(_) => None,
        },
        body: match &a.body {
          AttachmentBody::Bytes(bytes) => Some(base64::engine::general_purpose::STANDARD.encode(bytes)),
          AttachmentBody::Path(_) => None,
        },
      })
      .collect(),
  }
}

fn json_error(failure: &crate::model::TestFailure, test_id: Option<&crate::model::TestId>) -> JsonError {
  let location = failure
    .stack
    .as_deref()
    .and_then(base::parse_error_location)
    .or_else(|| {
      test_id.map(|id| base::ErrorLocation {
        file: id.file.clone(),
        line: id.line.unwrap_or(0),
        column: id.column.unwrap_or(0),
      })
    })
    .map(|loc| JsonLocation {
      file: loc.file,
      line: loc.line,
      column: loc.column,
    });
  JsonError {
    message: base::strip_ansi(&failure.message).into_owned(),
    stack: failure.stack.clone(),
    location,
    // Playwright's `snippet` is the source excerpt around the failure;
    // ours is the rendered assertion diff, which fills the same slot in
    // every consumer that prints it verbatim.
    snippet: failure
      .diff
      .as_ref()
      .map(|d| base::strip_ansi(d).into_owned())
      .filter(|d| !d.trim().is_empty()),
  }
}

fn stdio(text: &str) -> Vec<JsonStdio> {
  if text.is_empty() {
    return Vec::new();
  }
  vec![JsonStdio::Text { text: text.to_string() }]
}

fn annotations(annotations: &[TestAnnotation]) -> Vec<JsonAnnotation> {
  annotations
    .iter()
    .map(|a| match a {
      TestAnnotation::Skip { reason, .. } => JsonAnnotation {
        kind: "skip".into(),
        description: reason.clone(),
      },
      TestAnnotation::Slow { reason, .. } => JsonAnnotation {
        kind: "slow".into(),
        description: reason.clone(),
      },
      TestAnnotation::Fixme { reason, .. } => JsonAnnotation {
        kind: "fixme".into(),
        description: reason.clone(),
      },
      TestAnnotation::Fail { reason, .. } => JsonAnnotation {
        kind: "fail".into(),
        description: reason.clone(),
      },
      TestAnnotation::Only => JsonAnnotation {
        kind: "only".into(),
        description: None,
      },
      TestAnnotation::Tag(tag) => JsonAnnotation {
        kind: "tag".into(),
        description: Some(tag.clone()),
      },
      TestAnnotation::Info { type_name, description } => JsonAnnotation {
        kind: type_name.clone(),
        description: Some(description.clone()),
      },
    })
    .collect()
}

/// Playwright's JSON report carries `test.step` entries only — API
/// calls, fixtures and expects would bury the user's own structure.
fn visible_steps(steps: &[TestStep]) -> Vec<JsonStep> {
  steps
    .iter()
    .filter(|s| s.category == StepCategory::TestStep)
    .map(|s| {
      let nested = visible_steps(&s.steps);
      JsonStep {
        title: s.title.clone(),
        duration: s.duration.as_millis(),
        error: s.error.clone(),
        steps: (!nested.is_empty()).then_some(nested),
      }
    })
    .collect()
}

#[async_trait::async_trait]
impl Reporter for JsonReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    self.collector.observe(event);
  }

  async fn finalize(&mut self) -> ferridriver::error::Result<()> {
    let json = serde_json::to_string_pretty(&self.serialize())?;
    if let Some(parent) = self.output_path.parent() {
      std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&self.output_path, json)?;
    tracing::info!("JSON report written to {}", self.output_path.display());
    Ok(())
  }
}
