//! Reporter system: event-driven, multiplexed, trait-based.

pub mod html;
pub mod json;
pub mod junit;
pub mod terminal;

use std::time::Duration;

use tokio::sync::mpsc;

use crate::model::{TestId, TestOutcome};

// ── Events ──

/// Events emitted during a test run.
#[derive(Debug, Clone)]
pub enum ReporterEvent {
  /// The entire run is starting.
  RunStarted { total_tests: usize, num_workers: u32 },
  /// A worker has been spawned.
  WorkerStarted { worker_id: u32 },
  /// A test is about to execute.
  TestStarted { test_id: TestId, attempt: u32 },
  /// A test finished (pass, fail, skip, etc.).
  TestFinished { test_id: TestId, outcome: TestOutcome },
  /// A worker has shut down.
  WorkerFinished { worker_id: u32 },
  /// The entire run completed.
  RunFinished {
    total: usize,
    passed: usize,
    failed: usize,
    skipped: usize,
    flaky: usize,
    duration: Duration,
  },
}

// ── Reporter Trait ──

/// Trait that all reporters implement.
#[async_trait::async_trait]
pub trait Reporter: Send + Sync {
  /// Called for every event.
  async fn on_event(&mut self, event: &ReporterEvent);

  /// Called after the run to finalize output (write files, close streams).
  async fn finalize(&mut self) -> Result<(), String> {
    Ok(())
  }
}

// ── Reporter Set (multiplexer) ──

/// Multiplexes events to multiple reporters.
pub struct ReporterSet {
  reporters: Vec<Box<dyn Reporter>>,
}

impl Default for ReporterSet {
  fn default() -> Self {
    Self {
      reporters: Vec::new(),
    }
  }
}

impl ReporterSet {
  pub fn new(reporters: Vec<Box<dyn Reporter>>) -> Self {
    Self { reporters }
  }

  pub async fn emit(&mut self, event: &ReporterEvent) {
    for reporter in &mut self.reporters {
      reporter.on_event(event).await;
    }
  }

  pub async fn finalize(&mut self) -> ReporterSet {
    for reporter in &mut self.reporters {
      if let Err(e) = reporter.finalize().await {
        tracing::error!("reporter finalize error: {e}");
      }
    }
    // Return empty set after finalization.
    Self::default()
  }
}

// ── Event Bus ──

/// Thread-safe broadcaster for reporter events.
/// Workers send events through this; the main thread fans out to reporters.
#[derive(Clone)]
pub struct EventBus {
  tx: mpsc::UnboundedSender<ReporterEvent>,
}

impl EventBus {
  pub fn new(tx: mpsc::UnboundedSender<ReporterEvent>) -> Self {
    Self { tx }
  }

  pub async fn emit(&self, event: ReporterEvent) {
    let _ = self.tx.send(event);
  }
}

// ── Factory ──

/// Create reporters from config names.
pub fn create_reporters(names: &[crate::config::ReporterConfig], output_dir: &std::path::Path) -> ReporterSet {
  let mut reporters: Vec<Box<dyn Reporter>> = Vec::new();

  for config in names {
    match config.name.as_str() {
      "terminal" | "list" => {
        reporters.push(Box::new(terminal::TerminalReporter::new()));
      }
      "json" => {
        let path = output_dir.join("results.json");
        reporters.push(Box::new(json::JsonReporter::new(path)));
      }
      "junit" => {
        let path = output_dir.join("junit.xml");
        reporters.push(Box::new(junit::JUnitReporter::new(path)));
      }
      "html" => {
        let path = output_dir.join("report.html");
        reporters.push(Box::new(html::HtmlReporter::new(path)));
      }
      other => {
        tracing::warn!("unknown reporter: {other}, skipping");
      }
    }
  }

  if reporters.is_empty() {
    reporters.push(Box::new(terminal::TerminalReporter::new()));
  }

  ReporterSet::new(reporters)
}
