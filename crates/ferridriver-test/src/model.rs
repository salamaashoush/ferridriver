//! Core test model types: `TestId`, `TestCase`, `TestSuite`, `TestPlan`, `TestOutcome`,
//! `TestInfo`, `TestStep`, `SuiteMode`.

use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
  /// Source line number (used by rerun reporter for `file:line` output).
  pub line: Option<usize>,
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

  /// File path with optional line number (e.g., `features/login.feature:15`).
  #[must_use]
  pub fn file_location(&self) -> String {
    match self.line {
      Some(line) => format!("{}:{}", self.file, line),
      None => self.file.clone(),
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
#[derive(Clone)]
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
  /// Per-test fixture overrides from `test.use()`. Merged with global config by the worker.
  pub use_options: Option<serde_json::Value>,
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
#[derive(Clone)]
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
#[derive(Clone)]
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
  dyn Fn(FixturePool, Arc<TestInfo>) -> Pin<Box<dyn Future<Output = Result<(), TestFailure>> + Send>> + Send + Sync,
>;

// ── Test Plan ──

/// The full test plan after discovery + filtering + sharding.
#[derive(Clone)]
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

// ── Plan Builder ──

/// Suite metadata for plan building.
pub struct SuiteDef {
  /// Suite ID (e.g. `"file::SuiteName"`). Must match `TestCase.id.suite`.
  pub id: String,
  pub name: String,
  pub file: String,
  pub mode: SuiteMode,
}

/// Hook registration for plan building.
pub struct HookDef {
  /// Suite ID this hook belongs to. Empty string = root/default suite.
  pub suite_id: String,
  pub kind: HookKind,
}

/// Hook kind with the associated callback.
pub enum HookKind {
  BeforeAll(SuiteHookFn),
  AfterAll(SuiteHookFn),
  BeforeEach(HookFn),
  AfterEach(HookFn),
}

/// Builds a `TestPlan` from flat test cases, suite definitions, and hooks.
///
/// Groups tests by `TestCase.id.suite`, attaches hooks to matching suites,
/// and respects suite mode (parallel/serial). This is the single place
/// where suite→test→hook association happens — callers (NAPI, CLI, macros)
/// just register flat data.
pub struct TestPlanBuilder {
  tests: Vec<TestCase>,
  suites: Vec<SuiteDef>,
  hooks: Vec<HookDef>,
}

impl TestPlanBuilder {
  pub fn new() -> Self {
    Self {
      tests: Vec::new(),
      suites: Vec::new(),
      hooks: Vec::new(),
    }
  }

  pub fn add_test(&mut self, test: TestCase) {
    self.tests.push(test);
  }

  pub fn add_suite(&mut self, suite: SuiteDef) {
    self.suites.push(suite);
  }

  pub fn add_hook(&mut self, hook: HookDef) {
    self.hooks.push(hook);
  }

  /// Consume the builder and produce a `TestPlan`.
  ///
  /// Tests are grouped by `id.suite` (matching `SuiteDef.id`).
  /// Tests without a suite go into a default parallel suite.
  /// Hooks are attached to their matching suite by `suite_id`.
  pub fn build(self) -> TestPlan {
    use rustc_hash::FxHashMap;

    // Index suite metadata by ID.
    let suite_meta: FxHashMap<String, (String, String, SuiteMode)> = self
      .suites
      .into_iter()
      .map(|s| (s.id, (s.name, s.file, s.mode)))
      .collect();

    // Group tests by suite key.
    let mut grouped: FxHashMap<String, Vec<TestCase>> = FxHashMap::default();
    for tc in self.tests {
      let key = tc.id.suite.clone().unwrap_or_default();
      grouped.entry(key).or_default().push(tc);
    }

    // Build hooks per suite.
    let mut hook_map: FxHashMap<String, Hooks> = FxHashMap::default();
    for h in self.hooks {
      let hooks = hook_map.entry(h.suite_id).or_default();
      match h.kind {
        HookKind::BeforeAll(f) => hooks.before_all.push(f),
        HookKind::AfterAll(f) => hooks.after_all.push(f),
        HookKind::BeforeEach(f) => hooks.before_each.push(f),
        HookKind::AfterEach(f) => hooks.after_each.push(f),
      }
    }

    // Assemble suites.
    let mut plan_suites: Vec<TestSuite> = Vec::new();
    let mut total = 0usize;

    for (suite_key, tests) in grouped {
      total += tests.len();
      let (name, file, mode) = if suite_key.is_empty() {
        ("tests".to_string(), String::new(), SuiteMode::Parallel)
      } else if let Some((n, f, m)) = suite_meta.get(&suite_key) {
        (n.clone(), f.clone(), *m)
      } else {
        // Suite ID exists on tests but no SuiteDef was registered — use defaults.
        (suite_key.clone(), String::new(), SuiteMode::Parallel)
      };
      let hooks = hook_map.remove(&suite_key).unwrap_or_default();
      plan_suites.push(TestSuite {
        name,
        file,
        tests,
        hooks,
        annotations: Vec::new(),
        mode,
      });
    }

    TestPlan {
      suites: plan_suites,
      total_tests: total,
      shard: None,
    }
  }
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
  /// Snapshot path template (e.g. `{testDir}/__snapshots__/{testFilePath}/{arg}{ext}`).
  pub snapshot_path_template: Option<String>,
  /// Snapshot update mode.
  pub update_snapshots: crate::config::UpdateSnapshotsMode,
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
  /// Runtime annotations added via `test_info.annotate()`.
  pub annotations: Arc<Mutex<Vec<TestAnnotation>>>,
}

impl TestInfo {
  /// Create a minimal TestInfo for non-test-runner contexts (MCP, standalone).
  pub fn new_anonymous() -> Self {
    Self {
      test_id: TestId {
        file: String::new(),
        suite: None,
        name: "anonymous".into(),
        line: None,
      },
      title_path: Vec::new(),
      retry: 0,
      worker_index: 0,
      parallel_index: 0,
      repeat_each_index: 0,
      output_dir: PathBuf::new(),
      snapshot_dir: PathBuf::new(),
      snapshot_path_template: None,
      update_snapshots: crate::config::UpdateSnapshotsMode::default(),
      attachments: Arc::new(Mutex::new(Vec::new())),
      steps: Arc::new(Mutex::new(Vec::new())),
      soft_errors: Arc::new(Mutex::new(Vec::new())),
      timeout: Duration::from_secs(30),
      tags: Vec::new(),
      start_time: Instant::now(),
      event_bus: None,
      annotations: Arc::new(Mutex::new(Vec::new())),
    }
  }

  /// Add a structured annotation at runtime.
  pub async fn annotate(&self, type_name: impl Into<String>, description: impl Into<String>) {
    let mut annotations = self.annotations.lock().await;
    annotations.push(TestAnnotation::Info {
      type_name: type_name.into(),
      description: description.into(),
    });
  }

  /// Get all runtime annotations.
  pub async fn get_annotations(&self) -> Vec<TestAnnotation> {
    let annotations = self.annotations.lock().await;
    annotations.clone()
  }
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
  pub async fn begin_step(&self, title: impl Into<String>, category: StepCategory) -> StepHandle {
    let title = title.into();
    let step_id = format!("{}@{}", category, STEP_ID_COUNTER.fetch_add(1, Ordering::Relaxed));

    if let Some(bus) = &self.event_bus {
      bus
        .emit(crate::reporter::ReporterEvent::StepStarted(Box::new(
          crate::reporter::StepStartedEvent {
            test_id: self.test_id.clone(),
            step_id: step_id.clone(),
            parent_step_id: None,
            title: title.clone(),
            category: category.clone(),
          },
        )))
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
    let step_id = format!("{}@{}", category, STEP_ID_COUNTER.fetch_add(1, Ordering::Relaxed));

    if let Some(bus) = &self.event_bus {
      bus
        .emit(crate::reporter::ReporterEvent::StepStarted(Box::new(
          crate::reporter::StepStartedEvent {
            test_id: self.test_id.clone(),
            step_id: step_id.clone(),
            parent_step_id: Some(parent_step_id.to_string()),
            title: title.clone(),
            category: category.clone(),
          },
        )))
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

  /// Record a step that already executed elsewhere but still needs to flow
  /// through reporter events and the stored step tree.
  pub async fn record_step(
    &self,
    title: impl Into<String>,
    category: StepCategory,
    status: StepStatus,
    duration: Duration,
    error: Option<String>,
    metadata: Option<serde_json::Value>,
  ) {
    let title = title.into();
    let step_id = format!("{}@{}", category, STEP_ID_COUNTER.fetch_add(1, Ordering::Relaxed));

    if let Some(bus) = &self.event_bus {
      bus
        .emit(crate::reporter::ReporterEvent::StepStarted(Box::new(
          crate::reporter::StepStartedEvent {
            test_id: self.test_id.clone(),
            step_id: step_id.clone(),
            parent_step_id: None,
            title: title.clone(),
            category: category.clone(),
          },
        )))
        .await;
      bus
        .emit(crate::reporter::ReporterEvent::StepFinished(Box::new(
          crate::reporter::StepFinishedEvent {
            test_id: self.test_id.clone(),
            step_id: step_id.clone(),
            title: title.clone(),
            category: category.clone(),
            duration,
            error: error.clone(),
            metadata: metadata.clone(),
          },
        )))
        .await;
    }

    self
      .steps
      .lock()
      .await
      .push(TestStep {
        step_id,
        title,
        category,
        duration,
        status,
        error,
        location: None,
        parent_step_id: None,
        metadata,
        steps: Vec::new(),
      });
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
        .emit(crate::reporter::ReporterEvent::StepFinished(Box::new(
          crate::reporter::StepFinishedEvent {
            test_id: self.test_id.clone(),
            step_id: self.step_id.clone(),
            title: self.title.clone(),
            category: self.category.clone(),
            duration,
            error: error.clone(),
            metadata: self.metadata.clone(),
          },
        )))
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
    self.finish_with_status(StepStatus::Skipped, reason).await;
  }

  /// Complete this step as pending (not yet implemented).
  pub async fn pending(self, reason: Option<String>) {
    self.finish_with_status(StepStatus::Pending, reason).await;
  }

  async fn finish_with_status(self, status: StepStatus, error: Option<String>) {
    let duration = self.start.elapsed();

    if let Some(bus) = &self.event_bus {
      bus
        .emit(crate::reporter::ReporterEvent::StepFinished(Box::new(
          crate::reporter::StepFinishedEvent {
            test_id: self.test_id.clone(),
            step_id: self.step_id.clone(),
            title: self.title.clone(),
            category: self.category.clone(),
            duration,
            error: error.clone(),
            metadata: self.metadata.clone(),
          },
        )))
        .await;
    }

    let step = TestStep {
      step_id: self.step_id,
      title: self.title,
      category: self.category,
      duration,
      status,
      error,
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
  /// Step exists but is not yet implemented.
  Pending,
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

impl StepCategory {
  /// Whether this step category is visible in standard reporter output.
  /// TestStep and Hook are always shown. Expect, Fixture, PwApi are hidden
  /// unless verbose mode is enabled.
  pub fn is_visible(&self) -> bool {
    matches!(self, Self::TestStep | Self::Hook)
  }
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TestAnnotation {
  /// Skip this test. Optional condition: `"firefox"`, `"chromium"`, `"linux"`, `"ci"`, `"!webkit"`.
  /// When condition is None, always skips. When condition is Some, skips only if condition matches.
  Skip {
    reason: Option<String>,
    condition: Option<String>,
  },
  /// Triple the timeout for this test (×3). Optional condition + description.
  /// Matches Playwright's `test.slow()` / `test.slow(condition, description)`.
  Slow {
    reason: Option<String>,
    condition: Option<String>,
  },
  /// Known bug — skip with intent to fix. Same condition semantics as Skip.
  /// Matches Playwright's `test.fixme()` / `test.fixme(condition, description)`.
  Fixme {
    reason: Option<String>,
    condition: Option<String>,
  },
  /// Expect this test to fail (inverts pass/fail). Optional condition + description.
  /// Matches Playwright's `test.fail()` / `test.fail(condition, description)`.
  Fail {
    reason: Option<String>,
    condition: Option<String>,
  },
  Only,
  Tag(String),
  /// Structured metadata: type + description (e.g., issue/JIRA-1234, severity/critical).
  Info {
    type_name: String,
    description: String,
  },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ExpectedStatus {
  #[default]
  Pass,
  Fail,
}

// ── Runtime Modifiers (shared between JS test body and Rust worker) ──

/// Runtime test modifiers set by `test.skip()`, `test.fail()`, `test.slow()` inside
/// a test body. Shared via `Arc` between the NAPI layer (JS thread writes) and the
/// Rust worker (reads after callback returns).
///
/// Uses atomics and `std::sync::Mutex` for cross-thread safety. No actual race —
/// the worker reads strictly after the TSFN callback completes.
pub struct TestModifiers {
  /// Set by `test.skip()` / `test.fixme()` inside test body.
  pub skipped: AtomicBool,
  /// Reason for runtime skip.
  pub skip_reason: std::sync::Mutex<Option<String>>,
  /// Set by `test.fail()` inside test body — inverts pass/fail.
  pub expected_failure: AtomicBool,
  /// Set by `test.slow()` inside test body.
  pub slow: AtomicBool,
  /// Set by `testInfo.setTimeout()` inside test body.
  pub timeout_override: std::sync::Mutex<Option<u64>>,
}

impl Default for TestModifiers {
  fn default() -> Self {
    Self {
      skipped: AtomicBool::new(false),
      skip_reason: std::sync::Mutex::new(None),
      expected_failure: AtomicBool::new(false),
      slow: AtomicBool::new(false),
      timeout_override: std::sync::Mutex::new(None),
    }
  }
}

impl std::fmt::Debug for TestModifiers {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("TestModifiers")
      .field("skipped", &self.skipped.load(Ordering::Relaxed))
      .field("expected_failure", &self.expected_failure.load(Ordering::Relaxed))
      .field("slow", &self.slow.load(Ordering::Relaxed))
      .finish()
  }
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
  /// Annotations from the test definition + runtime (tags, severity, issues, etc.).
  pub annotations: Vec<TestAnnotation>,
  /// Project/run metadata (from config). Available to reporters for JSON/HTML output.
  pub metadata: serde_json::Value,
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

// ── Unified Fixtures ──

/// Unified fixture bag for test/step/hook callbacks.
///
/// E2E tests and hooks get browser/page/context/request/testInfo.
/// BDD steps additionally get args/data_table/doc_string.
/// BDD hooks get the E2E fields with BDD fields as None.
#[derive(Clone)]
pub struct TestFixtures {
  pub browser: Arc<ferridriver::Browser>,
  pub page: Arc<ferridriver::Page>,
  pub context: Arc<ferridriver::context::ContextRef>,
  pub request: Arc<ferridriver::api_request::APIRequestContext>,
  pub test_info: Arc<TestInfo>,
  pub modifiers: Arc<TestModifiers>,
  pub browser_config: crate::config::BrowserConfig,
  // BDD fields (None for E2E tests/hooks)
  pub bdd_args: Option<Vec<serde_json::Value>>,
  pub bdd_data_table: Option<Vec<Vec<String>>>,
  pub bdd_doc_string: Option<String>,
}
