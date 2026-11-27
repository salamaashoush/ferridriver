//! Runtime test execution context: [`TestInfo`], [`TestOutput`], [`OpenSteps`], and [`StepHandle`].

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use tokio::sync::Mutex;

use crate::reporter::EventBus;

use super::{
  Attachment, AttachmentBody, StepCategory, StepLocation, StepStatus, TestAnnotation, TestFailure, TestId, TestStep,
};

// ── Test Info (runtime context available during test execution) ──

/// Runtime test information accessible during test execution.
/// Mirrors Playwright's `TestInfo` interface.
#[derive(Clone)]
pub struct TestInfo {
  /// Test ID.
  pub test_id: TestId,
  /// Title path: `["suite", "subsuite", "test name"]`.
  pub title_path: Vec<String>,
  /// Current retry attempt (0-indexed).
  pub retry: u32,
  /// Worker index (0-based).
  pub worker_index: u32,
  /// Parallel index (same as `worker_index` for now).
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
  /// When true, every snapshot comparison short-circuits to a pass.
  /// Mirrors Playwright's `--ignore-snapshots` CLI flag.
  pub ignore_snapshots: bool,
  /// Collected attachments.
  pub attachments: Arc<Mutex<Vec<Attachment>>>,
  /// Every snapshot comparison the test made, in order.
  pub snapshots: Arc<std::sync::Mutex<Vec<crate::snapshot_record::SnapshotComparison>>>,
  /// The images those comparisons attached. Kept apart from `attachments`
  /// behind a plain mutex because `toMatchSnapshot` records from a
  /// synchronous matcher, which cannot await the attachments lock.
  pub snapshot_attachments: Arc<std::sync::Mutex<Vec<Attachment>>>,
  /// Collected test steps. A plain mutex: a browser call opens and
  /// closes its step from an action observer, which cannot await.
  pub steps: Arc<std::sync::Mutex<Vec<TestStep>>>,
  /// Soft assertion errors (collected, not thrown).
  pub soft_errors: Arc<std::sync::Mutex<Vec<TestFailure>>>,
  /// Set when the attempt failed in a way a retry cannot fix
  /// (Playwright's `_hasNonRetriableError`).
  pub non_retriable: Arc<std::sync::atomic::AtomicBool>,
  /// Hard errors collected during test execution. The worker pushes
  /// the primary failure here after the test body returns; afterEach
  /// hooks observe the full list. Mirrors Playwright's
  /// `testInfo.errors`.
  pub errors: Arc<Mutex<Vec<TestFailure>>>,
  /// Optional suffix used to differentiate snapshot files between
  /// configurations. Mirrors Playwright's `testInfo.snapshotSuffix`.
  pub snapshot_suffix: Arc<Mutex<String>>,
  /// Source column number where the test is declared. The TS / Rust
  /// discovery layers don't parse columns yet, so this is `None` in
  /// practice; surfaced for parity with Playwright's
  /// `testInfo.column`.
  pub column: Option<u32>,
  /// Snapshot of the project entry the test belongs to. Each test in
  /// a multi-project run sees its own project; single-project runs
  /// see `None` since there's no per-project context.
  pub project: Option<crate::config::ProjectConfig>,
  /// Snapshot of the active `TestConfig`. Cloned at test-info
  /// construction time so the `testInfo.config` accessor is cheap.
  pub config_snapshot: Option<Arc<crate::config::TestConfig>>,
  /// The `expect` block this test runs under, already resolved against
  /// its project (`TestConfig::resolved_expect`). Matchers start from
  /// these and layer their per-call options on top.
  pub expect: Arc<crate::config::ExpectConfig>,
  /// Directory of the config file the run resolved from — what a
  /// relative `snapshotPathTemplate` is relative TO (Playwright's
  /// `configDir`). Empty means the working directory.
  pub config_dir: PathBuf,
  /// The project's `testDir`: `{testDir}` in a template, and what a
  /// spec's path is made relative to for `{testFilePath}`.
  pub test_dir: PathBuf,
  /// Snapshot-name indices for this test, so two anonymous snapshots or
  /// two calls with the same name do not overwrite each other. Aria
  /// snapshots count separately, exactly as upstream.
  pub snapshot_names: Arc<std::sync::Mutex<crate::snapshot_path::SnapshotNames>>,
  pub aria_snapshot_names: Arc<std::sync::Mutex<crate::snapshot_path::SnapshotNames>>,
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
  /// Composite session key of the live per-test trace recorder, set by
  /// the worker once tracing starts on the test's context. Steps opened
  /// while this is `Some` are mirrored into the trace as action events.
  pub trace_composite: Arc<std::sync::Mutex<Option<String>>>,
  /// `step_id` -> trace call id, so child steps nest under their parent
  /// in the trace viewer's action tree.
  pub trace_step_calls: Arc<std::sync::Mutex<rustc_hash::FxHashMap<String, String>>>,
  /// Steps opened and not yet ended — where a nested step finds its
  /// parent, its title path and the box it inherits.
  pub open_steps: Arc<std::sync::Mutex<OpenSteps>>,
  /// What the test itself printed. A spec's `console.log` belongs to the
  /// test that ran it, not to the process — reporters, the HTML report
  /// and the UI's terminal all read it back off the outcome. A plain
  /// mutex: the writer is a console sink running on the script VM's
  /// thread, with no async context to await in.
  pub output: Arc<std::sync::Mutex<TestOutput>>,
}

/// Output a test produced while it ran, kept apart by stream.
#[derive(Debug, Default, Clone)]
pub struct TestOutput {
  pub stdout: String,
  pub stderr: String,
}

impl TestOutput {
  /// Append one line, adding the newline the writer left off.
  pub fn push_line(&mut self, stderr: bool, text: &str) {
    let target = if stderr { &mut self.stderr } else { &mut self.stdout };
    target.push_str(text);
    if !text.ends_with('\n') {
      target.push('\n');
    }
  }
}

/// A soft assertion records into the test it was raised in.
///
/// The rule that a soft failure is recorded rather than raised lives in
/// `ferridriver_expect::soft`; this is only where the failures land.
impl ferridriver_expect::SoftSink for TestInfo {
  fn record(&self, failure: &ferridriver_expect::AssertionFailure) {
    self.add_soft_error(TestFailure {
      message: failure.message.clone(),
      stack: failure.location.map(|l| l.to_string()),
      diff: failure.diff.clone(),
      screenshot: None,
      error_context: failure.error_context.clone(),
    });
  }

  fn forbid_retry(&self) {
    TestInfo::forbid_retry(self);
  }
}

impl TestInfo {
  /// Who issued this attempt's browser calls, as the pending-call registry
  /// files them (`ferridriver::trace::pending_calls`). The worker index
  /// keeps two projects running the same test apart.
  #[must_use]
  pub fn call_owner(&self) -> String {
    format!("worker {}: {}", self.worker_index, self.test_id.execution_key())
  }

  /// Create a minimal `TestInfo` for non-test-runner contexts (MCP, standalone).
  #[must_use]
  pub fn new_anonymous() -> Self {
    Self {
      test_id: TestId {
        root_path: None,
        repeat_each_index: 0,
        file: String::new(),
        suite: None,
        name: "anonymous".into(),
        line: None,
        column: None,
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
      ignore_snapshots: false,
      attachments: Arc::new(Mutex::new(Vec::new())),
      snapshots: Arc::default(),
      snapshot_attachments: Arc::default(),
      steps: Arc::default(),
      soft_errors: Arc::new(std::sync::Mutex::new(Vec::new())),
      non_retriable: Arc::new(std::sync::atomic::AtomicBool::new(false)),
      errors: Arc::new(Mutex::new(Vec::new())),
      snapshot_suffix: Arc::new(Mutex::new(String::new())),
      column: None,
      project: None,
      config_snapshot: None,
      expect: Arc::new(crate::config::ExpectConfig::default()),
      config_dir: PathBuf::default(),
      test_dir: PathBuf::default(),
      snapshot_names: Arc::default(),
      aria_snapshot_names: Arc::default(),
      timeout: Duration::from_secs(30),
      tags: Vec::new(),
      start_time: Instant::now(),
      event_bus: None,
      annotations: Arc::new(Mutex::new(Vec::new())),
      trace_composite: Arc::new(std::sync::Mutex::new(None)),
      trace_step_calls: Arc::new(std::sync::Mutex::new(rustc_hash::FxHashMap::default())),
      open_steps: Arc::default(),
      output: Arc::new(std::sync::Mutex::new(TestOutput::default())),
    }
  }

  /// Record one line the test printed and, when a bus is attached,
  /// publish it live. A reporter that streams output (`line`, a UI's
  /// terminal) shows it while the test is still running; the same text
  /// is replayed in bulk on the outcome for reporters that do not.
  pub fn emit_output(&self, stderr: bool, text: &str) {
    if let Ok(mut held) = self.output.lock() {
      held.push_line(stderr, text);
    }
    if let Some(bus) = &self.event_bus {
      bus.emit(crate::reporter::ReporterEvent::TestOutput(Arc::new(
        crate::reporter::TestOutputEvent {
          test_id: self.test_id.clone(),
          project: self.project_name().unwrap_or_default(),
          stderr,
          text: text.to_string(),
        },
      )));
    }
  }

  /// Record an annotation the test body pushed, keeping a modifier
  /// (`skip`, `fixme`, `fail`, `slow`) typed and a missing description
  /// absent, as Playwright's `testInfo.annotations` does.
  pub async fn annotate_runtime(&self, type_name: String, description: Option<String>) {
    let annotation = match type_name.as_str() {
      "skip" => TestAnnotation::Skip {
        reason: description,
        condition: None,
      },
      "fixme" => TestAnnotation::Fixme {
        reason: description,
        condition: None,
      },
      "fail" => TestAnnotation::Fail {
        reason: description,
        condition: None,
      },
      "slow" => TestAnnotation::Slow {
        reason: description,
        condition: None,
      },
      _ => TestAnnotation::Info {
        type_name,
        description: description.unwrap_or_default(),
      },
    };
    self.annotations.lock().await.push(annotation);
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
  /// Playwright: `testInfo.attach(name, { body?, contentType?, path? })`.
  /// `body` is bytes or text (`Vec<u8>`, `&[u8]`, `String`, `&str`) or a
  /// file to copy (`PathBuf`, `&Path`).
  ///
  /// ```ignore
  /// test_info.attach("after-delete", "image/png", page.screenshot().await?).await;
  /// test_info.attach("log", "text/plain", "ok").await;
  /// ```
  pub async fn attach(
    &self,
    name: impl Into<String>,
    content_type: impl Into<String>,
    body: impl Into<AttachmentBody>,
  ) {
    self
      .attach_to_step(name.into(), content_type.into(), body.into(), None)
      .await;
  }

  /// [`Self::attach`] from inside a step (`stepInfo.attach`), so a
  /// report can show the attachment under the step that produced it.
  pub async fn attach_to_step(
    &self,
    name: String,
    content_type: String,
    body: AttachmentBody,
    step_id: Option<String>,
  ) {
    let mut attachments = self.attachments.lock().await;
    attachments.push(Attachment {
      name,
      content_type,
      body,
      step_id,
    });
  }

  /// Keep what a snapshot matcher compared and attached, and hand back
  /// its verdict.
  ///
  /// # Errors
  ///
  /// The matcher's own failure.
  pub fn record_snapshot(&self, judged: crate::snapshot::Judged) -> Result<Option<String>, TestFailure> {
    if let Some(comparison) = judged.comparison {
      self
        .snapshots
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(comparison);
    }
    self
      .snapshot_attachments
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .extend(judged.attachments);
    judged.result
  }

  /// Record a soft assertion error (test continues, fails at end).
  ///
  /// Synchronous on purpose: a soft failure is recorded from inside a
  /// matcher, and the value matchers have no `await` to spend.
  pub fn add_soft_error(&self, error: TestFailure) {
    self
      .soft_errors
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .push(error);
  }

  /// Keep this test from being retried after this attempt.
  pub fn forbid_retry(&self) {
    self.non_retriable.store(true, std::sync::atomic::Ordering::Relaxed);
  }

  /// Check if any soft errors have been collected.
  #[must_use]
  pub fn has_soft_errors(&self) -> bool {
    !self
      .soft_errors
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .is_empty()
  }

  /// Drain all soft errors for final reporting.
  #[must_use]
  pub fn drain_soft_errors(&self) -> Vec<TestFailure> {
    self
      .soft_errors
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .drain(..)
      .collect()
  }

  /// The failures a soft assertion has recorded so far, without
  /// draining them — what `testInfo.errors` reads.
  #[must_use]
  pub fn soft_error_messages(&self) -> Vec<String> {
    self
      .soft_errors
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .iter()
      .map(|e| e.message.clone())
      .collect()
  }

  /// Record a test step.
  pub fn push_step(&self, step: TestStep) {
    lock(&self.steps).push(step);
  }

  /// Get elapsed time since test start.
  #[must_use]
  pub fn elapsed(&self) -> Duration {
    self.start_time.elapsed()
  }

  /// Begin a new step with real-time event emission.
  ///
  /// Returns a `StepHandle` that must be completed via `handle.end()`.
  /// Emits `ReporterEvent::StepStarted` immediately if an event bus is available.
  pub async fn begin_step(&self, title: impl Into<String>, category: StepCategory) -> StepHandle {
    self.begin_step_at(title, category, None).await
  }

  /// [`Self::begin_step`] with the step's source location (a BDD step's
  /// `.feature` file line, a `test.step` call site). The location flows
  /// into the step's trace span as its stack frame — the viewer's
  /// Source tab — and into the recorded [`TestStep::location`].
  pub fn begin_step_at(
    &self,
    title: impl Into<String>,
    category: StepCategory,
    location: Option<StepLocation>,
  ) -> impl std::future::Future<Output = StepHandle> {
    std::future::ready(self.open_handle_step(title, category, None, location))
  }

  /// Begin a nested step (child of a parent step).
  pub fn begin_child_step(
    &self,
    title: impl Into<String>,
    category: StepCategory,
    parent_step_id: &str,
  ) -> impl std::future::Future<Output = StepHandle> {
    std::future::ready(self.open_handle_step(title, category, Some(parent_step_id), None))
  }

  /// [`Self::begin_child_step`] with the child's own source location.
  pub fn begin_child_step_at(
    &self,
    title: impl Into<String>,
    category: StepCategory,
    parent_step_id: &str,
    location: Option<StepLocation>,
  ) -> impl std::future::Future<Output = StepHandle> {
    std::future::ready(self.open_handle_step(title, category, Some(parent_step_id), location))
  }

  /// A step its opener holds and ends itself (a hook, a BDD step). While
  /// it is open, steps opened through [`Self::open_step_now`] with no
  /// scoped step inside it belong to it — Playwright's
  /// `_findLastPredefinedStep`.
  fn open_handle_step(
    &self,
    title: impl Into<String>,
    category: StepCategory,
    parent_step_id: Option<&str>,
    location: Option<StepLocation>,
  ) -> StepHandle {
    let mut handle = self.open_step(title, category, parent_step_id, location, StepDetails::default());
    lock(&self.open_steps).held.push((handle.step_id.clone(), handle.start));
    handle.held_in = Some(Arc::clone(&self.open_steps));
    handle
  }

  /// The one place a live step is opened: allocates the id, streams
  /// `StepStarted` (with the location, which reporters and the
  /// test-server protocol carry) and opens the mirrored trace span.
  fn open_step(
    &self,
    title: impl Into<String>,
    category: StepCategory,
    parent_step_id: Option<&str>,
    location: Option<StepLocation>,
    details: StepDetails,
  ) -> StepHandle {
    let title = title.into();
    let step_id = format!("{}@{}", category, STEP_ID_COUNTER.fetch_add(1, Ordering::Relaxed));

    if let Some(bus) = &self.event_bus {
      bus.emit(crate::reporter::ReporterEvent::StepStarted(Arc::new(
        crate::reporter::StepStartedEvent {
          test_id: self.test_id.clone(),
          project: self.project_name().unwrap_or_default(),
          step_id: step_id.clone(),
          parent_step_id: parent_step_id.map(ToString::to_string),
          title: title.clone(),
          category: category.clone(),
          location: location.clone(),
          subtitle: details.subtitle.clone(),
          params: details.params.clone(),
        },
      )));
    }

    let trace_span = if details.untraced {
      None
    } else {
      self.open_step_span(
        &step_id,
        &title,
        &category,
        parent_step_id,
        Duration::ZERO,
        location.as_ref(),
      )
    };
    let trace_prev_parent = match trace_span.as_ref() {
      Some(span) => TraceParentGuard::RestoreTo(span.make_current_parent()),
      None => TraceParentGuard::NotCurrent,
    };
    StepHandle {
      step_id,
      test_id: self.test_id.clone(),
      project: self.project_name().unwrap_or_default(),
      title,
      category,
      parent_step_id: parent_step_id.map(ToString::to_string),
      start: Instant::now(),
      started_at: SystemTime::now(),
      subtitle: details.subtitle,
      params: details.params,
      metadata: None,
      location,
      annotations: Vec::new(),
      event_bus: self.event_bus.clone(),
      steps: Arc::clone(&self.steps),
      held_in: None,
      trace_span,
      trace_prev_parent,
    }
  }

  /// Record a step that already executed elsewhere but still needs to flow
  /// through reporter events and the stored step tree.
  pub fn record_step(&self, step: RecordedStep) {
    let RecordedStep {
      title,
      category,
      status,
      duration,
      error,
      metadata,
    } = step;
    let step_id = format!("{}@{}", category, STEP_ID_COUNTER.fetch_add(1, Ordering::Relaxed));

    if let Some(bus) = &self.event_bus {
      bus.emit(crate::reporter::ReporterEvent::StepStarted(Arc::new(
        crate::reporter::StepStartedEvent {
          test_id: self.test_id.clone(),
          project: self.project_name().unwrap_or_default(),
          step_id: step_id.clone(),
          parent_step_id: None,
          title: title.clone(),
          category: category.clone(),
          location: None,
          subtitle: None,
          params: None,
        },
      )));
      bus.emit(crate::reporter::ReporterEvent::StepFinished(Arc::new(
        crate::reporter::StepFinishedEvent {
          test_id: self.test_id.clone(),
          project: self.project_name().unwrap_or_default(),
          step_id: step_id.clone(),
          title: title.clone(),
          category: category.clone(),
          duration,
          error: error.clone(),
          metadata: metadata.clone(),
          annotations: Vec::new(),
        },
      )));
    }

    if let Some(span) = self.open_step_span(&step_id, &title, &category, None, duration, None) {
      span.finish_message(error.clone());
    }

    lock(&self.steps).push(TestStep {
      step_id,
      title,
      category,
      duration,
      subtitle: None,
      params: None,
      start_time: SystemTime::now()
        .checked_sub(duration)
        .unwrap_or(SystemTime::UNIX_EPOCH),
      status,
      error,
      location: None,
      annotations: Vec::new(),
      parent_step_id: None,
      metadata,
      steps: Vec::new(),
    });
  }

  /// Mirror a starting step into the active per-test trace as an
  /// action span. `None` when the test's context is not being traced.
  /// `backdate` shifts the span's start time into the past (steps
  /// recorded after they already ran, [`Self::record_step`]); the
  /// location becomes the span's stack frame (viewer's Source tab).
  fn open_step_span(
    &self,
    step_id: &str,
    title: &str,
    category: &StepCategory,
    parent_step_id: Option<&str>,
    backdate: Duration,
    location: Option<&StepLocation>,
  ) -> Option<ferridriver::trace::ActionSpan> {
    let composite = self
      .trace_composite
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone()?;
    let parent_call = parent_step_id.and_then(|parent| {
      self
        .trace_step_calls
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(parent)
        .cloned()
    });
    let stack = location
      .map(|loc| {
        vec![ferridriver::trace::StackFrame {
          file: loc.file.clone(),
          line: loc.line,
          column: loc.column,
        }]
      })
      .unwrap_or_default();
    let span = ferridriver::trace::begin_custom_action(
      &composite,
      ferridriver::trace::CustomAction {
        class: "Test",
        method: trace_method_for_category(category),
        title: title.to_string(),
        params: serde_json::json!({}),
        parent_id: parent_call,
        // The reporter's step id, so a UI showing both the step tree and
        // the trace knows they are the same step.
        step_id: Some(step_id.to_string()),
        backdate_ms: backdate.as_secs_f64() * 1000.0,
        stack,
      },
    )?;
    self
      .trace_step_calls
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .insert(step_id.to_string(), span.call_id().to_string());
    Some(span)
  }
}

/// Trace `action.method` for a step category (the viewer's fallback
/// apiName is `class.method`; the explicit title wins for display).
fn trace_method_for_category(category: &StepCategory) -> &'static str {
  match category {
    StepCategory::TestStep | StepCategory::PwApi => "step",
    StepCategory::Expect => "expect",
    StepCategory::Fixture => "fixture",
    StepCategory::Hook => "hook",
  }
}

/// A step that is open right now.
pub struct OpenStep {
  handle: StepHandle,
  title: String,
  /// Frames an error raised inside it is re-attributed to, inherited by
  /// every step opened while it is open.
  boxed_stack: Vec<StepLocation>,
}

/// The steps of a test that are open right now.
#[derive(Default)]
pub struct OpenSteps {
  /// Steps that run a body (`test.step`, a fixture, `toPass`), outermost
  /// first. A step opened while one is open nests under the innermost.
  scoped: Vec<OpenStep>,
  /// Steps that run no code of the test's own (a browser call, a built-in
  /// matcher), so nothing can nest under them. Kept off the stack: two
  /// of them in flight at once (`Promise.all`) are siblings, and a stack
  /// would nest the second under the first.
  leaves: rustc_hash::FxHashMap<String, StepHandle>,
  /// Steps their opener holds a [`StepHandle`] to, by id and start, in
  /// the order they opened.
  held: Vec<(String, Instant)>,
}

impl OpenSteps {
  /// Where a step opened now belongs: the innermost open step that runs
  /// a body, or the latest held one if that opened after it.
  fn parent(&self) -> Option<String> {
    let scoped = self.scoped.last().map(|s| (&s.handle.step_id, s.handle.start));
    let held = self.held.last().map(|(id, start)| (id, *start));
    match (scoped, held) {
      (Some(scoped), Some(held)) if held.1 > scoped.1 => Some(held.0.clone()),
      (Some(scoped), _) => Some(scoped.0.clone()),
      (None, held) => held.map(|(id, _)| id.clone()),
    }
  }
}

fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
  mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl TestInfo {
  /// Open a step now. `nests` says whether steps opened while it is open
  /// belong under it: true for one that runs a body, false for a leaf
  /// (see `OpenSteps::leaves`). A leaf is not mirrored into the trace,
  /// which records the browser call or the assertion itself.
  ///
  /// The one place a step's parent, location and title path are decided,
  /// for every host and for the runner's own action observer.
  #[must_use]
  pub fn open_step_now(&self, spec: crate::step::StepSpec, nests: bool) -> crate::step::StepStarted {
    let frames: Vec<StepLocation> = spec
      .frames
      .iter()
      .filter_map(|f| match f {
        // Host coordinates only reach here unresolved when the host
        // has no source map for them; they name nothing on disk.
        crate::step::StepFrame::Host { .. } => None,
        crate::step::StepFrame::Source(loc) => Some(loc.clone()),
      })
      .collect();
    let mut open = lock(&self.open_steps);
    let parent_boxed = open.scoped.last().map(|s| s.boxed_stack.clone()).unwrap_or_default();
    let (location, boxed_stack) = crate::step::resolve_location(&spec.options, &frames, &parent_boxed);

    let mut title_path = self.title_path.clone();
    title_path.extend(open.scoped.iter().map(|s| s.title.clone()));
    title_path.push(spec.title.clone());

    let parent = open.parent();
    let mut handle = self.open_step(
      spec.title.clone(),
      spec.category,
      parent.as_deref(),
      location.clone(),
      StepDetails {
        subtitle: spec.options.subtitle.clone(),
        params: spec.options.params.clone(),
        untraced: !nests,
      },
    );
    handle.metadata.clone_from(&spec.metadata);
    let started = crate::step::StepStarted {
      step_id: handle.step_id.clone(),
      location,
      boxed_stack: boxed_stack.clone(),
      title_path,
    };
    if nests {
      open.scoped.push(OpenStep {
        handle,
        title: spec.title,
        boxed_stack,
      });
    } else {
      open.leaves.insert(started.step_id.clone(), handle);
    }
    started
  }

  /// Close a step [`Self::open_step_now`] opened. Closing one that is
  /// not open (already closed by [`Self::close_open_steps`]) does nothing.
  pub fn close_step_now(&self, step_id: &str, outcome: crate::step::StepOutcome) {
    let handle = {
      let mut open = lock(&self.open_steps);
      match open.scoped.iter().position(|s| s.handle.step_id == step_id) {
        Some(at) => Some(open.scoped.remove(at).handle),
        None => open.leaves.remove(step_id),
      }
    };
    let Some(mut handle) = handle else { return };
    handle.annotations = outcome.annotations;
    handle.finish_with_status(outcome.status, outcome.error);
  }
}

/// The runner is the step driver: `#[ferritest]` Rust tests reach it
/// directly, and both JS hosts reach it through the bridge, so one set
/// of rules decides every step's id, parent, location and title path.
impl crate::step::StepDriver for TestInfo {
  fn begin_step(&self, spec: crate::step::StepSpec) -> crate::step::StepFuture<'_, crate::step::StepStarted> {
    Box::pin(async move { self.open_step_now(spec, true) })
  }

  fn end_step(&self, step_id: String, outcome: crate::step::StepOutcome) -> crate::step::StepFuture<'_, ()> {
    Box::pin(async move { self.close_step_now(&step_id, outcome) })
  }
}

impl TestInfo {
  /// The project this test belongs to, by name.
  ///
  /// The worker carries the project as its MERGED config (that is what
  /// `merge_project` renames), so a plan that snapshotted the project
  /// entry is not the only source — `testInfo.project` and every
  /// `{projectName}` in a snapshot template read this.
  #[must_use]
  pub fn project_name(&self) -> Option<String> {
    self
      .project
      .as_ref()
      .map(|p| p.name.clone())
      .or_else(|| self.config_snapshot.as_ref().and_then(|c| c.name.clone()))
      .filter(|name| !name.is_empty())
  }

  /// What a template can name for THIS test.
  #[must_use]
  pub fn snapshot_context(&self) -> crate::snapshot_path::SnapshotPathContext {
    let expect = &self.expect;
    let config_dir = if self.config_dir.as_os_str().is_empty() {
      std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    } else {
      self.config_dir.clone()
    };
    crate::snapshot_path::SnapshotPathContext {
      // An unset `testDir` is the config's own directory, as
      // Playwright's `project.testDir` defaults — otherwise a spec's
      // path is relative to nothing and `{testFileDir}` renders the
      // absolute one.
      test_dir: if self.test_dir.as_os_str().is_empty() {
        config_dir.clone()
      } else {
        self.test_dir.clone()
      },
      config_dir,
      snapshot_dir: self.snapshot_dir.clone(),
      project_name: self.project_name().unwrap_or_default(),
      // Absolute, so `strip_prefix(testDir)` actually strips: a plan
      // file is relative to the working directory, a `testDir` is
      // anchored to its config file.
      test_file: {
        let file = PathBuf::from(&self.test_id.file);
        if file.is_absolute() {
          file
        } else {
          std::env::current_dir().unwrap_or_default().join(file)
        }
      },
      title_path: self.title_path.clone(),
      snapshot_suffix: self.snapshot_suffix.try_lock().map(|s| s.clone()).unwrap_or_default(),
      screenshot_template: expect.to_have_screenshot.path_template.clone(),
      aria_template: expect.to_match_aria_snapshot.path_template.clone(),
      config_template: self.snapshot_path_template.clone(),
    }
  }

  /// Where a snapshot of `kind` called `name` lives, and what to call
  /// the `-actual` / `-diff` files beside it.
  ///
  /// The ONE resolver: a matcher and `testInfo.snapshotPath()` must
  /// answer with the same string, which is the entire point of the
  /// template. `update_index` is Playwright's
  /// `'updateSnapshotIndex'` — a matcher consumes an index, a lookup
  /// only reads one.
  #[must_use]
  pub fn resolve_snapshot_paths(
    &self,
    kind: crate::snapshot_path::SnapshotKind,
    name: &crate::snapshot_path::SnapshotName,
    update_index: bool,
  ) -> crate::snapshot_path::ResolvedSnapshotPaths {
    self.resolve_snapshot_paths_with_extension(kind, name, update_index, None)
  }

  /// [`Self::resolve_snapshot_paths`] with the extension an unnamed
  /// snapshot takes from its value.
  pub fn resolve_snapshot_paths_with_extension(
    &self,
    kind: crate::snapshot_path::SnapshotKind,
    name: &crate::snapshot_path::SnapshotName,
    update_index: bool,
    anonymous_extension: Option<&str>,
  ) -> crate::snapshot_path::ResolvedSnapshotPaths {
    let counters = if kind == crate::snapshot_path::SnapshotKind::Aria {
      &self.aria_snapshot_names
    } else {
      &self.snapshot_names
    };
    let mut guard = counters.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    crate::snapshot_path::resolve_snapshot_paths(
      &self.snapshot_context(),
      kind,
      name,
      &mut guard,
      update_index,
      anonymous_extension,
    )
  }

  /// Close every step still open, so a body that failed mid-step leaves
  /// no dangling reporter event or trace span behind. Leaves go first: a
  /// browser call cut short by a timeout never reports its own end, and
  /// it ended before the step it ran in.
  pub fn close_open_steps(&self, reason: &str) {
    let (scoped, leaves) = {
      let mut open = lock(&self.open_steps);
      let scoped = std::mem::take(&mut open.scoped);
      let mut leaves: Vec<StepHandle> = open.leaves.drain().map(|(_, handle)| handle).collect();
      leaves.sort_by_key(|handle| handle.start);
      (scoped, leaves)
    };
    for handle in leaves {
      handle.finish_with_status(StepStatus::Failed, Some(reason.to_string()));
    }
    for step in scoped.into_iter().rev() {
      step
        .handle
        .finish_with_status(StepStatus::Failed, Some(reason.to_string()));
    }
  }
}

/// An already-executed step handed to [`TestInfo::record_step`]. Its
/// mirrored trace span is backdated by `duration` (the step ends at
/// report time).
pub struct RecordedStep {
  pub title: String,
  pub category: StepCategory,
  pub status: StepStatus,
  pub duration: Duration,
  pub error: Option<String>,
  pub metadata: Option<serde_json::Value>,
}

/// Global step ID counter for unique step identification.
static STEP_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Whether a step's trace span became the recorder's current parent,
/// and if so which parent id to restore when the step closes.
enum TraceParentGuard {
  /// The step never became the current parent (no trace span).
  NotCurrent,
  /// Restore this enclosing-parent id on close (stack discipline).
  RestoreTo(Option<String>),
}

/// What a step says about itself beyond its title, as
/// `test.step(…, { subtitle, params })` gives it.
#[derive(Debug, Clone, Default)]
struct StepDetails {
  subtitle: Option<String>,
  params: Option<serde_json::Value>,
  /// Keep the step out of the trace.
  untraced: bool,
}

/// Handle to an in-progress step. Must be completed via `end()`.
///
/// On `end()`:
/// - Emits `ReporterEvent::StepFinished` for real-time reporting
/// - Pushes a `TestStep` to the test's step list for batch reporting
pub struct StepHandle {
  pub step_id: String,
  pub test_id: TestId,
  /// Project the attempt belongs to — see [`crate::reporter::StepStartedEvent::project`].
  pub project: String,
  pub title: String,
  pub category: StepCategory,
  pub parent_step_id: Option<String>,
  pub start: Instant,
  /// Wall-clock twin of [`Self::start`], recorded into [`TestStep::start_time`].
  pub started_at: SystemTime,
  /// Recorded into [`TestStep::subtitle`].
  pub subtitle: Option<String>,
  /// Recorded into [`TestStep::params`].
  pub params: Option<serde_json::Value>,
  /// Arbitrary metadata attached to this step (set before calling `end()`).
  pub metadata: Option<serde_json::Value>,
  /// Source location recorded into [`TestStep::location`].
  pub location: Option<StepLocation>,
  /// Annotations recorded into [`TestStep::annotations`].
  pub annotations: Vec<TestAnnotation>,
  event_bus: Option<EventBus>,
  steps: Arc<std::sync::Mutex<Vec<TestStep>>>,
  /// The open-step registry that lists this step until it ends, for a
  /// step its opener holds.
  held_in: Option<Arc<std::sync::Mutex<OpenSteps>>>,
  /// Mirrored trace action, open while the step runs.
  trace_span: Option<ferridriver::trace::ActionSpan>,
  /// How to restore the recorder's enclosing parent when this step's
  /// span closes.
  trace_prev_parent: TraceParentGuard,
}

impl StepHandle {
  /// Complete this step. Pass `None` for success, `Some(msg)` for failure.
  pub fn end(self, error: Option<String>) {
    let status = if error.is_some() {
      StepStatus::Failed
    } else {
      StepStatus::Passed
    };
    self.finish_with_status(status, error);
  }

  /// Complete this step as skipped.
  pub fn skip(self, reason: Option<String>) {
    self.finish_with_status(StepStatus::Skipped, reason);
  }

  /// Complete this step as pending (not yet implemented).
  pub fn pending(self, reason: Option<String>) {
    self.finish_with_status(StepStatus::Pending, reason);
  }

  fn finish_with_status(self, status: StepStatus, error: Option<String>) {
    let duration = self.start.elapsed();
    if let Some(registry) = &self.held_in {
      lock(registry).held.retain(|(id, _)| *id != self.step_id);
    }
    if let Some(span) = self.trace_span {
      match self.trace_prev_parent {
        TraceParentGuard::RestoreTo(previous) => span.finish_message_restoring(error.clone(), previous),
        TraceParentGuard::NotCurrent => span.finish_message(error.clone()),
      }
    }

    if let Some(bus) = &self.event_bus {
      bus.emit(crate::reporter::ReporterEvent::StepFinished(Arc::new(
        crate::reporter::StepFinishedEvent {
          test_id: self.test_id.clone(),
          project: self.project.clone(),
          step_id: self.step_id.clone(),
          title: self.title.clone(),
          category: self.category.clone(),
          duration,
          error: error.clone(),
          metadata: self.metadata.clone(),
          annotations: self.annotations.clone(),
        },
      )));
    }

    let step = TestStep {
      step_id: self.step_id,
      title: self.title,
      category: self.category,
      duration,
      subtitle: self.subtitle,
      params: self.params,
      start_time: self.started_at,
      status,
      error,
      location: self.location,
      annotations: self.annotations,
      parent_step_id: self.parent_step_id,
      metadata: self.metadata,
      steps: Vec::new(),
    };
    lock(&self.steps).push(step);
  }
}
