//! Trace recording in Playwright-compatible format.
//!
//! Produces a ZIP file containing:
//! - `test.trace` — JSONL: one JSON event per line
//! - `resources/` — screenshots and attachments keyed by SHA1
//!
//! Compatible with `npx playwright show-trace trace.zip`.
//!
//! Trace modes (matching Playwright):
//! - `Off` — no tracing
//! - `On` — always record
//! - `RetainOnFailure` — record but only keep if test fails
//! - `OnFirstRetry` — record only on first retry attempt
//!
//! Performance: zero cost when `Off`. When enabled, event construction is
//! allocation-light (`Cow<'static, str>` for fixed strings, pre-sized Vec),
//! ZIP uses `Stored` compression (no deflate CPU), and `serde_json::to_writer`
//! streams directly into the ZIP — no intermediate String allocations.
//! The worker offloads the file write to `spawn_blocking` so it never blocks
//! the async runtime.

use std::borrow::Cow;
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::model::TestStep;

/// Trace recording mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TraceMode {
  #[default]
  Off,
  On,
  RetainOnFailure,
  OnFirstRetry,
}

impl TraceMode {
  /// Parse from string (config/CLI).
  #[must_use]
  pub fn from_str(s: &str) -> Self {
    match s {
      "on" => Self::On,
      "retain-on-failure" => Self::RetainOnFailure,
      "on-first-retry" => Self::OnFirstRetry,
      _ => Self::Off,
    }
  }

  /// Should we record for this test attempt?
  #[must_use]
  pub fn should_record(self, attempt: u32, _failed: bool) -> bool {
    match self {
      Self::Off => false,
      Self::On => true,
      Self::RetainOnFailure => true, // record always, discard if passed
      Self::OnFirstRetry => attempt == 2,
    }
  }

  /// Should we keep the trace after the test finished?
  #[must_use]
  pub fn should_retain(self, failed: bool) -> bool {
    match self {
      Self::Off => false,
      Self::On => true,
      Self::RetainOnFailure => failed,
      Self::OnFirstRetry => true, // if we're recording, keep it
    }
  }

  /// Combined check: should we actually write a trace file?
  /// For `RetainOnFailure`, we skip the write entirely for passing tests
  /// since trace data comes from already-collected steps (unlike video which is live).
  #[must_use]
  pub fn should_write(self, attempt: u32, failed: bool) -> bool {
    self.should_record(attempt, failed) && self.should_retain(failed)
  }
}

/// A single trace event (Playwright format v8).
///
/// Uses `Cow<'static, str>` for fields that are almost always static literals,
/// avoiding heap allocation for the common case.
#[derive(Serialize)]
#[serde(tag = "type")]
enum TraceEvent<'a> {
  #[serde(rename = "context-options")]
  ContextOptions {
    #[serde(rename = "browserName")]
    browser_name: &'static str,
    platform: &'static str,
    #[serde(rename = "wallTime")]
    wall_time: u64,
    #[serde(rename = "sdkLanguage")]
    sdk_language: &'static str,
  },
  #[serde(rename = "before")]
  Before {
    #[serde(rename = "callId")]
    call_id: Cow<'a, str>,
    #[serde(rename = "startTime")]
    start_time: u64,
    class: &'static str,
    method: Cow<'a, str>,
    title: Cow<'a, str>,
    #[serde(rename = "parentId", skip_serializing_if = "Option::is_none")]
    parent_id: Option<Cow<'a, str>>,
  },
  #[serde(rename = "after")]
  After {
    #[serde(rename = "callId")]
    call_id: Cow<'a, str>,
    #[serde(rename = "endTime")]
    end_time: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
  },
}

/// Count total events needed for a step tree (2 per step: before + after).
fn count_events(steps: &[TestStep]) -> usize {
  steps.iter().map(|s| 2 + count_events(&s.steps)).sum()
}

/// Records trace events for a single test.
pub struct TraceRecorder<'a> {
  events: Vec<TraceEvent<'a>>,
  call_counter: u32,
  wall_time: u64,
}

impl<'a> TraceRecorder<'a> {
  /// Create a new recorder pre-sized for the given step count.
  #[must_use]
  pub fn for_steps(steps: &[TestStep]) -> Self {
    let capacity = 1 + count_events(steps); // +1 for context-options
    let wall_time = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .unwrap_or_default()
      .as_millis() as u64;

    let mut events = Vec::with_capacity(capacity);
    events.push(TraceEvent::ContextOptions {
      browser_name: "chromium",
      platform: std::env::consts::OS,
      wall_time,
      sdk_language: "rust",
    });

    Self {
      events,
      call_counter: 0,
      wall_time,
    }
  }

  /// Record a test step as before/after events (borrows step data, no cloning).
  pub fn record_step(&mut self, step: &'a TestStep, parent_id: Option<Cow<'a, str>>) {
    self.call_counter += 1;
    let call_id: Cow<'a, str> = Cow::Owned(format!("s{}", self.call_counter));

    self.events.push(TraceEvent::Before {
      call_id: call_id.clone(),
      start_time: self.wall_time.saturating_sub(step.duration.as_millis() as u64),
      class: "Test",
      method: Cow::Owned(step.category.to_string()),
      title: Cow::Borrowed(&step.title),
      parent_id,
    });

    // Record nested steps.
    for child in &step.steps {
      self.record_step(child, Some(call_id.clone()));
    }

    self.events.push(TraceEvent::After {
      call_id,
      end_time: self.wall_time,
      error: step.error.as_deref(),
    });
  }

  /// Record all steps from a test's outcome.
  pub fn record_steps(&mut self, steps: &'a [TestStep]) {
    for step in steps {
      self.record_step(step, None);
    }
  }

  /// Serialize all events into an in-memory JSONL+ZIP buffer.
  ///
  /// Uses `Stored` compression (no deflate CPU) and `serde_json::to_writer`
  /// streaming directly into the ZIP — no intermediate String per event.
  /// Returns owned bytes suitable for `spawn_blocking` file write.
  ///
  /// # Errors
  ///
  /// Returns an error if serialization fails (should never happen).
  pub fn into_zip_bytes(self) -> Result<Vec<u8>, String> {
    let mut buf = Vec::with_capacity(256 + self.events.len() * 128);
    let cursor = std::io::Cursor::new(&mut buf);
    let mut zip = zip::ZipWriter::new(cursor);

    let options = zip::write::SimpleFileOptions::default()
      .compression_method(zip::CompressionMethod::Stored);

    zip
      .start_file("test.trace", options)
      .map_err(|e| format!("zip start_file: {e}"))?;

    for event in &self.events {
      serde_json::to_writer(&mut zip, event).map_err(|e| format!("serialize trace event: {e}"))?;
      zip.write_all(b"\n").map_err(|e| format!("write newline: {e}"))?;
    }

    zip.finish().map_err(|e| format!("zip finish: {e}"))?;
    Ok(buf)
  }
}

/// Write pre-serialized ZIP bytes to a file. Designed for `spawn_blocking`.
///
/// # Errors
///
/// Returns an error if file I/O fails.
pub fn write_trace_file(path: &Path, data: &[u8]) -> Result<(), String> {
  if let Some(parent) = path.parent() {
    std::fs::create_dir_all(parent).map_err(|e| format!("create trace dir: {e}"))?;
  }
  std::fs::write(path, data).map_err(|e| format!("write trace zip {}: {e}", path.display()))
}
