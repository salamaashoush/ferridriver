//! Reporter system: event-driven, multiplexed, trait-based.

pub mod allure;
pub mod bdd;
pub mod html;
pub mod json;
pub mod junit;
pub mod progress;
pub mod rerun;
pub mod terminal;

use std::time::Duration;

use tokio::sync::mpsc;

use crate::model::{StepCategory, TestId, TestOutcome};

// ── Events ──

#[derive(Debug, Clone)]
pub struct StepStartedEvent {
  pub test_id: TestId,
  pub step_id: String,
  pub parent_step_id: Option<String>,
  pub title: String,
  pub category: StepCategory,
}

#[derive(Debug, Clone)]
pub struct StepFinishedEvent {
  pub test_id: TestId,
  pub step_id: String,
  pub title: String,
  pub category: StepCategory,
  pub duration: Duration,
  pub error: Option<String>,
  /// Arbitrary metadata attached to this step (e.g. BDD keyword/text).
  pub metadata: Option<serde_json::Value>,
}

/// Events emitted during a test run.
#[derive(Debug, Clone)]
pub enum ReporterEvent {
  /// The entire run is starting.
  RunStarted { total_tests: usize, num_workers: u32 },
  /// A worker has been spawned.
  WorkerStarted { worker_id: u32 },
  /// A test is about to execute.
  TestStarted { test_id: TestId, attempt: u32 },
  /// A step within a test has started (real-time, emitted during execution).
  StepStarted(Box<StepStartedEvent>),
  /// A step within a test has finished (real-time, emitted during execution).
  StepFinished(Box<StepFinishedEvent>),
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

  /// Append an additional reporter (e.g., NAPI ResultCollector).
  pub fn add(&mut self, reporter: Box<dyn Reporter>) {
    self.reporters.push(reporter);
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

/// Unified reporter factory. Creates reporters from config names, routing
/// mode-dependent reporters (terminal, json, junit) based on `mode`.
pub(crate) fn create_reporters(
  names: &[crate::config::ReporterConfig],
  output_dir: &std::path::Path,
  mode: crate::config::RunMode,
) -> ReporterSet {
  let mut reporters: Vec<Box<dyn Reporter>> = Vec::new();
  let mut has_terminal = false;

  for config in names {
    match config.name.as_str() {
      // ── Mode-dependent reporters ──
      "terminal" | "list" | "bdd" | "default" | "" => {
        if !has_terminal {
          match mode {
            crate::config::RunMode::E2e => reporters.push(Box::new(terminal::TerminalReporter::new())),
            crate::config::RunMode::Bdd => reporters.push(Box::new(bdd::terminal::BddTerminalReporter::new())),
          }
          has_terminal = true;
        }
      }
      "json" => match mode {
        crate::config::RunMode::E2e => {
          reporters.push(Box::new(json::JsonReporter::new(output_dir.join("results.json"))));
        }
        crate::config::RunMode::Bdd => {
          reporters.push(Box::new(bdd::json::BddJsonReporter::new(output_dir.join("bdd-results.json"))));
        }
      },
      "junit" => match mode {
        crate::config::RunMode::E2e => {
          reporters.push(Box::new(junit::JUnitReporter::new(output_dir.join("junit.xml"))));
        }
        crate::config::RunMode::Bdd => {
          reporters.push(Box::new(bdd::junit::BddJunitReporter::new(output_dir.join("bdd-junit.xml"))));
        }
      },

      // ── Shared reporters (same for both modes) ──
      "html" => {
        reporters.push(Box::new(html::HtmlReporter::new(output_dir.join("report.html"))));
      }
      "allure" => {
        let dir = config
          .options
          .get("output_dir")
          .and_then(|v| v.as_str())
          .map(std::path::PathBuf::from)
          .unwrap_or_else(|| output_dir.join("allure-results"));
        let mut reporter = allure::AllureReporter::new(dir);
        if let Some(title) = config.options.get("suite_title").and_then(|v| v.as_str()) {
          reporter = reporter.with_suite_title(title.to_string());
        }
        reporters.push(Box::new(reporter));
      }
      "progress" => {
        reporters.push(Box::new(progress::ProgressReporter::new()));
      }
      "rerun" => {
        reporters.push(Box::new(rerun::RerunReporter::new(output_dir.join("@rerun.txt"))));
      }

      // ── BDD-specific reporters (usable in any mode) ──
      "cucumber-json" | "cucumber" => {
        reporters.push(Box::new(bdd::cucumber_json::CucumberJsonReporter::new(
          output_dir.join("cucumber.json"),
        )));
      }
      "messages" | "ndjson" => {
        reporters.push(Box::new(bdd::messages::CucumberMessagesReporter::new(
          output_dir.join("cucumber-messages.ndjson"),
        )));
      }
      "usage" => {
        reporters.push(Box::new(bdd::usage::UsageReporter::new()));
      }

      other => {
        tracing::warn!("unknown reporter: {other}, skipping");
      }
    }
  }

  if reporters.is_empty() {
    match mode {
      crate::config::RunMode::E2e => reporters.push(Box::new(terminal::TerminalReporter::new())),
      crate::config::RunMode::Bdd => reporters.push(Box::new(bdd::terminal::BddTerminalReporter::new())),
    }
  }

  // Always add the rerun reporter so @rerun.txt is available for --last-failed.
  let has_rerun = names.iter().any(|c| c.name == "rerun");
  if !has_rerun {
    reporters.push(Box::new(rerun::RerunReporter::new(output_dir.join("@rerun.txt"))));
  }

  ReporterSet::new(reporters)
}
