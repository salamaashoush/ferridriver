//! `blob` reporter — emits a `report.zip` containing every
//! `ReporterEvent` as a JSON-lines stream. Mirrors Playwright's
//! `/tmp/playwright/packages/playwright/src/reporters/blob.ts`.
//!
//! The blob is the input to `ferridriver merge-reports <dir>`, which
//! replays the merged event stream through whatever reporters the merge
//! is configured with. That only works if the blob is *lossless*: a
//! merged HTML or JUnit report is built from these events and nothing
//! else, so an event shape that drops steps, attachments or stacks
//! silently degrades every merged report. Everything an outcome carries
//! round-trips.
//!
//! Inline attachment bytes (a failure screenshot) are written straight
//! into the zip as `resources/<sha1>` entries as they arrive, rather
//! than base64'd into the JSONL — that keeps them out of the reporter's
//! memory for the length of the run and out of the text stream.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

use super::{Reporter, ReporterEvent, RunStatus, StepFinishedEvent, StepStartedEvent, TestOutputEvent};
use crate::model::{
  Attachment, AttachmentBody, ExpectedStatus, StepCategory, StepLocation, StepStatus, TestAnnotation, TestFailure,
  TestId, TestOutcome, TestStatus, TestStep,
};

/// Bumped when the wire shape changes in a way an older reader cannot
/// handle. Readers accept anything up to their own version.
///
/// 3 turned a step's `location` from a `"file:line"` string into
/// `{file, line, column}` and added it to `step-started`, plus step
/// annotations and an attachment's owning step. The reader still takes
/// the string form, so a merge across the boundary keeps every shard's
/// step locations.
///
/// 4 names the project on every per-test event and carries the run's
/// `FullConfig` + `Suite` tree on `run-started`. Both are `#[serde
/// (default)]`, so a schema-3 blob still replays — with an empty
/// preamble, which is what a reporter that needs the tree would see
/// for a shard written by an older build.
const SCHEMA_VERSION: u32 = 4;

/// Wire-format mirror of `ReporterEvent`. Distinct from the runtime
/// enum so adding a new event variant doesn't break stored blobs and
/// vice-versa — the Wire shape is the contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum WireEvent {
  Header {
    schema: u32,
    shard_index: Option<u32>,
    shard_total: Option<u32>,
  },
  RunStarted {
    total_tests: usize,
    num_workers: u32,
    metadata: serde_json::Value,
    #[serde(default)]
    start_time_ms: u64,
    /// The run's `FullConfig` + `Suite` tree. Absent in a blob written
    /// before schema 4, which replays as an empty preamble.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    preamble: Option<crate::reporter::api::RunPreamble>,
  },
  WorkerStarted {
    worker_id: u32,
  },
  TestStarted {
    test_id: WireTestId,
    #[serde(default)]
    project: String,
    attempt: u32,
    #[serde(default)]
    worker_id: u32,
  },
  StepStarted {
    test_id: WireTestId,
    #[serde(default)]
    project: String,
    step_id: String,
    parent_step_id: Option<String>,
    title: String,
    category: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    location: Option<WireStepLocation>,
  },
  StepFinished {
    test_id: WireTestId,
    #[serde(default)]
    project: String,
    step_id: String,
    title: String,
    category: String,
    duration_ms: u64,
    error: Option<String>,
    metadata: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    annotations: Vec<TestAnnotation>,
  },
  TestOutput {
    test_id: WireTestId,
    #[serde(default)]
    project: String,
    stderr: bool,
    text: String,
  },
  TestFinished {
    outcome: Box<WireOutcome>,
  },
  RunError {
    error: WireFailure,
  },
  WorkerFinished {
    worker_id: u32,
  },
  RunFinished {
    total: usize,
    passed: usize,
    failed: usize,
    skipped: usize,
    flaky: usize,
    duration_ms: u64,
    #[serde(default)]
    status: String,
  },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireTestId {
  pub file: String,
  pub suite: Option<String>,
  pub name: String,
  pub line: Option<usize>,
  #[serde(default)]
  pub column: Option<usize>,
}

/// Every field of a [`TestOutcome`]. A merged report is rebuilt from
/// this and nothing else.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireOutcome {
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub case_metadata: Option<serde_json::Value>,
  pub test_id: WireTestId,
  pub status: String,
  pub duration_ms: u64,
  pub attempt: u32,
  #[serde(default = "one")]
  pub max_attempts: u32,
  #[serde(default)]
  pub error: Option<WireFailure>,
  #[serde(default)]
  pub errors: Vec<WireFailure>,
  #[serde(default)]
  pub attachments: Vec<WireAttachment>,
  #[serde(default)]
  pub steps: Vec<WireStep>,
  #[serde(default, skip_serializing_if = "String::is_empty")]
  pub stdout: String,
  #[serde(default, skip_serializing_if = "String::is_empty")]
  pub stderr: String,
  #[serde(default)]
  pub annotations: Vec<TestAnnotation>,
  #[serde(default)]
  pub metadata: serde_json::Value,
  #[serde(default, skip_serializing_if = "String::is_empty")]
  pub project_name: String,
  #[serde(default)]
  pub worker_index: u32,
  #[serde(default)]
  pub parallel_index: u32,
  #[serde(default)]
  pub start_time_ms: u64,
  #[serde(default)]
  pub expected_failure: bool,
  #[serde(default)]
  pub timeout_ms: u64,
}

fn one() -> u32 {
  1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireFailure {
  pub message: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub stack: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub diff: Option<String>,
  /// Resource name of the failure screenshot inside the zip.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub screenshot: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireAttachment {
  pub name: String,
  pub content_type: String,
  /// Exactly one of these is set: a path the artifact already lives at,
  /// or the zip entry its bytes were stored under.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub path: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub resource: Option<String>,
  /// The step it was attached from (`stepInfo.attach`).
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub step_id: Option<String>,
}

/// A step's location on the wire.
///
/// Schema 2 and older wrote `"file:line"`; schema 3 writes the object
/// Playwright's `Location` has. Both are read, so `merge-reports` over
/// shards written by different builds keeps every location it is given.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WireStepLocation {
  Structured(StepLocation),
  Legacy(String),
}

impl From<StepLocation> for WireStepLocation {
  fn from(location: StepLocation) -> Self {
    Self::Structured(location)
  }
}

impl WireStepLocation {
  #[must_use]
  pub fn into_runtime(self) -> Option<StepLocation> {
    match self {
      Self::Structured(location) => Some(location),
      Self::Legacy(text) => StepLocation::parse(&text),
    }
  }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireStep {
  pub step_id: String,
  pub title: String,
  pub category: String,
  pub duration_ms: u64,
  pub status: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub error: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub location: Option<WireStepLocation>,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub annotations: Vec<TestAnnotation>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub parent_step_id: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub metadata: Option<serde_json::Value>,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub steps: Vec<WireStep>,
}

impl From<&TestId> for WireTestId {
  fn from(id: &TestId) -> Self {
    Self {
      file: id.file.clone(),
      suite: id.suite.clone(),
      name: id.name.clone(),
      line: id.line,
      column: id.column,
    }
  }
}

impl From<WireTestId> for TestId {
  fn from(w: WireTestId) -> Self {
    Self {
      file: w.file,
      suite: w.suite,
      name: w.name,
      line: w.line,
      column: w.column,
    }
  }
}

fn step_category_str(c: &StepCategory) -> &'static str {
  match c {
    StepCategory::TestStep => "test-step",
    StepCategory::Expect => "expect",
    StepCategory::Fixture => "fixture",
    StepCategory::Hook => "hook",
    StepCategory::PwApi => "pw-api",
  }
}

fn parse_step_category(s: &str) -> StepCategory {
  match s {
    "expect" => StepCategory::Expect,
    "fixture" => StepCategory::Fixture,
    "hook" => StepCategory::Hook,
    "pw-api" => StepCategory::PwApi,
    _ => StepCategory::TestStep,
  }
}

fn step_status_str(s: StepStatus) -> &'static str {
  match s {
    StepStatus::Passed => "passed",
    StepStatus::Failed => "failed",
    StepStatus::Skipped => "skipped",
    StepStatus::Pending => "pending",
  }
}

fn parse_step_status(s: &str) -> StepStatus {
  match s {
    "failed" => StepStatus::Failed,
    "skipped" => StepStatus::Skipped,
    "pending" => StepStatus::Pending,
    _ => StepStatus::Passed,
  }
}

fn epoch_ms(time: SystemTime) -> u64 {
  time
    .duration_since(SystemTime::UNIX_EPOCH)
    .ok()
    .and_then(|d| u64::try_from(d.as_millis()).ok())
    .unwrap_or_default()
}

fn from_epoch_ms(ms: u64) -> SystemTime {
  SystemTime::UNIX_EPOCH + Duration::from_millis(ms)
}

fn wire_steps(steps: &[TestStep]) -> Vec<WireStep> {
  steps
    .iter()
    .map(|s| WireStep {
      step_id: s.step_id.clone(),
      title: s.title.clone(),
      category: step_category_str(&s.category).to_string(),
      duration_ms: u64::try_from(s.duration.as_millis()).unwrap_or(u64::MAX),
      status: step_status_str(s.status).to_string(),
      error: s.error.clone(),
      location: s.location.clone().map(WireStepLocation::from),
      annotations: s.annotations.clone(),
      parent_step_id: s.parent_step_id.clone(),
      metadata: s.metadata.clone(),
      steps: wire_steps(&s.steps),
    })
    .collect()
}

fn runtime_steps(steps: Vec<WireStep>) -> Vec<TestStep> {
  steps
    .into_iter()
    .map(|s| TestStep {
      step_id: s.step_id,
      title: s.title,
      category: parse_step_category(&s.category),
      duration: Duration::from_millis(s.duration_ms),
      status: parse_step_status(&s.status),
      error: s.error,
      location: s.location.and_then(WireStepLocation::into_runtime),
      annotations: s.annotations,
      parent_step_id: s.parent_step_id,
      metadata: s.metadata,
      steps: runtime_steps(s.steps),
    })
    .collect()
}

/// Storage for inline artifact bytes. Implemented by the writer (stores
/// into the zip) and by the reader (resolves back out of it).
trait ResourceSink {
  fn store(&mut self, name_hint: &str, content_type: &str, bytes: &[u8]) -> Option<String>;
}

impl WireOutcome {
  fn from_runtime(outcome: &TestOutcome, sink: &mut dyn ResourceSink) -> Self {
    Self {
      case_metadata: outcome.case_metadata.clone(),
      test_id: (&outcome.test_id).into(),
      status: outcome.status.as_str().to_string(),
      duration_ms: u64::try_from(outcome.duration.as_millis()).unwrap_or(u64::MAX),
      attempt: outcome.attempt,
      max_attempts: outcome.max_attempts,
      error: outcome.error.as_ref().map(|e| wire_failure(e, sink)),
      errors: outcome.errors.iter().map(|e| wire_failure(e, sink)).collect(),
      attachments: outcome
        .attachments
        .iter()
        .map(|a| match &a.body {
          AttachmentBody::Path(path) => WireAttachment {
            name: a.name.clone(),
            content_type: a.content_type.clone(),
            path: Some(path.display().to_string()),
            resource: None,
            step_id: a.step_id.clone(),
          },
          AttachmentBody::Bytes(bytes) => WireAttachment {
            name: a.name.clone(),
            content_type: a.content_type.clone(),
            path: None,
            resource: sink.store(&a.name, &a.content_type, bytes),
            step_id: a.step_id.clone(),
          },
        })
        .collect(),
      steps: wire_steps(&outcome.steps),
      stdout: outcome.stdout.clone(),
      stderr: outcome.stderr.clone(),
      annotations: outcome.annotations.clone(),
      metadata: outcome.metadata.clone(),
      project_name: outcome.project_name.clone(),
      worker_index: outcome.worker_index,
      parallel_index: outcome.parallel_index,
      start_time_ms: epoch_ms(outcome.start_time),
      expected_failure: outcome.expected_status == ExpectedStatus::Fail,
      timeout_ms: u64::try_from(outcome.timeout.as_millis()).unwrap_or(u64::MAX),
    }
  }

  fn into_runtime(self, resources: &FxHashMap<String, Vec<u8>>) -> TestOutcome {
    let id: TestId = self.test_id.into();
    TestOutcome {
      case_metadata: self.case_metadata,
      test_id: id,
      status: TestStatus::parse(&self.status),
      duration: Duration::from_millis(self.duration_ms),
      attempt: self.attempt,
      max_attempts: self.max_attempts,
      error: self.error.map(|e| runtime_failure(e, resources)),
      errors: self.errors.into_iter().map(|e| runtime_failure(e, resources)).collect(),
      attachments: self
        .attachments
        .into_iter()
        .filter_map(|a| {
          let body = match (a.path, a.resource) {
            (Some(path), _) => AttachmentBody::Path(PathBuf::from(path)),
            (None, Some(resource)) => AttachmentBody::Bytes(resources.get(&resource).cloned()?),
            (None, None) => return None,
          };
          Some(Attachment {
            name: a.name,
            content_type: a.content_type,
            body,
            step_id: a.step_id,
          })
        })
        .collect(),
      steps: runtime_steps(self.steps),
      stdout: self.stdout,
      stderr: self.stderr,
      annotations: self.annotations,
      metadata: self.metadata,
      project_name: self.project_name,
      worker_index: self.worker_index,
      parallel_index: self.parallel_index,
      start_time: from_epoch_ms(self.start_time_ms),
      expected_status: if self.expected_failure {
        ExpectedStatus::Fail
      } else {
        ExpectedStatus::Pass
      },
      timeout: Duration::from_millis(self.timeout_ms),
    }
  }
}

fn wire_failure(failure: &TestFailure, sink: &mut dyn ResourceSink) -> WireFailure {
  WireFailure {
    message: failure.message.clone(),
    stack: failure.stack.clone(),
    diff: failure.diff.clone(),
    screenshot: failure
      .screenshot
      .as_ref()
      .and_then(|bytes| sink.store("failure", "image/png", bytes)),
  }
}

fn runtime_failure(failure: WireFailure, resources: &FxHashMap<String, Vec<u8>>) -> TestFailure {
  TestFailure {
    message: failure.message,
    stack: failure.stack,
    diff: failure.diff,
    screenshot: failure.screenshot.and_then(|name| resources.get(&name).cloned()),
  }
}

impl WireEvent {
  fn from_runtime(event: &ReporterEvent, sink: &mut dyn ResourceSink) -> Option<Self> {
    Some(match event {
      ReporterEvent::RunStarted {
        total_tests,
        num_workers,
        metadata,
        start_time,
        preamble,
      } => Self::RunStarted {
        total_tests: *total_tests,
        num_workers: *num_workers,
        metadata: metadata.clone(),
        start_time_ms: epoch_ms(*start_time),
        preamble: Some((**preamble).clone()),
      },
      ReporterEvent::WorkerStarted { worker_id } => Self::WorkerStarted { worker_id: *worker_id },
      ReporterEvent::TestStarted {
        test_id,
        project,
        attempt,
        worker_id,
      } => Self::TestStarted {
        test_id: test_id.into(),
        project: project.clone(),
        attempt: *attempt,
        worker_id: *worker_id,
      },
      ReporterEvent::StepStarted(s) => Self::StepStarted {
        test_id: (&s.test_id).into(),
        project: s.project.clone(),
        step_id: s.step_id.clone(),
        parent_step_id: s.parent_step_id.clone(),
        title: s.title.clone(),
        category: step_category_str(&s.category).to_string(),
        location: s.location.clone().map(WireStepLocation::from),
      },
      ReporterEvent::StepFinished(s) => Self::StepFinished {
        test_id: (&s.test_id).into(),
        project: s.project.clone(),
        step_id: s.step_id.clone(),
        title: s.title.clone(),
        category: step_category_str(&s.category).to_string(),
        duration_ms: u64::try_from(s.duration.as_millis()).unwrap_or(u64::MAX),
        error: s.error.clone(),
        metadata: s.metadata.clone(),
        annotations: s.annotations.clone(),
      },
      ReporterEvent::TestOutput(o) => Self::TestOutput {
        test_id: (&o.test_id).into(),
        project: o.project.clone(),
        stderr: o.stderr,
        text: o.text.clone(),
      },
      ReporterEvent::TestFinished { outcome } => Self::TestFinished {
        outcome: Box::new(WireOutcome::from_runtime(outcome, sink)),
      },
      ReporterEvent::RunError { error } => Self::RunError {
        error: wire_failure(error, sink),
      },
      ReporterEvent::WorkerFinished { worker_id } => Self::WorkerFinished { worker_id: *worker_id },
      ReporterEvent::RunFinished {
        total,
        passed,
        failed,
        skipped,
        flaky,
        duration,
        status,
      } => Self::RunFinished {
        total: *total,
        passed: *passed,
        failed: *failed,
        skipped: *skipped,
        flaky: *flaky,
        duration_ms: u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
        status: status.as_str().to_string(),
      },
    })
  }

  /// Lower a wire event back into the runtime variant. Header
  /// frames return `None` since they're metadata, not test events.
  #[must_use]
  pub fn into_runtime_with(self, resources: &FxHashMap<String, Vec<u8>>) -> Option<ReporterEvent> {
    Some(match self {
      Self::Header { .. } => return None,
      Self::RunStarted {
        total_tests,
        num_workers,
        metadata,
        start_time_ms,
        preamble,
      } => ReporterEvent::RunStarted {
        total_tests,
        num_workers,
        metadata,
        start_time: from_epoch_ms(start_time_ms),
        preamble: Arc::new(preamble.unwrap_or_else(crate::reporter::api::RunPreamble::empty)),
      },
      Self::WorkerStarted { worker_id } => ReporterEvent::WorkerStarted { worker_id },
      Self::TestStarted {
        test_id,
        project,
        attempt,
        worker_id,
      } => ReporterEvent::TestStarted {
        test_id: test_id.into(),
        project,
        attempt,
        worker_id,
      },
      Self::StepStarted {
        test_id,
        project,
        step_id,
        parent_step_id,
        title,
        category,
        location,
      } => ReporterEvent::StepStarted(Arc::new(StepStartedEvent {
        test_id: test_id.into(),
        project,
        step_id,
        parent_step_id,
        title,
        category: parse_step_category(&category),
        location: location.and_then(WireStepLocation::into_runtime),
      })),
      Self::StepFinished {
        test_id,
        project,
        step_id,
        title,
        category,
        duration_ms,
        error,
        metadata,
        annotations,
      } => ReporterEvent::StepFinished(Arc::new(StepFinishedEvent {
        test_id: test_id.into(),
        project,
        step_id,
        title,
        category: parse_step_category(&category),
        duration: Duration::from_millis(duration_ms),
        error,
        metadata,
        annotations,
      })),
      Self::TestOutput {
        test_id,
        project,
        stderr,
        text,
      } => ReporterEvent::TestOutput(Arc::new(TestOutputEvent {
        test_id: test_id.into(),
        project,
        stderr,
        text,
      })),
      Self::TestFinished { outcome } => ReporterEvent::TestFinished {
        outcome: Arc::new(outcome.into_runtime(resources)),
      },
      Self::RunError { error } => ReporterEvent::RunError {
        error: Box::new(runtime_failure(error, resources)),
      },
      Self::WorkerFinished { worker_id } => ReporterEvent::WorkerFinished { worker_id },
      Self::RunFinished {
        total,
        passed,
        failed,
        skipped,
        flaky,
        duration_ms,
        status,
      } => ReporterEvent::RunFinished {
        total,
        passed,
        failed,
        skipped,
        flaky,
        duration: Duration::from_millis(duration_ms),
        status: RunStatus::parse(&status),
      },
    })
  }
}

/// `--reporter blob` writes one `report-<shard>.zip` per run; each
/// zip contains an `events.jsonl` member plus a `resources/` entry per
/// inline artifact. The merge subcommand reads every zip in a
/// directory, concats the streams, and replays them through the
/// configured reporter.
pub struct BlobReporter {
  out_path: PathBuf,
  buffer: Vec<u8>,
  shard_index: Option<u32>,
  shard_total: Option<u32>,
  /// Opened on the first event so an unused reporter leaves no file.
  writer: Option<ResourceWriter>,
}

/// The zip under construction, plus the resource names already in it.
struct ResourceWriter {
  zip: zip::ZipWriter<std::fs::File>,
  seen: std::collections::HashSet<String>,
  failed: bool,
}

impl ResourceSink for ResourceWriter {
  fn store(&mut self, name_hint: &str, content_type: &str, bytes: &[u8]) -> Option<String> {
    if self.failed {
      return None;
    }
    // Content-addressed, so a screenshot attached twice (once as the
    // failure image, once as a named attachment) is stored once.
    let digest = ferridriver::tracing::sha1_hex(bytes);
    let name = format!(
      "resources/{}-{}{}",
      sanitize(name_hint),
      &digest[..16],
      extension_for(content_type)
    );
    if self.seen.contains(&name) {
      return Some(name);
    }
    let options: zip::write::SimpleFileOptions =
      zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    if let Err(e) = self.zip.start_file(&name, options) {
      tracing::warn!("blob: could not store resource {name}: {e}");
      self.failed = true;
      return None;
    }
    if let Err(e) = self.zip.write_all(bytes) {
      tracing::warn!("blob: could not write resource {name}: {e}");
      self.failed = true;
      return None;
    }
    self.seen.insert(name.clone());
    Some(name)
  }
}

/// A sink for callers that have nowhere to put bytes — inline artifacts
/// are dropped rather than inlined into the text stream.
struct NoResources;

impl ResourceSink for NoResources {
  fn store(&mut self, _name_hint: &str, _content_type: &str, _bytes: &[u8]) -> Option<String> {
    None
  }
}

fn sanitize(name: &str) -> String {
  name
    .chars()
    .map(|c| {
      if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
        c
      } else {
        '-'
      }
    })
    .collect()
}

fn extension_for(content_type: &str) -> &'static str {
  match content_type {
    "image/png" => ".png",
    "image/jpeg" => ".jpg",
    "video/webm" => ".webm",
    "application/zip" => ".zip",
    "text/plain" => ".txt",
    "application/json" => ".json",
    _ => "",
  }
}

impl BlobReporter {
  /// Construct a blob reporter that writes to `out_path` on
  /// `finalize()`. Shard metadata (if known) is recorded in the
  /// header frame so the merger can preserve the run boundary.
  #[must_use]
  pub fn new(out_path: PathBuf) -> Self {
    Self {
      out_path,
      buffer: Vec::new(),
      shard_index: None,
      shard_total: None,
      writer: None,
    }
  }

  pub fn with_shard(mut self, current: u32, total: u32) -> Self {
    self.shard_index = Some(current);
    self.shard_total = Some(total);
    self
  }

  fn header(&self) -> WireEvent {
    WireEvent::Header {
      schema: SCHEMA_VERSION,
      shard_index: self.shard_index,
      shard_total: self.shard_total,
    }
  }

  /// The zip, opened on demand. `None` when it could not be created —
  /// the run keeps going and `finalize` reports the failure.
  fn writer(&mut self) -> Option<&mut ResourceWriter> {
    if self.writer.is_none() {
      if let Some(parent) = self.out_path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
      {
        tracing::warn!("blob: could not create {}: {e}", parent.display());
        return None;
      }
      let file = match std::fs::File::create(&self.out_path) {
        Ok(file) => file,
        Err(e) => {
          tracing::warn!("blob: could not create {}: {e}", self.out_path.display());
          return None;
        },
      };
      self.writer = Some(ResourceWriter {
        zip: zip::ZipWriter::new(file),
        seen: std::collections::HashSet::new(),
        failed: false,
      });
      let header = self.header();
      write_event(&mut self.buffer, &header);
    }
    self.writer.as_mut()
  }
}

#[async_trait]
impl Reporter for BlobReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    // Inline artifact bytes go into the zip as they arrive; only the
    // (small) text line is held until finalize.
    let wire = match self.writer() {
      Some(writer) => WireEvent::from_runtime(event, writer),
      None => WireEvent::from_runtime(event, &mut NoResources),
    };
    if let Some(wire) = wire {
      write_event(&mut self.buffer, &wire);
    }
  }

  async fn finalize(&mut self) -> ferridriver::error::Result<()> {
    use ferridriver::FerriError;
    // No event ever arrived; still produce a valid, empty blob so a merge
    // over a shard that ran nothing does not error. Opening the writer is
    // what emits the header.
    self.writer();
    let Some(writer) = self.writer.take() else {
      return Err(FerriError::backend(format!(
        "blob: could not open {}",
        self.out_path.display()
      )));
    };
    let mut zip = writer.zip;
    let opts: zip::write::SimpleFileOptions =
      zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    zip
      .start_file("events.jsonl", opts)
      .map_err(|e| FerriError::backend(format!("zip start_file: {e}")))?;
    zip
      .write_all(&self.buffer)
      .map_err(|e| FerriError::backend(format!("zip write: {e}")))?;
    zip
      .finish()
      .map_err(|e| FerriError::backend(format!("zip finish: {e}")))?;
    Ok(())
  }
}

fn write_event(buffer: &mut Vec<u8>, event: &WireEvent) {
  match serde_json::to_string(event) {
    Ok(line) => {
      buffer.extend_from_slice(line.as_bytes());
      buffer.push(b'\n');
    },
    // Silence here would make a blob quietly incomplete, and the merge
    // that reads it would report fewer tests than ran.
    Err(e) => tracing::error!("blob: could not serialize event: {e}"),
  }
}

/// Read every `report-*.zip` (or any `*.zip`) under `dir` and return
/// the concatenated runtime event stream.
///
/// # Errors
///
/// Returns an error if a zip is unreadable or contains malformed JSON.
pub fn read_blob_dir(dir: &std::path::Path) -> Result<Vec<ReporterEvent>, String> {
  let mut events = Vec::new();
  let entries = std::fs::read_dir(dir).map_err(|e| format!("read_dir {}: {e}", dir.display()))?;
  let mut zips: Vec<PathBuf> = Vec::new();
  for entry in entries {
    let entry = entry.map_err(|e| format!("dir entry: {e}"))?;
    let path = entry.path();
    if path.extension().and_then(|s| s.to_str()) == Some("zip") {
      zips.push(path);
    }
  }
  zips.sort();
  for path in zips {
    events.extend(read_blob(&path)?);
  }
  Ok(events)
}

/// Read one blob zip back into the runtime event stream, restoring the
/// inline artifacts stored alongside it.
///
/// # Errors
///
/// Returns an error if the zip is unreadable, is missing its event
/// stream, or contains a malformed line.
pub fn read_blob(path: &std::path::Path) -> Result<Vec<ReporterEvent>, String> {
  use std::io::Read;

  let file = std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
  let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("zip read {}: {e}", path.display()))?;

  let mut resources: FxHashMap<String, Vec<u8>> = FxHashMap::default();
  let names: Vec<String> = zip.file_names().map(ToString::to_string).collect();
  for name in names {
    if !name.starts_with("resources/") {
      continue;
    }
    let mut entry = zip
      .by_name(&name)
      .map_err(|e| format!("read {name} in {}: {e}", path.display()))?;
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes).map_err(|e| format!("read {name}: {e}"))?;
    resources.insert(name, bytes);
  }

  let mut buf = String::new();
  zip
    .by_name("events.jsonl")
    .map_err(|e| format!("missing events.jsonl in {}: {e}", path.display()))?
    .read_to_string(&mut buf)
    .map_err(|e| format!("read jsonl: {e}"))?;

  let mut events = Vec::new();
  for (i, line) in buf.lines().enumerate() {
    if line.trim().is_empty() {
      continue;
    }
    let wire: WireEvent =
      serde_json::from_str(line).map_err(|e| format!("parse line {i} in {}: {e}", path.display()))?;
    if let WireEvent::Header { schema, .. } = &wire
      && *schema > SCHEMA_VERSION
    {
      return Err(format!(
        "{}: blob schema {schema} is newer than this build understands ({SCHEMA_VERSION})",
        path.display()
      ));
    }
    if let Some(event) = wire.into_runtime_with(&resources) {
      events.push(event);
    }
  }
  Ok(events)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn an_outcome_round_trips_with_its_steps_and_artifacts() {
    let dir = std::env::temp_dir().join(format!("ferri-blob-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("report.zip");

    let outcome = TestOutcome {
      test_id: TestId {
        file: "a.spec.ts".into(),
        suite: Some("a.spec.ts::group".into()),
        name: "works".into(),
        line: Some(9),
        column: Some(2),
      },
      status: TestStatus::TimedOut,
      duration: Duration::from_millis(1234),
      attempt: 2,
      max_attempts: 3,
      error: Some(TestFailure {
        message: "boom".into(),
        stack: Some("at a.spec.ts:9:2".into()),
        diff: Some("- a\n+ b".into()),
        screenshot: Some(vec![1, 2, 3, 4]),
      }),
      errors: vec![TestFailure {
        message: "boom".into(),
        stack: None,
        diff: None,
        screenshot: None,
      }],
      attachments: vec![Attachment {
        name: "shot".into(),
        content_type: "image/png".into(),
        body: AttachmentBody::Bytes(vec![9, 9, 9]),
        step_id: None,
      }],
      steps: vec![TestStep {
        step_id: "s1".into(),
        title: "outer".into(),
        category: StepCategory::TestStep,
        duration: Duration::from_millis(5),
        status: StepStatus::Failed,
        error: Some("nope".into()),
        location: Some(StepLocation::new("a.spec.ts", 10)),
        annotations: Vec::new(),
        parent_step_id: None,
        metadata: None,
        steps: vec![TestStep {
          step_id: "s2".into(),
          title: "inner".into(),
          category: StepCategory::TestStep,
          duration: Duration::from_millis(2),
          status: StepStatus::Passed,
          error: None,
          location: None,
          annotations: Vec::new(),
          parent_step_id: Some("s1".into()),
          metadata: None,
          steps: Vec::new(),
        }],
      }],
      stdout: "hello\n".into(),
      annotations: vec![TestAnnotation::Tag("@smoke".into())],
      project_name: "chromium".into(),
      worker_index: 3,
      parallel_index: 3,
      start_time: from_epoch_ms(1_700_000_000_000),
      expected_status: ExpectedStatus::Fail,
      timeout: Duration::from_secs(30),
      ..Default::default()
    };

    let mut reporter = BlobReporter::new(path.clone());
    reporter
      .on_event(&ReporterEvent::TestFinished {
        outcome: Arc::new(outcome.clone()),
      })
      .await;
    reporter.finalize().await.expect("finalize");

    let events = read_blob(&path).expect("read blob");
    let ReporterEvent::TestFinished { outcome: back } = &events[0] else {
      panic!("expected TestFinished, got {:?}", events[0]);
    };

    assert_eq!(back.status, TestStatus::TimedOut);
    assert_eq!(back.test_id.column, Some(2));
    assert_eq!(back.attempt, 2);
    assert_eq!(back.max_attempts, 3);
    assert_eq!(back.project_name, "chromium");
    assert_eq!(back.worker_index, 3);
    assert_eq!(back.expected_status, ExpectedStatus::Fail);
    assert_eq!(back.timeout, Duration::from_secs(30));
    assert_eq!(back.start_time, from_epoch_ms(1_700_000_000_000));
    assert_eq!(back.stdout, "hello\n");
    assert_eq!(back.errors.len(), 1);
    assert_eq!(
      back.error.as_ref().and_then(|e| e.diff.clone()).as_deref(),
      Some("- a\n+ b")
    );
    assert_eq!(
      back.error.as_ref().and_then(|e| e.screenshot.clone()),
      Some(vec![1, 2, 3, 4])
    );
    assert_eq!(back.steps.len(), 1);
    assert_eq!(back.steps[0].steps.len(), 1);
    assert_eq!(back.steps[0].steps[0].title, "inner");
    assert_eq!(back.steps[0].status, StepStatus::Failed);
    assert_eq!(back.attachments.len(), 1);
    assert!(matches!(&back.attachments[0].body, AttachmentBody::Bytes(b) if b == &[9, 9, 9]));
    assert_eq!(back.annotations.len(), 1);

    std::fs::remove_dir_all(&dir).ok();
  }
}
