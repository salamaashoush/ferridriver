//! Core test model types: `TestId`, `TestCase`, `TestSuite`, `TestPlan`, `TestOutcome`,
//! `TestInfo`, `TestStep`, `SuiteMode`.

use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use crate::fixture::FixturePool;
use crate::reporter::EventBus;

// ── Test Identity ──

/// Globally unique test identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TestId {
  pub file: String,
  pub suite: Option<String>,
  pub name: String,
}

impl TestId {
  /// Stable full name for display and hashing.
  #[must_use]
  pub fn full_name(&self) -> String {
    match &self.suite {
      Some(s) => format!("{} > {} > {}", self.file, s, self.name),
      None => format!("{} > {}", self.file, self.name),
    }
  }
}

impl fmt::Display for TestId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.full_name())
  }
}

// ── Test Function ──

/// The async test body: takes a fixture pool, returns success or failure.
/// Uses `Arc` so tests can be re-dispatched for retries and repeatEach.
pub type TestFn =
  Arc<dyn Fn(FixturePool) -> Pin<Box<dyn Future<Output = Result<(), TestFailure>> + Send>> + Send + Sync>;

// ── Test Case ──

/// A single test case with metadata and body.
pub struct TestCase {
  pub id: TestId,
  pub test_fn: TestFn,
  /// Fixture names this test requests (drives DAG resolution).
  pub fixture_requests: Vec<String>,
  /// Annotations: skip, slow, fixme, tags.
  pub annotations: Vec<TestAnnotation>,
  /// Per-test timeout override.
  pub timeout: Option<Duration>,
  /// Per-test retry override.
  pub retries: Option<u32>,
  /// Expected status (for `test.fail()` annotation).
  pub expected_status: ExpectedStatus,
}

// ── Test Suite ──

/// How tests within a suite are scheduled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SuiteMode {
  /// Tests run in parallel (default for fullyParallel, or `test.describe.parallel()`).
  #[default]
  Parallel,
  /// Tests run sequentially in one worker. If one fails, rest are skipped.
  /// Maps to `test.describe.serial()`.
  Serial,
}

/// A group of tests (maps to `test.describe` / `#[cfg(test)] mod`).
pub struct TestSuite {
  pub name: String,
  pub file: String,
  pub tests: Vec<TestCase>,
  pub hooks: Hooks,
  /// Suite-level annotations (applied to all children).
  pub annotations: Vec<TestAnnotation>,
  /// Execution mode for this suite.
  pub mode: SuiteMode,
}

/// Lifecycle hooks attached to a suite.
pub struct Hooks {
  /// Runs once per suite per worker (no test context).
  pub before_all: Vec<SuiteHookFn>,
  /// Runs once per suite per worker on teardown (no test context).
  pub after_all: Vec<SuiteHookFn>,
  /// Runs before each test (receives test info with tags, name, step API).
  pub before_each: Vec<HookFn>,
  /// Runs after each test, even on failure (receives test info).
  pub after_each: Vec<HookFn>,
}

impl Default for Hooks {
  fn default() -> Self {
    Self {
      before_all: Vec::new(),
      after_all: Vec::new(),
      before_each: Vec::new(),
      after_each: Vec::new(),
    }
  }
}

/// Suite-scoped hook (before_all / after_all). Receives only the fixture pool.
/// Runs once per suite per worker, no test context available.
pub type SuiteHookFn =
  Arc<dyn Fn(FixturePool) -> Pin<Box<dyn Future<Output = Result<(), TestFailure>> + Send>> + Send + Sync>;

/// Test-scoped hook (before_each / after_each). Receives fixture pool + `TestInfo`.
/// `TestInfo` provides access to test tags, name, step API, and event bus.
pub type HookFn = Arc<
  dyn Fn(FixturePool, Arc<TestInfo>) -> Pin<Box<dyn Future<Output = Result<(), TestFailure>> + Send>>
    + Send
    + Sync,
>;

// ── Test Plan ──

/// The full test plan after discovery + filtering + sharding.
pub struct TestPlan {
  pub suites: Vec<TestSuite>,
  /// Total test count (after filtering, before retry expansion).
  pub total_tests: usize,
  /// Shard info if sharding is active.
  pub shard: Option<ShardInfo>,
}

#[derive(Debug, Clone)]
pub struct ShardInfo {
  pub current: u32,
  pub total: u32,
}

// ── Test Info (runtime context available during test execution) ──

/// Runtime test information accessible during test execution.
/// Mirrors Playwright's `TestInfo` interface.
#[derive(Clone)]
pub struct TestInfo {
  /// Test ID.
  pub test_id: TestId,
  /// Title path: ["suite", "subsuite", "test name"].
  pub title_path: Vec<String>,
  /// Current retry attempt (0-indexed).
  pub retry: u32,
  /// Worker index (0-based).
  pub worker_index: u32,
  /// Parallel index (same as worker_index for now).
  pub parallel_index: u32,
  /// repeatEach index (0-based).
  pub repeat_each_index: u32,
  /// Output directory for this test's artifacts.
  pub output_dir: PathBuf,
  /// Snapshot directory for this test.
  pub snapshot_dir: PathBuf,
  /// Collected attachments.
  pub attachments: Arc<Mutex<Vec<Attachment>>>,
  /// Collected test steps.
  pub steps: Arc<Mutex<Vec<TestStep>>>,
  /// Soft assertion errors (collected, not thrown).
  pub soft_errors: Arc<Mutex<Vec<TestFailure>>>,
  /// Test timeout.
  pub timeout: Duration,
  /// Tags from annotations.
  pub tags: Vec<String>,
  /// Test start time.
  pub start_time: Instant,
  /// Event bus for real-time step event emission (set by worker).
  pub event_bus: Option<EventBus>,
}

impl TestInfo {
  /// Add an attachment to this test.
  pub async fn attach(&self, name: String, content_type: String, body: AttachmentBody) {
    let mut attachments = self.attachments.lock().await;
    attachments.push(Attachment {
      name,
      content_type,
      body,
    });
  }

  /// Record a soft assertion error (test continues, fails at end).
  pub async fn add_soft_error(&self, error: TestFailure) {
    let mut errors = self.soft_errors.lock().await;
    errors.push(error);
  }

  /// Check if any soft errors have been collected.
  pub async fn has_soft_errors(&self) -> bool {
    let errors = self.soft_errors.lock().await;
    !errors.is_empty()
  }

  /// Drain all soft errors for final reporting.
  pub async fn drain_soft_errors(&self) -> Vec<TestFailure> {
    let mut errors = self.soft_errors.lock().await;
    errors.drain(..).collect()
  }

  /// Record a test step.
  pub async fn push_step(&self, step: TestStep) {
    let mut steps = self.steps.lock().await;
    steps.push(step);
  }

  /// Get elapsed time since test start.
  pub fn elapsed(&self) -> Duration {
    self.start_time.elapsed()
  }

  /// Begin a new step with real-time event emission.
  ///
  /// Returns a `StepHandle` that must be completed via `handle.end()`.
  /// Emits `ReporterEvent::StepStarted` immediately if an event bus is available.
  pub async fn begin_step(
    &self,
    title: impl Into<String>,
    category: StepCategory,
  ) -> StepHandle {
    let title = title.into();
    let step_id = format!(
      "{}@{}",
      category,
      STEP_ID_COUNTER.fetch_add(1, Ordering::Relaxed)
    );

    if let Some(bus) = &self.event_bus {
      bus.emit(crate::reporter::ReporterEvent::StepStarted(Box::new(crate::reporter::StepStartedEvent {
        test_id: self.test_id.clone(),
        step_id: step_id.clone(),
        parent_step_id: None,
        title: title.clone(),
        category: category.clone(),
      })))
      .await;
    }

    StepHandle {
      step_id,
      test_id: self.test_id.clone(),
      title,
      category,
      parent_step_id: None,
      start: Instant::now(),
      metadata: None,
      event_bus: self.event_bus.clone(),
      steps: Arc::clone(&self.steps),
    }
  }

  /// Begin a nested step (child of a parent step).
  pub async fn begin_child_step(
    &self,
    title: impl Into<String>,
    category: StepCategory,
    parent_step_id: &str,
  ) -> StepHandle {
    let title = title.into();
    let step_id = format!(
      "{}@{}",
      category,
      STEP_ID_COUNTER.fetch_add(1, Ordering::Relaxed)
    );

    if let Some(bus) = &self.event_bus {
      bus.emit(crate::reporter::ReporterEvent::StepStarted(Box::new(crate::reporter::StepStartedEvent {
        test_id: self.test_id.clone(),
        step_id: step_id.clone(),
        parent_step_id: Some(parent_step_id.to_string()),
        title: title.clone(),
        category: category.clone(),
      })))
      .await;
    }

    StepHandle {
      step_id,
      test_id: self.test_id.clone(),
      title,
      category,
      parent_step_id: Some(parent_step_id.to_string()),
      start: Instant::now(),
      metadata: None,
      event_bus: self.event_bus.clone(),
      steps: Arc::clone(&self.steps),
    }
  }
}

/// Global step ID counter for unique step identification.
static STEP_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Handle to an in-progress step. Must be completed via `end()`.
///
/// On `end()`:
/// - Emits `ReporterEvent::StepFinished` for real-time reporting
/// - Pushes a `TestStep` to the test's step list for batch reporting
pub struct StepHandle {
  pub step_id: String,
  pub test_id: TestId,
  pub title: String,
  pub category: StepCategory,
  pub parent_step_id: Option<String>,
  pub start: Instant,
  /// Arbitrary metadata attached to this step (set before calling `end()`).
  pub metadata: Option<serde_json::Value>,
  event_bus: Option<EventBus>,
  steps: Arc<Mutex<Vec<TestStep>>>,
}

impl StepHandle {
  /// Complete this step. Pass `None` for success, `Some(msg)` for failure.
  pub async fn end(self, error: Option<String>) {
    let duration = self.start.elapsed();
    let status = if error.is_some() {
      StepStatus::Failed
    } else {
      StepStatus::Passed
    };

    // Emit real-time event.
    if let Some(bus) = &self.event_bus {
      bus
        .emit(crate::reporter::ReporterEvent::StepFinished(Box::new(crate::reporter::StepFinishedEvent {
          test_id: self.test_id.clone(),
          step_id: self.step_id.clone(),
          title: self.title.clone(),
          category: self.category.clone(),
          duration,
          error: error.clone(),
        })))
        .await;
    }

    // Push to batch step list (for TestOutcome.steps).
    let step = TestStep {
      step_id: self.step_id,
      title: self.title,
      category: self.category,
      duration,
      status,
      error,
      location: None,
      parent_step_id: self.parent_step_id,
      metadata: self.metadata.clone(),
      steps: Vec::new(),
    };
    self.steps.lock().await.push(step);
  }

  /// Complete this step as skipped.
  pub async fn skip(self, reason: Option<String>) {
    let duration = self.start.elapsed();

    if let Some(bus) = &self.event_bus {
      bus
        .emit(crate::reporter::ReporterEvent::StepFinished(Box::new(crate::reporter::StepFinishedEvent {
          test_id: self.test_id.clone(),
          step_id: self.step_id.clone(),
          title: self.title.clone(),
          category: self.category.clone(),
          duration,
          error: reason.clone(),
        })))
        .await;
    }

    let step = TestStep {
      step_id: self.step_id,
      title: self.title,
      category: self.category,
      duration,
      status: StepStatus::Skipped,
      error: reason,
      location: None,
      parent_step_id: self.parent_step_id,
      metadata: self.metadata,
      steps: Vec::new(),
    };
    self.steps.lock().await.push(step);
  }
}

// ── Test Step ──

/// A structured test step (maps to Playwright's `test.step()`).
#[derive(Debug, Clone)]
pub struct TestStep {
  /// Unique step identifier (for parent/child tracking and reporter correlation).
  pub step_id: String,
  pub title: String,
  pub category: StepCategory,
  pub duration: Duration,
  /// Step completion status.
  pub status: StepStatus,
  pub error: Option<String>,
  /// Source location (e.g., "file.rs:42" or "feature.feature:10").
  pub location: Option<String>,
  /// Parent step ID for nesting.
  pub parent_step_id: Option<String>,
  /// Arbitrary metadata for domain-specific extensions (e.g., BDD keyword, tags).
  /// Reporters can use this for custom rendering without the core needing domain knowledge.
  pub metadata: Option<serde_json::Value>,
  pub steps: Vec<TestStep>,
}

/// Status of a completed test step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
  Passed,
  Failed,
  Skipped,
}

/// Category of a test step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepCategory {
  /// User-defined step via test.step().
  TestStep,
  /// Expect assertion.
  Expect,
  /// Fixture setup/teardown.
  Fixture,
  /// Hook execution.
  Hook,
  /// Playwright API call.
  PwApi,
}

impl fmt::Display for StepCategory {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::TestStep => write!(f, "test.step"),
      Self::Expect => write!(f, "expect"),
      Self::Fixture => write!(f, "fixture"),
      Self::Hook => write!(f, "hook"),
      Self::PwApi => write!(f, "pw:api"),
    }
  }
}

// ── Annotations ──

#[derive(Debug, Clone)]
pub enum TestAnnotation {
  Skip { reason: Option<String> },
  Slow,
  Fixme { reason: Option<String> },
  Fail,
  Tag(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ExpectedStatus {
  #[default]
  Pass,
  Fail,
}

// ── Outcome ──

/// Status of a completed test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestStatus {
  Passed,
  Failed,
  TimedOut,
  Skipped,
  /// Passed on retry (flaky).
  Flaky,
  /// Interrupted by signal/cancellation.
  Interrupted,
}

impl fmt::Display for TestStatus {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Passed => write!(f, "passed"),
      Self::Failed => write!(f, "failed"),
      Self::TimedOut => write!(f, "timed out"),
      Self::Skipped => write!(f, "skipped"),
      Self::Flaky => write!(f, "flaky"),
      Self::Interrupted => write!(f, "interrupted"),
    }
  }
}

/// Result of a single test attempt.
#[derive(Debug, Clone)]
pub struct TestOutcome {
  pub test_id: TestId,
  pub status: TestStatus,
  pub duration: Duration,
  pub attempt: u32,
  pub max_attempts: u32,
  pub error: Option<TestFailure>,
  pub attachments: Vec<Attachment>,
  pub steps: Vec<TestStep>,
  pub stdout: String,
  pub stderr: String,
}

/// A test failure with diagnostic information.
#[derive(Debug, Clone)]
pub struct TestFailure {
  pub message: String,
  pub stack: Option<String>,
  pub diff: Option<String>,
  /// Screenshot on failure (auto-captured).
  pub screenshot: Option<Vec<u8>>,
}

impl fmt::Display for TestFailure {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}", self.message)?;
    if let Some(diff) = &self.diff {
      write!(f, "\n{diff}")?;
    }
    Ok(())
  }
}

impl std::error::Error for TestFailure {}

/// Enables `?` on any `Result<T, String>` inside test functions.
/// Locator methods (click, fill, press, etc.) return `Result<T, String>`.
impl From<String> for TestFailure {
  fn from(message: String) -> Self {
    Self {
      message,
      stack: None,
      diff: None,
      screenshot: None,
    }
  }
}

impl From<&str> for TestFailure {
  fn from(message: &str) -> Self {
    Self::from(message.to_string())
  }
}

/// An artifact attached to a test result.
#[derive(Debug, Clone)]
pub struct Attachment {
  pub name: String,
  pub content_type: String,
  pub body: AttachmentBody,
}

#[derive(Debug, Clone)]
pub enum AttachmentBody {
  Bytes(Vec<u8>),
  Path(PathBuf),
}
