//! Trace recording in Playwright's trace format VERSION 8.
//!
//! Produces a ZIP file containing:
//! - `trace.trace` — JSONL: first line is a `context-options` event with
//!   `version: 8` (the loader assumes v6 otherwise and mis-modernizes
//!   everything, `traceModernizer.ts`), followed by one merged `action`
//!   event per test step (before+after fields in one line — immune to the
//!   loader's orphaned-`after` crash).
//! - `trace.network` — JSONL of `resource-snapshot` events (empty for
//!   step traces; the runner does not capture network bodies here).
//!
//! Canonical event shapes: `/tmp/playwright/packages/isomorphic/trace/versions/traceV8.ts`.
//! The library-side `context.tracing` recorder lives in
//! `crates/ferridriver/src/trace.rs`; this recorder reconstructs a trace
//! from a finished test's step tree instead of live protocol traffic, so
//! `npx playwright show-trace` / trace.playwright.dev open runner traces
//! directly.
//!
//! Step times: `TestStep` carries only a duration, so the recorder lays
//! steps out sequentially from a zero origin — each step starts where its
//! predecessor ended, children nest inside their parent's span.
//!
//! Trace modes (matching Playwright):
//! - `Off` — no tracing
//! - `On` — always record
//! - `RetainOnFailure` — record but only keep if test fails
//! - `OnFirstRetry` — record only on first retry attempt

use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub use ferridriver_config::test::TraceMode;

use crate::model::TestStep;

/// Trace format version this recorder emits.
const TRACE_VERSION: u32 = 8;

/// Count total steps in a step tree (one merged `action` line per step).
fn count_steps(steps: &[TestStep]) -> usize {
  steps.iter().map(|s| 1 + count_steps(&s.steps)).sum()
}

/// Records a finished test's step tree as Playwright v8 trace lines.
pub struct TraceRecorder {
  /// Serialized JSONL lines; index 0 is the `context-options` event.
  lines: Vec<String>,
  call_counter: u32,
}

impl TraceRecorder {
  /// Create a new recorder pre-sized for the given step count.
  #[must_use]
  pub fn for_steps(browser_name: &str, steps: &[TestStep]) -> Self {
    let capacity = 1 + count_steps(steps);
    let wall_time = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .unwrap_or_default()
      .as_millis() as u64;

    let mut lines = Vec::with_capacity(capacity);
    lines.push(
      serde_json::json!({
        "version": TRACE_VERSION,
        "type": "context-options",
        "origin": "testRunner",
        "browserName": browser_name,
        "platform": std::env::consts::OS,
        "wallTime": wall_time,
        "monotonicTime": 0.0,
        "title": "",
        "options": {},
        "sdkLanguage": "javascript",
      })
      .to_string(),
    );

    Self { lines, call_counter: 0 }
  }

  /// Record one step (and its children) as merged `action` events.
  /// Returns the step's end time on the reconstructed timeline.
  fn record_step(&mut self, step: &TestStep, parent_id: Option<&str>, start_time: f64) -> f64 {
    self.call_counter += 1;
    let call_id = format!("call@{}", self.call_counter);
    let end_time = start_time + step.duration.as_secs_f64() * 1000.0;

    let mut event = serde_json::json!({
      "type": "action",
      "callId": call_id,
      "startTime": start_time,
      "endTime": end_time,
      "class": "Test",
      "method": step.category.to_string(),
      "title": step.title,
      "params": {},
    });
    if let Some(parent) = parent_id {
      event["parentId"] = serde_json::Value::String(parent.to_string());
    }
    if let Some(ref error) = step.error {
      event["error"] = serde_json::json!({ "name": "Error", "message": error });
    }
    if let Some(frame) = step.location.as_deref().and_then(location_to_stack_frame) {
      event["stack"] = serde_json::json!([frame]);
    }
    self.lines.push(event.to_string());

    let mut cursor = start_time;
    for child in &step.steps {
      cursor = self.record_step(child, Some(&call_id), cursor);
    }

    end_time
  }

  /// Record all steps from a test's outcome, laid out sequentially.
  pub fn record_steps(&mut self, steps: &[TestStep]) {
    let mut cursor = 0.0;
    for step in steps {
      cursor = self.record_step(step, None, cursor);
    }
  }

  /// Serialize all events into an in-memory JSONL+ZIP buffer.
  ///
  /// Uses `Stored` compression (no deflate CPU). Returns owned bytes
  /// suitable for a `spawn_blocking` file write.
  ///
  /// # Errors
  ///
  /// Returns an error if the ZIP write fails (should never happen for an
  /// in-memory cursor).
  pub fn into_zip_bytes(self) -> Result<Vec<u8>, String> {
    let mut buf = Vec::with_capacity(256 + self.lines.iter().map(String::len).sum::<usize>());
    let cursor = std::io::Cursor::new(&mut buf);
    let mut zip = zip::ZipWriter::new(cursor);

    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

    zip
      .start_file("trace.trace", options)
      .map_err(|e| format!("zip start_file: {e}"))?;
    zip
      .write_all(self.lines.join("\n").as_bytes())
      .map_err(|e| format!("write trace lines: {e}"))?;

    zip
      .start_file("trace.network", options)
      .map_err(|e| format!("zip start_file: {e}"))?;

    zip.finish().map_err(|e| format!("zip finish: {e}"))?;
    Ok(buf)
  }
}

/// Parse a `"path/to/file.feature:12"` location into a v8 stack frame.
fn location_to_stack_frame(location: &str) -> Option<serde_json::Value> {
  let (file, line) = location.rsplit_once(':')?;
  let line: u64 = line.parse().ok()?;
  if file.is_empty() {
    return None;
  }
  Some(serde_json::json!({ "file": file, "line": line, "column": 0 }))
}

/// Write pre-serialized ZIP bytes to a file. Designed for `spawn_blocking`.
///
/// # Errors
///
/// Returns an error if file I/O fails.
pub fn write_trace_file(path: &Path, data: &[u8]) -> ferridriver::error::Result<()> {
  if let Some(parent) = path.parent() {
    std::fs::create_dir_all(parent)?;
  }
  std::fs::write(path, data)?;
  Ok(())
}

#[cfg(test)]
mod tests {
  use std::time::Duration;

  use super::*;
  use crate::model::{StepCategory, StepStatus};

  fn step(title: &str, duration_ms: u64, error: Option<&str>, children: Vec<TestStep>) -> TestStep {
    TestStep {
      step_id: format!("step-{title}"),
      title: title.to_string(),
      category: StepCategory::TestStep,
      duration: Duration::from_millis(duration_ms),
      status: if error.is_some() {
        StepStatus::Failed
      } else {
        StepStatus::Passed
      },
      error: error.map(String::from),
      location: Some("features/smoke.feature:4".into()),
      parent_step_id: None,
      metadata: None,
      steps: children,
    }
  }

  fn trace_lines(recorder: TraceRecorder) -> Vec<serde_json::Value> {
    let bytes = recorder.into_zip_bytes().expect("zip");
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("archive");
    let mut trace = String::new();
    std::io::Read::read_to_string(&mut archive.by_name("trace.trace").expect("trace.trace"), &mut trace).expect("read");
    trace
      .lines()
      .map(|l| serde_json::from_str(l).expect("valid json line"))
      .collect()
  }

  #[test]
  fn first_line_is_context_options_version_8() {
    let recorder = TraceRecorder::for_steps("chromium", &[]);
    let lines = trace_lines(recorder);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["version"].as_u64(), Some(8));
    assert_eq!(lines[0]["type"].as_str(), Some("context-options"));
    assert_eq!(lines[0]["origin"].as_str(), Some("testRunner"));
    assert_eq!(lines[0]["browserName"].as_str(), Some("chromium"));
    assert_eq!(lines[0]["monotonicTime"].as_f64(), Some(0.0));
  }

  #[test]
  fn steps_become_merged_action_events_with_nesting() {
    let steps = vec![step(
      "Given a page",
      30,
      None,
      vec![step("goto", 10, None, vec![]), step("wait", 20, None, vec![])],
    )];
    let mut recorder = TraceRecorder::for_steps("firefox", &steps);
    recorder.record_steps(&steps);
    let lines = trace_lines(recorder);

    assert_eq!(lines.len(), 4);
    let parent = &lines[1];
    assert_eq!(parent["type"].as_str(), Some("action"));
    assert_eq!(parent["callId"].as_str(), Some("call@1"));
    assert_eq!(parent["title"].as_str(), Some("Given a page"));
    assert!(parent.get("parentId").is_none());
    assert_eq!(parent["startTime"].as_f64(), Some(0.0));
    assert_eq!(parent["endTime"].as_f64(), Some(30.0));
    assert_eq!(parent["stack"][0]["file"].as_str(), Some("features/smoke.feature"));
    assert_eq!(parent["stack"][0]["line"].as_u64(), Some(4));

    let first_child = &lines[2];
    assert_eq!(first_child["parentId"].as_str(), Some("call@1"));
    assert_eq!(first_child["startTime"].as_f64(), Some(0.0));
    assert_eq!(first_child["endTime"].as_f64(), Some(10.0));

    let second_child = &lines[3];
    assert_eq!(second_child["parentId"].as_str(), Some("call@1"));
    assert_eq!(second_child["startTime"].as_f64(), Some(10.0));
    assert_eq!(second_child["endTime"].as_f64(), Some(30.0));
  }

  #[test]
  fn sibling_steps_are_laid_out_sequentially() {
    let steps = vec![
      step("first", 15, None, vec![]),
      step("second", 25, Some("boom"), vec![]),
    ];
    let mut recorder = TraceRecorder::for_steps("webkit", &steps);
    recorder.record_steps(&steps);
    let lines = trace_lines(recorder);

    assert_eq!(lines[1]["startTime"].as_f64(), Some(0.0));
    assert_eq!(lines[1]["endTime"].as_f64(), Some(15.0));
    assert_eq!(lines[2]["startTime"].as_f64(), Some(15.0));
    assert_eq!(lines[2]["endTime"].as_f64(), Some(40.0));
    assert_eq!(lines[2]["error"]["name"].as_str(), Some("Error"));
    assert_eq!(lines[2]["error"]["message"].as_str(), Some("boom"));
  }

  #[test]
  fn zip_contains_trace_and_network_entries() {
    let recorder = TraceRecorder::for_steps("chromium", &[]);
    let bytes = recorder.into_zip_bytes().expect("zip");
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("archive");
    let names: Vec<String> = (0..archive.len())
      .map(|i| archive.by_index(i).expect("entry").name().to_string())
      .collect();
    assert!(names.contains(&"trace.trace".to_string()));
    assert!(names.contains(&"trace.network".to_string()));
  }
}
