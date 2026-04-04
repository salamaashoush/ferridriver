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

use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::model::{TestId, TestStep};

/// Trace recording mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
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
  pub fn should_record(self, attempt: u32, failed: bool) -> bool {
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
}

/// A single trace event (Playwright format v8).
#[derive(Serialize)]
#[serde(tag = "type")]
enum TraceEvent {
  #[serde(rename = "context-options")]
  ContextOptions {
    #[serde(rename = "browserName")]
    browser_name: String,
    platform: String,
    #[serde(rename = "wallTime")]
    wall_time: u64,
    #[serde(rename = "sdkLanguage")]
    sdk_language: String,
  },
  #[serde(rename = "before")]
  Before {
    #[serde(rename = "callId")]
    call_id: String,
    #[serde(rename = "startTime")]
    start_time: u64,
    class: String,
    method: String,
    title: String,
    #[serde(rename = "parentId", skip_serializing_if = "Option::is_none")]
    parent_id: Option<String>,
  },
  #[serde(rename = "after")]
  After {
    #[serde(rename = "callId")]
    call_id: String,
    #[serde(rename = "endTime")]
    end_time: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
  },
}

/// Records trace events for a single test.
pub struct TraceRecorder {
  events: Vec<TraceEvent>,
  call_counter: u32,
}

impl TraceRecorder {
  /// Create a new recorder, emitting the initial context-options event.
  #[must_use]
  pub fn new() -> Self {
    let wall_time = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .unwrap_or_default()
      .as_millis() as u64;

    let mut recorder = Self {
      events: Vec::new(),
      call_counter: 0,
    };

    recorder.events.push(TraceEvent::ContextOptions {
      browser_name: "chromium".into(),
      platform: std::env::consts::OS.into(),
      wall_time,
      sdk_language: "rust".into(),
    });

    recorder
  }

  /// Record a test step as before/after events.
  pub fn record_step(&mut self, step: &TestStep, parent_id: Option<&str>) {
    self.call_counter += 1;
    let call_id = format!("s{}", self.call_counter);

    let now = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .unwrap_or_default()
      .as_millis() as u64;

    self.events.push(TraceEvent::Before {
      call_id: call_id.clone(),
      start_time: now.saturating_sub(step.duration.as_millis() as u64),
      class: "Test".into(),
      method: step.category.to_string(),
      title: step.title.clone(),
      parent_id: parent_id.map(String::from),
    });

    // Record nested steps.
    for child in &step.steps {
      self.record_step(child, Some(&call_id));
    }

    self.events.push(TraceEvent::After {
      call_id,
      end_time: now,
      error: step.error.clone(),
    });
  }

  /// Record all steps from a test's outcome.
  pub fn record_steps(&mut self, steps: &[TestStep]) {
    for step in steps {
      self.record_step(step, None);
    }
  }

  /// Bundle the trace into a ZIP file at the given path.
  ///
  /// # Errors
  ///
  /// Returns an error if file I/O fails.
  pub fn write_zip(&self, path: &PathBuf) -> Result<(), String> {
    if let Some(parent) = path.parent() {
      std::fs::create_dir_all(parent).map_err(|e| format!("create trace dir: {e}"))?;
    }

    let file =
      std::fs::File::create(path).map_err(|e| format!("create trace zip {}: {e}", path.display()))?;
    let mut zip = zip::ZipWriter::new(file);

    let options = zip::write::SimpleFileOptions::default()
      .compression_method(zip::CompressionMethod::Deflated);

    // Write test.trace (JSONL).
    zip
      .start_file("test.trace", options)
      .map_err(|e| format!("zip start_file: {e}"))?;

    for event in &self.events {
      let json = serde_json::to_string(event).map_err(|e| format!("serialize trace event: {e}"))?;
      zip
        .write_all(json.as_bytes())
        .map_err(|e| format!("write trace event: {e}"))?;
      zip.write_all(b"\n").map_err(|e| format!("write newline: {e}"))?;
    }

    zip.finish().map_err(|e| format!("zip finish: {e}"))?;
    Ok(())
  }
}

impl Default for TraceRecorder {
  fn default() -> Self {
    Self::new()
  }
}
