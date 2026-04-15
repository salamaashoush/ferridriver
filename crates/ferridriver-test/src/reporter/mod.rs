//! Reporter system: event-driven, multiplexed, trait-based.

pub mod allure;
pub mod bdd;
pub mod html;
pub mod json;
pub mod junit;
pub mod progress;
pub mod rerun;
pub mod terminal;

use std::sync::Arc;
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
  RunStarted {
    total_tests: usize,
    num_workers: u32,
    /// Arbitrary metadata from config (Playwright's `metadata` field).
    metadata: serde_json::Value,
  },
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
    Self { reporters: Vec::new() }
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

  /// Replace all reporters with a new set.
  pub fn replace(&mut self, reporters: Vec<Box<dyn Reporter>>) {
    self.reporters = reporters;
  }

  pub async fn emit(&mut self, event: &ReporterEvent) {
    for reporter in &mut self.reporters {
      reporter.on_event(event).await;
    }
  }

  pub async fn finalize(&mut self) {
    for reporter in &mut self.reporters {
      if let Err(e) = reporter.finalize().await {
        tracing::error!("reporter finalize error: {e}");
      }
    }
  }
}

// ── Event Bus ──

/// Builder for constructing an `EventBus` with registered subscribers.
///
/// Register all subscribers before calling `build()`. Once built, the bus
/// is immutable — no new subscribers can be added. This ensures workers
/// (which clone the bus) fan out to a fixed set of consumers.
pub struct EventBusBuilder {
  subscribers: Vec<mpsc::UnboundedSender<ReporterEvent>>,
}

impl Default for EventBusBuilder {
  fn default() -> Self {
    Self::new()
  }
}

impl EventBusBuilder {
  pub fn new() -> Self {
    Self {
      subscribers: Vec::new(),
    }
  }

  /// Register a subscriber. Returns a `Subscription` (the receiving end).
  /// Must be called before `build()`.
  pub fn subscribe(&mut self) -> Subscription {
    let (tx, rx) = mpsc::unbounded_channel();
    self.subscribers.push(tx);
    Subscription { rx }
  }

  /// Finalize the bus. No more subscribers can be added after this.
  pub fn build(self) -> EventBus {
    EventBus {
      inner: Arc::new(EventBusInner {
        subscribers: std::sync::RwLock::new(self.subscribers),
      }),
    }
  }
}

/// The receiving end of a subscriber channel.
pub struct Subscription {
  pub rx: mpsc::UnboundedReceiver<ReporterEvent>,
}

/// Fan-out event bus. Workers clone this and call `emit()` — events are
/// delivered to all subscribers registered at build time.
///
/// Clone is cheap (Arc internals). All clones share the same subscriber list.
#[derive(Clone)]
pub struct EventBus {
  inner: Arc<EventBusInner>,
}

struct EventBusInner {
  /// Subscriber channels — frozen after build. Read-only during emit (no lock needed).
  /// `close()` swaps to empty Vec via `std::sync::RwLock` (write only on shutdown).
  subscribers: std::sync::RwLock<Vec<mpsc::UnboundedSender<ReporterEvent>>>,
}

impl EventBus {
  /// Emit an event to all subscribers. Lock-free read path — `RwLock::read()` never
  /// blocks other readers. Only `close()` takes a write lock (once, at shutdown).
  pub async fn emit(&self, event: ReporterEvent) {
    let subs = self.inner.subscribers.read().expect("EventBus RwLock poisoned");
    if subs.is_empty() {
      return;
    }
    let last = subs.len() - 1;
    for sub in &subs[..last] {
      let _ = sub.send(event.clone());
    }
    let _ = subs[last].send(event);
  }

  /// Explicitly close all sender channels.
  pub fn close(&self) {
    self
      .inner
      .subscribers
      .write()
      .expect("EventBus RwLock poisoned")
      .clear();
  }
}

// ── Reporter Driver ──

/// Standalone consumer that drains a `Subscription` and drives a `ReporterSet`.
/// Decoupled from test execution — can run as an independent tokio task.
///
/// Spawn this with `tokio::spawn(driver.run())`. When the event bus is dropped
/// (all senders gone), the subscription channel closes, the driver finalizes
/// all reporters, and returns the `ReporterSet` for potential reuse.
pub struct ReporterDriver {
  reporters: ReporterSet,
  subscription: Subscription,
}

impl ReporterDriver {
  pub fn new(reporters: ReporterSet, subscription: Subscription) -> Self {
    Self {
      reporters,
      subscription,
    }
  }

  /// Consume events until the channel closes, finalize reporters, return them.
  pub async fn run(mut self) -> ReporterSet {
    while let Some(event) = self.subscription.rx.recv().await {
      self.reporters.emit(&event).await;
    }
    self.reporters.finalize().await;
    self.reporters
  }
}

// ── Factory ──

/// Unified reporter factory. Creates reporters from config names, routing
/// mode-dependent reporters (terminal, json, junit) based on `mode`.
pub(crate) fn create_reporters(
  names: &[crate::config::ReporterConfig],
  output_dir: &std::path::Path,
  _has_bdd: bool,
  quiet: bool,
  report_slow_tests: Option<crate::config::ReportSlowTestsConfig>,
) -> ReporterSet {
  let mut reporters: Vec<Box<dyn Reporter>> = Vec::new();
  let mut has_terminal = false;

  for config in names {
    match config.name.as_str() {
      // Terminal reporter handles both E2E and BDD — detects BDD by step metadata.
      "terminal" | "list" | "bdd" | "default" | "" => {
        if !has_terminal && !quiet {
          reporters.push(Box::new(
            terminal::TerminalReporter::new().with_slow_tests_config(report_slow_tests.clone()),
          ));
          has_terminal = true;
        }
      },
      "json" => {
        reporters.push(Box::new(json::JsonReporter::new(output_dir.join("results.json"))));
      },
      "junit" => {
        reporters.push(Box::new(junit::JUnitReporter::new(output_dir.join("junit.xml"))));
      },

      // ── Shared reporters (same for both modes) ──
      "html" => {
        reporters.push(Box::new(html::HtmlReporter::new(output_dir.join("report.html"))));
      },
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
      },
      "progress" => {
        reporters.push(Box::new(progress::ProgressReporter::new()));
      },
      "rerun" => {
        reporters.push(Box::new(rerun::RerunReporter::new(output_dir.join("@rerun.txt"))));
      },

      // ── BDD-specific reporters (usable in any mode) ──
      "cucumber-json" | "cucumber" => {
        reporters.push(Box::new(bdd::cucumber_json::CucumberJsonReporter::new(
          output_dir.join("cucumber.json"),
        )));
      },
      "messages" | "ndjson" => {
        reporters.push(Box::new(bdd::messages::CucumberMessagesReporter::new(
          output_dir.join("cucumber-messages.ndjson"),
        )));
      },
      "usage" => {
        reporters.push(Box::new(bdd::usage::UsageReporter::new()));
      },

      other => {
        tracing::warn!("unknown reporter: {other}, skipping");
      },
    }
  }

  if reporters.is_empty() {
    reporters.push(Box::new(terminal::TerminalReporter::new()));
  }

  // Always add the rerun reporter so @rerun.txt is available for --last-failed.
  let has_rerun = names.iter().any(|c| c.name == "rerun");
  if !has_rerun {
    reporters.push(Box::new(rerun::RerunReporter::new(output_dir.join("@rerun.txt"))));
  }

  ReporterSet::new(reporters)
}
