//! `blob` reporter — emits a `report.zip` containing every
//! `ReporterEvent` as a JSON-lines stream. Mirrors Playwright's
//! `/tmp/playwright/packages/playwright/src/reporters/blob.ts`.
//!
//! The merge subcommand (`ferridriver-test merge-reports <dir>`)
//! reads every blob in a directory, replays the merged event stream
//! through the configured reporter, and produces a unified report.

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{Reporter, ReporterEvent, StepFinishedEvent, StepStartedEvent};
use crate::model::{StepCategory, TestId, TestOutcome, TestStatus};

const SCHEMA_VERSION: u32 = 1;

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
  },
  WorkerStarted {
    worker_id: u32,
  },
  TestStarted {
    test_id: WireTestId,
    attempt: u32,
  },
  StepStarted {
    test_id: WireTestId,
    step_id: String,
    parent_step_id: Option<String>,
    title: String,
    category: String,
  },
  StepFinished {
    test_id: WireTestId,
    step_id: String,
    title: String,
    category: String,
    duration_ms: u64,
    error: Option<String>,
    metadata: Option<serde_json::Value>,
  },
  TestFinished {
    test_id: WireTestId,
    status: String,
    duration_ms: u64,
    attempt: u32,
    error: Option<String>,
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
  },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireTestId {
  pub file: String,
  pub suite: Option<String>,
  pub name: String,
  pub line: Option<usize>,
}

impl From<&TestId> for WireTestId {
  fn from(id: &TestId) -> Self {
    Self {
      file: id.file.clone(),
      suite: id.suite.clone(),
      name: id.name.clone(),
      line: id.line,
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
    }
  }
}

fn step_category_str(c: StepCategory) -> &'static str {
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

fn status_str(s: TestStatus) -> &'static str {
  match s {
    TestStatus::Passed => "passed",
    TestStatus::Failed => "failed",
    TestStatus::TimedOut => "timed-out",
    TestStatus::Skipped => "skipped",
    TestStatus::Flaky => "flaky",
    TestStatus::Interrupted => "interrupted",
  }
}

fn parse_status(s: &str) -> TestStatus {
  match s {
    "failed" => TestStatus::Failed,
    "timed-out" => TestStatus::TimedOut,
    "skipped" => TestStatus::Skipped,
    "flaky" => TestStatus::Flaky,
    "interrupted" => TestStatus::Interrupted,
    _ => TestStatus::Passed,
  }
}

impl WireEvent {
  pub fn from_runtime(event: &ReporterEvent) -> Option<Self> {
    Some(match event {
      ReporterEvent::RunStarted {
        total_tests,
        num_workers,
        metadata,
      } => Self::RunStarted {
        total_tests: *total_tests,
        num_workers: *num_workers,
        metadata: metadata.clone(),
      },
      ReporterEvent::WorkerStarted { worker_id } => Self::WorkerStarted { worker_id: *worker_id },
      ReporterEvent::TestStarted { test_id, attempt } => Self::TestStarted {
        test_id: test_id.into(),
        attempt: *attempt,
      },
      ReporterEvent::StepStarted(s) => Self::StepStarted {
        test_id: (&s.test_id).into(),
        step_id: s.step_id.clone(),
        parent_step_id: s.parent_step_id.clone(),
        title: s.title.clone(),
        category: step_category_str(s.category.clone()).to_string(),
      },
      ReporterEvent::StepFinished(s) => Self::StepFinished {
        test_id: (&s.test_id).into(),
        step_id: s.step_id.clone(),
        title: s.title.clone(),
        category: step_category_str(s.category.clone()).to_string(),
        duration_ms: s.duration.as_millis() as u64,
        error: s.error.clone(),
        metadata: s.metadata.clone(),
      },
      ReporterEvent::TestFinished { test_id, outcome } => Self::TestFinished {
        test_id: test_id.into(),
        status: status_str(outcome.status.clone()).to_string(),
        duration_ms: outcome.duration.as_millis() as u64,
        attempt: outcome.attempt,
        error: outcome.error.as_ref().map(|e| e.message.clone()),
      },
      ReporterEvent::WorkerFinished { worker_id } => Self::WorkerFinished { worker_id: *worker_id },
      ReporterEvent::RunFinished {
        total,
        passed,
        failed,
        skipped,
        flaky,
        duration,
      } => Self::RunFinished {
        total: *total,
        passed: *passed,
        failed: *failed,
        skipped: *skipped,
        flaky: *flaky,
        duration_ms: duration.as_millis() as u64,
      },
    })
  }

  /// Lower a wire event back into the runtime variant. Header
  /// frames return `None` since they're metadata, not test events.
  pub fn into_runtime(self) -> Option<ReporterEvent> {
    Some(match self {
      Self::Header { .. } => return None,
      Self::RunStarted {
        total_tests,
        num_workers,
        metadata,
      } => ReporterEvent::RunStarted {
        total_tests,
        num_workers,
        metadata,
      },
      Self::WorkerStarted { worker_id } => ReporterEvent::WorkerStarted { worker_id },
      Self::TestStarted { test_id, attempt } => ReporterEvent::TestStarted {
        test_id: test_id.into(),
        attempt,
      },
      Self::StepStarted {
        test_id,
        step_id,
        parent_step_id,
        title,
        category,
      } => ReporterEvent::StepStarted(Box::new(StepStartedEvent {
        test_id: test_id.into(),
        step_id,
        parent_step_id,
        title,
        category: parse_step_category(&category),
      })),
      Self::StepFinished {
        test_id,
        step_id,
        title,
        category,
        duration_ms,
        error,
        metadata,
      } => ReporterEvent::StepFinished(Box::new(StepFinishedEvent {
        test_id: test_id.into(),
        step_id,
        title,
        category: parse_step_category(&category),
        duration: Duration::from_millis(duration_ms),
        error,
        metadata,
      })),
      Self::TestFinished {
        test_id,
        status,
        duration_ms,
        attempt,
        error,
      } => {
        let status = parse_status(&status);
        let id: TestId = test_id.into();
        ReporterEvent::TestFinished {
          test_id: id.clone(),
          outcome: TestOutcome {
            test_id: id,
            status,
            duration: Duration::from_millis(duration_ms),
            attempt,
            max_attempts: 1,
            error: error.map(|message| crate::model::TestFailure {
              message,
              stack: None,
              diff: None,
              screenshot: None,
            }),
            attachments: Vec::new(),
            steps: Vec::new(),
            stdout: String::new(),
            stderr: String::new(),
            annotations: Vec::new(),
            metadata: serde_json::Value::Null,
          },
        }
      },
      Self::WorkerFinished { worker_id } => ReporterEvent::WorkerFinished { worker_id },
      Self::RunFinished {
        total,
        passed,
        failed,
        skipped,
        flaky,
        duration_ms,
      } => ReporterEvent::RunFinished {
        total,
        passed,
        failed,
        skipped,
        flaky,
        duration: Duration::from_millis(duration_ms),
      },
    })
  }
}

/// `--reporter blob` writes one `report-<shard>.zip` per run; each
/// zip contains a single `events.jsonl` member. The merge subcommand
/// reads every zip in a directory, concats the streams, and replays
/// them through the configured reporter.
pub struct BlobReporter {
  out_path: PathBuf,
  buffer: Vec<u8>,
  shard_index: Option<u32>,
  shard_total: Option<u32>,
}

impl BlobReporter {
  /// Construct a blob reporter that writes to `out_path` on
  /// `finalize()`. Shard metadata (if known) is recorded in the
  /// header frame so the merger can preserve the run boundary.
  #[must_use]
  pub fn new(out_path: PathBuf) -> Self {
    let mut buffer = Vec::new();
    write_event(
      &mut buffer,
      &WireEvent::Header {
        schema: SCHEMA_VERSION,
        shard_index: None,
        shard_total: None,
      },
    );
    Self {
      out_path,
      buffer,
      shard_index: None,
      shard_total: None,
    }
  }

  pub fn with_shard(mut self, current: u32, total: u32) -> Self {
    self.shard_index = Some(current);
    self.shard_total = Some(total);
    // Rewrite the header now that we know the shard.
    self.buffer.clear();
    write_event(
      &mut self.buffer,
      &WireEvent::Header {
        schema: SCHEMA_VERSION,
        shard_index: self.shard_index,
        shard_total: self.shard_total,
      },
    );
    self
  }
}

#[async_trait]
impl Reporter for BlobReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    if let Some(wire) = WireEvent::from_runtime(event) {
      write_event(&mut self.buffer, &wire);
    }
  }

  async fn finalize(&mut self) -> ferridriver::error::Result<()> {
    use ferridriver::FerriError;
    if let Some(parent) = self.out_path.parent() {
      std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(&self.out_path)?;
    let mut zip = zip::ZipWriter::new(file);
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
  if let Ok(line) = serde_json::to_string(event) {
    buffer.extend_from_slice(line.as_bytes());
    buffer.push(b'\n');
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
    let file = std::fs::File::open(&path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("zip read {}: {e}", path.display()))?;
    let mut events_file = zip
      .by_name("events.jsonl")
      .map_err(|e| format!("missing events.jsonl in {}: {e}", path.display()))?;
    let mut buf = String::new();
    use std::io::Read;
    events_file
      .read_to_string(&mut buf)
      .map_err(|e| format!("read jsonl: {e}"))?;
    for (i, line) in buf.lines().enumerate() {
      if line.trim().is_empty() {
        continue;
      }
      let wire: WireEvent =
        serde_json::from_str(line).map_err(|e| format!("parse line {i} in {}: {e}", path.display()))?;
      if let Some(event) = wire.into_runtime() {
        events.push(event);
      }
    }
  }
  Ok(events)
}
