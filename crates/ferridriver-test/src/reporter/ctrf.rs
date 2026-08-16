//! CTRF reporter — Common Test Report Format.
//!
//! One JSON schema that a growing set of tools (GitHub PR summaries,
//! flaky-test dashboards, the `ctrf` CLI) consume regardless of which
//! runner produced it. Schema: <https://ctrf.io>.
//!
//! `{ results: { tool, summary, tests[], environment? } }` where each
//! test carries its status, duration, retries, attachments and the
//! errors that failed it.

use std::path::PathBuf;

use serde::Serialize;

use crate::model::{AttachmentBody, TestOutcomeKind, TestStatus};
use crate::reporter::base::{self, ResultCollector, TestRecord};
use crate::reporter::{Reporter, ReporterEvent};

pub struct CtrfReporter {
  output_path: PathBuf,
  collector: ResultCollector,
}

#[derive(Serialize)]
struct CtrfReport {
  results: CtrfResults,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CtrfResults {
  tool: CtrfTool,
  summary: CtrfSummary,
  tests: Vec<CtrfTest>,
  #[serde(skip_serializing_if = "Option::is_none")]
  environment: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct CtrfTool {
  name: &'static str,
  version: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CtrfSummary {
  tests: usize,
  passed: usize,
  failed: usize,
  pending: usize,
  skipped: usize,
  other: usize,
  start: i64,
  stop: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CtrfTest {
  name: String,
  status: &'static str,
  duration: u128,
  #[serde(skip_serializing_if = "Option::is_none")]
  start: Option<i64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  stop: Option<i64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  suite: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  message: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  trace: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  line: Option<usize>,
  #[serde(skip_serializing_if = "Option::is_none")]
  file_path: Option<String>,
  retries: u32,
  flaky: bool,
  #[serde(skip_serializing_if = "Vec::is_empty")]
  tags: Vec<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  browser: Option<String>,
  #[serde(skip_serializing_if = "Vec::is_empty")]
  attachments: Vec<CtrfAttachment>,
  #[serde(skip_serializing_if = "Vec::is_empty")]
  steps: Vec<CtrfStep>,
  #[serde(skip_serializing_if = "Option::is_none")]
  stdout: Option<Vec<String>>,
  #[serde(skip_serializing_if = "Option::is_none")]
  stderr: Option<Vec<String>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CtrfAttachment {
  name: String,
  content_type: String,
  path: String,
}

#[derive(Serialize)]
struct CtrfStep {
  name: String,
  status: &'static str,
}

impl CtrfReporter {
  #[must_use]
  pub fn new(output_path: PathBuf) -> Self {
    Self {
      output_path,
      collector: ResultCollector::new(),
    }
  }

  fn serialize(&self) -> CtrfReport {
    let counts = self.collector.counts();
    let start = self
      .collector
      .run
      .start_time
      .duration_since(std::time::UNIX_EPOCH)
      .ok()
      .and_then(|d| i64::try_from(d.as_millis()).ok())
      .unwrap_or_default();
    let tests: Vec<CtrfTest> = self.collector.records().iter().map(ctrf_test).collect();
    CtrfReport {
      results: CtrfResults {
        tool: CtrfTool {
          name: "ferridriver",
          version: env!("CARGO_PKG_VERSION"),
        },
        summary: CtrfSummary {
          tests: tests.len(),
          passed: counts.expected + counts.flaky,
          failed: counts.unexpected,
          pending: 0,
          skipped: counts.skipped,
          other: 0,
          start,
          stop: start + i64::try_from(self.collector.run.duration.as_millis()).unwrap_or_default(),
        },
        tests,
        environment: (!self.collector.run.metadata.is_null()).then(|| self.collector.run.metadata.clone()),
      },
    }
  }
}

/// CTRF's status vocabulary: `passed | failed | skipped | pending | other`.
fn ctrf_status(kind: TestOutcomeKind, status: TestStatus) -> &'static str {
  match kind {
    TestOutcomeKind::Expected | TestOutcomeKind::Flaky => "passed",
    TestOutcomeKind::Skipped => "skipped",
    TestOutcomeKind::Unexpected => match status {
      TestStatus::Interrupted => "other",
      _ => "failed",
    },
  }
}

fn ctrf_test(record: &TestRecord) -> CtrfTest {
  let last = record.last();
  let kind = record.outcome_kind();
  let error = base::attempt_errors(last).into_iter().next();
  let start = last.start_epoch_ms();
  CtrfTest {
    name: record.id().name.clone(),
    status: ctrf_status(kind, last.status),
    duration: record.total_duration().as_millis(),
    start: (start > 0).then_some(start),
    stop: (start > 0).then_some(start + i64::try_from(last.duration.as_millis()).unwrap_or_default()),
    suite: record.id().suite.clone(),
    message: error.map(|e| base::strip_ansi(&e.message).into_owned()),
    trace: error.and_then(|e| e.stack.clone()),
    line: record.id().line,
    file_path: Some(record.key.file.clone()),
    retries: last.attempt.saturating_sub(1),
    flaky: kind == TestOutcomeKind::Flaky,
    tags: last.tags(),
    browser: (!record.key.project.is_empty()).then(|| record.key.project.clone()),
    attachments: last
      .attachments
      .iter()
      .filter_map(|a| match &a.body {
        AttachmentBody::Path(path) => Some(CtrfAttachment {
          name: a.name.clone(),
          content_type: a.content_type.clone(),
          path: path.display().to_string(),
        }),
        AttachmentBody::Bytes(_) => None,
      })
      .collect(),
    steps: last
      .steps
      .iter()
      .filter(|s| s.category.is_visible())
      .map(|s| CtrfStep {
        name: s.title.clone(),
        status: match s.status {
          crate::model::StepStatus::Passed => "passed",
          crate::model::StepStatus::Failed => "failed",
          crate::model::StepStatus::Skipped => "skipped",
          crate::model::StepStatus::Pending => "pending",
        },
      })
      .collect(),
    stdout: (!last.stdout.is_empty()).then(|| vec![last.stdout.clone()]),
    stderr: (!last.stderr.is_empty()).then(|| vec![last.stderr.clone()]),
  }
}

#[async_trait::async_trait]
impl Reporter for CtrfReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    self.collector.observe(event);
  }

  async fn finalize(&mut self) -> ferridriver::error::Result<()> {
    let json = serde_json::to_string_pretty(&self.serialize())?;
    if let Some(parent) = self.output_path.parent() {
      std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&self.output_path, json)?;
    tracing::info!("CTRF report written to {}", self.output_path.display());
    Ok(())
  }
}
