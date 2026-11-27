//! Worker: owns a browser instance, executes hooks, creates fresh context+page per test.
//!
//! Hook execution model (matching Playwright):
//! - beforeAll: once per suite PER WORKER, tracked in `active_suites` map
//! - afterAll: when worker finishes, for every suite that had beforeAll run
//! - beforeEach: before every test, gets the test's fixture pool
//! - afterEach: after every test (even on failure), gets the test's fixture pool
//!
//! Serial batches: all tests run in order on this worker. On first failure, remaining
//! tests are skipped but afterAll still runs.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use rustc_hash::FxHashMap;
use tokio::sync::{Mutex, mpsc};

use crate::config::TestConfig;
use crate::dispatcher::{SerialBatch, TestAssignment, WorkItem};
use crate::fixture::{FixtureDef, FixturePool, FixtureScope};
use crate::model::{
  Attachment, AttachmentBody, ExpectedStatus, Hooks, StepCategory, TestAnnotation, TestFailure, TestInfo, TestOutcome,
  TestStatus,
};
use crate::reporter::{EventBus, ReporterEvent};

mod artifacts;
mod context_config;
mod resources;
#[cfg(test)]
mod tests;

pub(crate) use artifacts::test_artifact_dir_name;
use artifacts::{
  RawResult, absolutize, after_hooks_step, artifact_dir_name, capture_screenshot, describe_pending_calls,
  evaluate_condition, failure_attachments, judge, raw_result, record_failure_in_trace, redact_trace, run_caught,
  sanitize_filename, snapshot_dir, stop_video, take_page_snapshot, video_file, wants_page_snapshot,
  with_teardown_failure, write_error_context,
};
#[cfg(test)]
use context_config::{EffectiveContextConfig, build_context_options, config_viewport};
use context_config::{build_effective_context_config, build_suite_effective_context_config, core_http_credentials};
use resources::{
  TestBrowserResources, TraceSpec, WORKER_BROWSER_LAUNCH, build_request_def, build_suite_fixture_defs,
  build_test_fixture_defs, build_worker_browser_def,
};
pub(crate) use resources::{create_ready_page, is_retryable_bidi_page_error};

/// Result of a single test execution within a worker.
/// Whether the test that just ended may have left the worker's browser
/// unfit for the next one. A test that timed out may still be acting on
/// it, as Playwright assumes when it replaces the worker. A failure in a
/// browser with one context leaves that context, the only one the next
/// test can have, in whatever state the failure left it.
fn needs_fresh_browser(status: TestStatus, browser: &crate::runner::BrowserHandle) -> bool {
  match status {
    TestStatus::TimedOut => browser.try_get().is_some(),
    TestStatus::Failed => browser
      .try_get()
      .is_some_and(|browser| !browser.supports_isolated_contexts()),
    _ => false,
  }
}

/// Why a worker stopped taking work.
pub struct WorkerExit {
  /// An item taken from the queue and not run, for the next worker.
  pub pending: Option<WorkItem>,
  /// The worker's browser cannot be trusted with another test: one timed
  /// out on it, or failed in the only context it has. The next worker
  /// launches a fresh one.
  pub restart: bool,
}

pub struct WorkerTestResult {
  /// Shared with the reporter event that carried it: an outcome holds
  /// the attempt's screenshots and step tree, and copying it per
  /// consumer is the single largest allocation a finished test makes.
  pub outcome: Arc<TestOutcome>,
  pub should_retry: bool,
  /// The case as it was scheduled, so a retry runs with every setting it
  /// was declared with: timeout, retries, `test.fail()`, annotations, the
  /// `test.use` bag.
  pub test: crate::model::TestCase,
  pub suite_key: String,
  pub hooks: Arc<Hooks>,
  pub suite_mode: crate::model::SuiteMode,
  /// The tests of this test's serial group that never started because it
  /// failed and will be retried; they run after the retry, in order, on
  /// the same worker.
  pub serial_tail: Vec<TestAssignment>,
}

/// Per-suite state tracked on this worker.
struct SuiteState {
  repeat_each_index: u32,
  before_all_ran: bool,
  before_all_failed: bool,
  hooks: Arc<Hooks>,
  fixture_pool: FixturePool,
}

/// A worker that owns a browser and processes tests sequentially.
pub struct Worker {
  /// Unique across the whole run, including projects executing at the
  /// same time: it names this worker's scratch directory and is what a
  /// UI is told, so two workers sharing a number would share artifacts.
  pub id: u32,
  /// Which of this runner's worker slots this is (`0..workers`). The
  /// number a test sees as `parallelIndex`.
  pub slot: u32,
  config: Arc<TestConfig>,
  event_bus: Option<EventBus>,
  /// Flush trace events as they happen, because something is watching
  /// this run (a UI following the live trace). Off for a plain run: the
  /// trace is only read once it has been zipped, so buffering wins.
  live_traces: bool,
  /// Contexts pre-created for this worker's upcoming tests. Shared by
  /// every test the worker runs; drained when the worker exits.
  pool: Arc<crate::context_pool::ContextPool>,
}

impl Worker {
  #[must_use]
  pub fn new(id: u32, slot: u32, config: Arc<TestConfig>, event_bus: Option<EventBus>, live_traces: bool) -> Self {
    let pool = crate::context_pool::ContextPool::new(config.context_prewarm as usize);
    Self {
      id,
      slot,
      config,
      event_bus,
      live_traces,
      pool,
    }
  }

  /// The run facts every outcome this worker produces carries: which
  /// worker and slot ran it, under which project, when it started, and
  /// what it was declared to do. Sites fill the rest.
  fn outcome_base(&self, test: &crate::model::TestCase, started_at: SystemTime) -> TestOutcome {
    TestOutcome {
      project_name: self.config.name.clone().unwrap_or_default(),
      worker_index: self.id,
      parallel_index: self.slot,
      start_time: started_at,
      expected_status: test.expected_status,
      timeout: test
        .timeout
        .unwrap_or_else(|| Duration::from_millis(self.config.timeout)),
      metadata: self.config.metadata.clone(),
      case_metadata: test.metadata.clone(),
      ..Default::default()
    }
  }

  /// The `expect` block this worker's project resolved to. Every
  /// assertion a test makes — in a hook, in the body, in a fixture —
  /// defaults from it, which is why the whole test runs inside the
  /// scope rather than just the body.
  fn expect_config(&self) -> Arc<crate::config::ExpectConfig> {
    Arc::new(self.config.resolved_expect(None))
  }

  fn create_suite_test_info(&self, suite_key: &str, repeat_each_index: u32) -> Arc<TestInfo> {
    Arc::new(TestInfo {
      test_id: crate::model::TestId {
        root_path: None,
        repeat_each_index,
        file: suite_key.to_string(),
        suite: None,
        name: "suite hooks".to_string(),
        line: None,
        column: None,
      },
      title_path: vec![suite_key.to_string(), "suite hooks".to_string()],
      retry: 0,
      worker_index: self.id,
      parallel_index: self.slot,
      repeat_each_index,
      output_dir: absolutize(self.config.output_dir.join("__suite_hooks__").join(artifact_dir_name(
        suite_key.split("::").next().unwrap_or(suite_key),
        &suite_key.split("::").skip(1).collect::<Vec<_>>().join(" "),
        self.config.name.as_deref().unwrap_or_default(),
      ))),
      snapshot_dir: snapshot_dir(&self.config),
      snapshot_path_template: self.config.snapshot_path_template.clone(),
      update_snapshots: self.config.update_snapshots,
      ignore_snapshots: self.config.ignore_snapshots,
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
      config_snapshot: Some(Arc::clone(&self.config)),
      expect: Arc::new(self.config.resolved_expect(None)),
      config_dir: self.config.config_dir.clone().unwrap_or_default(),
      test_dir: self
        .config
        .test_dir
        .as_ref()
        .map_or_else(std::path::PathBuf::new, std::path::PathBuf::from),
      snapshot_names: Arc::new(std::sync::Mutex::new(crate::snapshot_path::SnapshotNames::default())),
      aria_snapshot_names: Arc::new(std::sync::Mutex::new(crate::snapshot_path::SnapshotNames::default())),
      timeout: Duration::from_millis(self.config.timeout),
      tags: Vec::new(),
      start_time: Instant::now(),
      event_bus: self.event_bus.clone(),
      annotations: Arc::new(Mutex::new(Vec::new())),
      trace_composite: Arc::new(std::sync::Mutex::new(None)),
      trace_step_calls: Arc::new(std::sync::Mutex::new(rustc_hash::FxHashMap::default())),
      open_steps: Arc::default(),
      output: std::sync::Arc::new(std::sync::Mutex::new(crate::model::TestOutput::default())),
    })
  }

  /// The worker's own fixtures, registered on the custom pool so child
  /// suite and test pools resolve them through the parent chain.
  fn worker_fixture_defs(&self, browser_handle: &Arc<crate::runner::BrowserHandle>) -> FxHashMap<String, FixtureDef> {
    // Register `browser` (worker scope) and `request` (test scope) on the
    // custom pool so child suite/test pools resolve them via the parent
    // chain. The backing `BrowserHandle` makes the browser launch lazy.
    // `request` is a new client per test, as Playwright's fixture is a
    // new `APIRequestContext` per test: one shared across the worker
    // carried each test's cookies into the next.
    let mut worker_defs: FxHashMap<String, FixtureDef> = FxHashMap::default();
    worker_defs.insert("browser".into(), build_worker_browser_def(Arc::clone(browser_handle)));
    worker_defs.insert(
      WORKER_BROWSER_LAUNCH.into(),
      build_worker_browser_def(Arc::clone(browser_handle)),
    );
    worker_defs.insert(
      "request".into(),
      build_request_def(
        self.config.base_url.clone().or_else(crate::config::run_base_url),
        core_http_credentials(&self.config.browser.use_options.http_credentials),
        self
          .config
          .browser
          .use_options
          .proxy
          .as_ref()
          .map(crate::config::ProxyConfig::to_engine),
      ),
    );
    worker_defs
  }

  /// Run `afterAll` for every suite whose `beforeAll` ran on this worker.
  async fn run_after_all(&self, active_suites: &FxHashMap<String, SuiteState>) {
    for (suite_key, state) in active_suites {
      if !state.before_all_ran {
        continue;
      }
      for (i, hook) in state.hooks.after_all.iter().enumerate() {
        let title = hook_title("afterAll", i, state.hooks.after_all.len());
        // afterAll has no test context: its steps go under a synthetic
        // test named after the suite.
        let step = SuiteHookStep {
          test_id: crate::model::TestId {
            root_path: None,
            repeat_each_index: state.repeat_each_index,
            file: suite_key.clone(),
            suite: None,
            name: title.clone(),
            line: None,
            column: None,
          },
          step_id: format!("hook:afterAll:{suite_key}:{i}"),
          title,
          describe: ToString::to_string,
        };
        if let Err(e) = self.run_suite_hook(hook, &state.fixture_pool, step).await {
          tracing::warn!(target: "ferridriver::worker", "afterAll error: {e}");
        }
      }
    }
  }

  /// Run work items from `rx` until it closes or `stop_flag` is set, then
  /// tear down every fixture pool and close the browser. Returns the item
  /// of the next `repeatEach` repetition, which this worker stopped at
  /// rather than run.
  ///
  /// # Errors
  ///
  /// Returns every fixture teardown and browser close failure, joined.
  #[tracing::instrument(skip_all, fields(worker_id = self.id))]
  pub async fn run(
    &self,
    browser_handle: Arc<crate::runner::BrowserHandle>,
    custom_fixture_pool: FixturePool,
    rx: async_channel::Receiver<WorkItem>,
    place: &crate::dispatcher::RosterPlace,
    result_tx: mpsc::Sender<WorkerTestResult>,
    stop_flag: Arc<std::sync::atomic::AtomicBool>,
    mut pending: Option<WorkItem>,
  ) -> ferridriver::Result<WorkerExit> {
    if let Some(event_bus) = &self.event_bus {
      event_bus.emit(ReporterEvent::WorkerStarted { worker_id: self.id });
    }

    let worker_defs = self.worker_fixture_defs(&browser_handle);
    let root_fixture_pool = custom_fixture_pool;
    let custom_fixture_pool = root_fixture_pool.child_with_defs(worker_defs, FixtureScope::Worker);

    let mut active_suites: FxHashMap<String, SuiteState> = FxHashMap::default();

    let mut repetition = None;
    let mut restart = false;
    while let Some(item) = match pending.take() {
      Some(item) => Some(item),
      None => place.next_or_leave(&rx),
    } {
      // `--max-failures` / `-x` flips this flag; drop any items that were
      // already buffered in the channel rather than processing them.
      if stop_flag.load(std::sync::atomic::Ordering::SeqCst) {
        break;
      }
      let index = item.repeat_each_index();
      if repetition.is_some_and(|previous| previous != index) {
        pending = Some(item);
        break;
      }
      repetition = Some(index);
      let locks = item.lock_names();
      let _held = crate::locks::acquire(locks.iter().map(String::as_str)).await;
      match item {
        WorkItem::Single(assignment) => {
          let result = ferridriver_expect::with_expect_config(
            self.expect_config(),
            Box::pin(self.run_single(&browser_handle, &custom_fixture_pool, &mut active_suites, *assignment)),
          )
          .await;
          restart = needs_fresh_browser(result.outcome.status, &browser_handle);
          if result_tx.send(result).await.is_err() || restart {
            break;
          }
        },
        WorkItem::Serial(batch) => {
          let results =
            Box::pin(self.run_serial_batch(&browser_handle, &custom_fixture_pool, &mut active_suites, batch)).await;
          for result in results {
            if result_tx.send(result).await.is_err() {
              break;
            }
          }
        },
      }
      // Yield so the runner can observe the just-sent result and trip the
      // stop flag (for `--max-failures` / `-x`) before this worker races
      // to pull the next item out of the buffered channel.
      tokio::task::yield_now().await;
    }

    // Run afterAll for every suite that had beforeAll on this worker.
    Box::pin(self.run_after_all(&active_suites)).await;

    let mut cleanup_errors = Vec::new();
    for state in active_suites.values() {
      if let Err(error) = state.fixture_pool.teardown_all().await {
        cleanup_errors.push(error.to_string());
      }
    }
    for pool in [&custom_fixture_pool, &root_fixture_pool] {
      if let Err(error) = pool.teardown_all().await {
        cleanup_errors.push(error.to_string());
      }
    }

    // Close contexts pre-created for tests that never arrived, before
    // the browser shuts down under them.
    self.pool.drain().await;

    // Graceful browser close — only fires when the worker actually
    // launched a browser via `BrowserHandle::get`. Tests that never
    // touched a browser-dependent fixture skip the close handshake
    // because no browser was launched in the first place.
    let cleanup = browser_handle.close().await;

    if let Some(event_bus) = &self.event_bus {
      event_bus.emit(ReporterEvent::WorkerFinished { worker_id: self.id });
    }
    if let Err(error) = cleanup {
      cleanup_errors.push(error.to_string());
    }
    if cleanup_errors.is_empty() {
      Ok(WorkerExit { pending, restart })
    } else {
      Err(ferridriver::FerriError::backend(cleanup_errors.join("; ")))
    }
  }

  /// Run a serial batch: all tests in order, skip rest on failure.
  async fn run_serial_batch(
    &self,
    browser: &Arc<crate::runner::BrowserHandle>,
    custom_pool: &FixturePool,
    active_suites: &mut FxHashMap<String, SuiteState>,
    batch: SerialBatch,
  ) -> Vec<WorkerTestResult> {
    let mut results = Vec::with_capacity(batch.assignments.len());
    let mut serial_failed = false;

    let mut assignments = batch.assignments.into_iter();
    while let Some(assignment) = assignments.next() {
      if serial_failed {
        // Skip remaining tests in the serial suite.
        let test = &assignment.test;
        let outcome = Arc::new(TestOutcome {
          test_id: test.id.clone(),
          status: TestStatus::Skipped,
          duration: Duration::ZERO,
          attempt: assignment.attempt,
          max_attempts: test.retries.unwrap_or(self.config.retries) + 1,
          error: Some(TestFailure {
            message: "skipped due to previous failure in serial suite".into(),
            stack: None,
            diff: None,
            screenshot: None,
            error_context: None,
          }),
          annotations: test.annotations.clone(),
          ..self.outcome_base(test, SystemTime::now())
        });
        if let Some(event_bus) = &self.event_bus {
          event_bus.emit(ReporterEvent::TestFinished {
            outcome: Arc::clone(&outcome),
          });
        }
        results.push(WorkerTestResult {
          outcome,
          should_retry: false,
          test: assignment.test.clone(),
          suite_key: assignment.suite_key,
          hooks: assignment.hooks,
          suite_mode: assignment.suite_mode,
          serial_tail: Vec::new(),
        });
        continue;
      }

      let mut result = ferridriver_expect::with_expect_config(
        self.expect_config(),
        Box::pin(self.run_single(browser, custom_pool, active_suites, assignment)),
      )
      .await;
      if result.should_retry {
        // The group resumes at the retry: what never started rides with
        // it instead of being reported skipped and then run anyway.
        result.serial_tail = assignments.by_ref().collect();
        results.push(result);
        break;
      }
      if result.outcome.status == TestStatus::Failed || result.outcome.status == TestStatus::TimedOut {
        serial_failed = true;
      }
      results.push(result);
    }

    results
  }

  /// Describe the live test for the debug hook.
  ///
  /// The context name is what makes a stop useful: a client that attaches
  /// to the bound browser without it lands on a fresh context and sees none
  /// of the state the test built. `None` when there is nothing to look at
  /// (no context yet, no browser launched) — stopping then would block the
  /// run for an empty page.
  async fn debug_test(
    browser: &Arc<crate::runner::BrowserHandle>,
    resources: &Arc<TestBrowserResources>,
    test_id: &crate::model::TestId,
    error: Option<String>,
  ) -> Option<crate::debug::DebugTest> {
    let context = resources.current_context().await?;
    Some(crate::debug::DebugTest {
      test: test_id.full_name(),
      location: test_id.line.map(|line| format!("{}:{line}", test_id.file)),
      error,
      browser: browser.peek()?,
      context: context.name().to_string(),
    })
  }

  /// Run a single test with full hook lifecycle.
  #[tracing::instrument(skip_all, fields(worker_id = self.id, test, attempt = assignment.attempt))]
  async fn run_single(
    &self,
    browser: &Arc<crate::runner::BrowserHandle>,
    custom_pool: &FixturePool,
    active_suites: &mut FxHashMap<String, SuiteState>,
    assignment: TestAssignment,
  ) -> WorkerTestResult {
    let test = &assignment.test;
    tracing::Span::current().record("test", test.id.full_name().as_str());
    let attempt = assignment.attempt;
    let max_attempts = test.retries.unwrap_or(self.config.retries) + 1;

    tracing::debug!(
      target: "ferridriver::worker",
      worker = self.id,
      test = test.id.full_name(),
      attempt,
      max_attempts,
      "dispatching test",
    );

    // If beforeAll failed, skip this test.
    if Box::pin(self.ensure_suite_ready(browser, custom_pool, active_suites, &assignment)).await {
      let outcome = TestOutcome {
        test_id: test.id.clone(),
        status: TestStatus::Skipped,
        duration: Duration::ZERO,
        attempt,
        max_attempts,
        error: Some(TestFailure {
          message: format!("skipped: beforeAll failed for suite '{}'", assignment.suite_key),
          stack: None,
          diff: None,
          screenshot: None,
          error_context: None,
        }),
        annotations: test.annotations.clone(),
        ..self.outcome_base(test, SystemTime::now())
      };
      return self.finish(outcome, false, assignment);
    }

    if should_skip(test, &self.config.browser) {
      let outcome = TestOutcome {
        test_id: test.id.clone(),
        status: TestStatus::Skipped,
        duration: Duration::ZERO,
        attempt,
        max_attempts,
        annotations: test.annotations.clone(),
        ..self.outcome_base(test, SystemTime::now())
      };
      return self.finish(outcome, false, assignment);
    }

    if let Some(event_bus) = &self.event_bus {
      event_bus.emit(ReporterEvent::TestStarted {
        test_id: test.id.clone(),
        project: self.config.name.clone().unwrap_or_default(),
        attempt,
        worker_id: self.id,
      });
    }

    let (outcome, should_retry) = Box::pin(self.run_attempt(browser, custom_pool, &assignment, max_attempts)).await;
    self.finish(outcome, should_retry, assignment)
  }

  /// Report a finished attempt and hand it back to the runner.
  fn finish(&self, outcome: TestOutcome, should_retry: bool, assignment: TestAssignment) -> WorkerTestResult {
    let outcome = Arc::new(outcome);
    if let Some(event_bus) = &self.event_bus {
      event_bus.emit(ReporterEvent::TestFinished {
        outcome: Arc::clone(&outcome),
      });
    }
    WorkerTestResult {
      outcome,
      should_retry,
      test: assignment.test,
      suite_key: assignment.suite_key,
      hooks: assignment.hooks,
      suite_mode: assignment.suite_mode,
      serial_tail: Vec::new(),
    }
  }

  /// Set up the test's suite on this worker the first time one of its
  /// tests arrives: its fixture pool, its worker-scope `auto` fixtures and
  /// its `beforeAll` hooks. Returns whether a `beforeAll` hook failed, in
  /// which case none of the suite's tests run.
  async fn ensure_suite_ready(
    &self,
    browser: &Arc<crate::runner::BrowserHandle>,
    custom_pool: &FixturePool,
    active_suites: &mut FxHashMap<String, SuiteState>,
    assignment: &TestAssignment,
  ) -> bool {
    let test_id = &assignment.test.id;
    let suite_key = &assignment.suite_key;
    let hooks = &assignment.hooks;
    let suite_state = active_suites.entry(suite_key.clone()).or_insert_with(|| {
      let suite_test_info = self.create_suite_test_info(suite_key, test_id.repeat_each_index);
      // Suite-hook contexts are not traced: per-test traces belong to
      // tests, and beforeAll/afterAll containers have no outcome to
      // attach one to.
      let suite_resources = Arc::new(TestBrowserResources::new(
        Arc::clone(browser),
        build_suite_effective_context_config(&self.config),
        suite_test_info.output_dir.clone(),
        None,
        Arc::clone(&self.pool),
        false,
      ));
      let suite_pool = custom_pool.child_with_defs(build_suite_fixture_defs(&suite_resources), FixtureScope::Worker);
      suite_pool.inject("test_info", suite_test_info);

      SuiteState {
        repeat_each_index: test_id.repeat_each_index,
        before_all_ran: false,
        before_all_failed: false,
        hooks: Arc::clone(hooks),
        fixture_pool: suite_pool,
      }
    });

    // Worker-scope `auto: true` fixtures resolve once before beforeAll runs.
    for name in suite_state.fixture_pool.auto_fixture_names_for(FixtureScope::Worker) {
      if let Err(e) = suite_state.fixture_pool.resolve(&name).await {
        tracing::warn!(target: "ferridriver::worker", "auto fixture '{name}' (suite) failed: {e}");
      }
    }

    if !suite_state.before_all_ran && !hooks.before_all.is_empty() {
      for (i, hook) in hooks.before_all.iter().enumerate() {
        let step = SuiteHookStep {
          test_id: test_id.clone(),
          step_id: format!("hook:beforeAll:{suite_key}:{i}"),
          title: hook_title("beforeAll", i, hooks.before_all.len()),
          describe: |failure| failure.message.clone(),
        };
        let result = self.run_suite_hook(hook, &suite_state.fixture_pool, step).await;
        if let Err(e) = result {
          tracing::error!(target: "ferridriver::worker", "beforeAll failed for {suite_key}: {e}");
          suite_state.before_all_failed = true;
          break;
        }
      }
      suite_state.before_all_ran = true;
    }
    suite_state.before_all_failed
  }

  /// Run one `beforeAll` / `afterAll` hook, reported as a step of
  /// `step.test_id`.
  async fn run_suite_hook(
    &self,
    hook: &crate::model::SuiteHookFn,
    pool: &FixturePool,
    step: SuiteHookStep,
  ) -> Result<(), TestFailure> {
    if let Some(event_bus) = &self.event_bus {
      event_bus.emit(ReporterEvent::StepStarted(Arc::new(
        crate::reporter::StepStartedEvent {
          project: self.config.name.clone().unwrap_or_default(),
          test_id: step.test_id.clone(),
          step_id: step.step_id.clone(),
          parent_step_id: None,
          title: step.title.clone(),
          category: StepCategory::Hook,
          location: None,
          subtitle: None,
          params: None,
        },
      )));
    }
    let start = Instant::now();
    let result = run_caught(hook(pool.clone())).await;
    let duration = start.elapsed();
    if let Some(event_bus) = &self.event_bus {
      event_bus.emit(ReporterEvent::StepFinished(Arc::new(
        crate::reporter::StepFinishedEvent {
          project: self.config.name.clone().unwrap_or_default(),
          test_id: step.test_id,
          step_id: step.step_id,
          title: step.title,
          category: StepCategory::Hook,
          duration,
          error: result.as_ref().err().map(step.describe),
          metadata: None,
          annotations: Vec::new(),
        },
      )));
    }
    result
  }

  /// The test's own `TestInfo` for this attempt.
  fn new_test_info(
    &self,
    test: &crate::model::TestCase,
    attempt: u32,
    timeout: Duration,
    start: Instant,
    trace_composite: &Arc<std::sync::Mutex<Option<String>>>,
  ) -> Arc<TestInfo> {
    let test_id = &test.id;
    Arc::new(TestInfo {
      test_id: test_id.clone(),
      title_path: test_id.title_path(),
      retry: attempt.saturating_sub(1),
      worker_index: self.id,
      parallel_index: self.slot,
      repeat_each_index: test_id.repeat_each_index,
      output_dir: absolutize(self.config.output_dir.join(test_artifact_dir_name(
        test_id,
        self.config.name.as_deref().unwrap_or_default(),
      ))),
      snapshot_dir: snapshot_dir(&self.config),
      snapshot_path_template: self.config.snapshot_path_template.clone(),
      update_snapshots: self.config.update_snapshots,
      ignore_snapshots: self.config.ignore_snapshots,
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
      config_snapshot: Some(Arc::clone(&self.config)),
      expect: Arc::new(self.config.resolved_expect(None)),
      config_dir: self.config.config_dir.clone().unwrap_or_default(),
      test_dir: self
        .config
        .test_dir
        .as_ref()
        .map_or_else(std::path::PathBuf::new, std::path::PathBuf::from),
      snapshot_names: Arc::new(std::sync::Mutex::new(crate::snapshot_path::SnapshotNames::default())),
      aria_snapshot_names: Arc::new(std::sync::Mutex::new(crate::snapshot_path::SnapshotNames::default())),
      timeout,
      tags: test
        .annotations
        .iter()
        .filter_map(|a| match a {
          TestAnnotation::Tag(t) => Some(t.clone()),
          _ => None,
        })
        .collect(),
      start_time: start,
      event_bus: self.event_bus.clone(),
      annotations: Arc::new(Mutex::new(Vec::new())),
      trace_composite: Arc::clone(trace_composite),
      trace_step_calls: Arc::new(std::sync::Mutex::new(rustc_hash::FxHashMap::default())),
      open_steps: Arc::default(),
      output: std::sync::Arc::new(std::sync::Mutex::new(crate::model::TestOutput::default())),
    })
  }

  /// Everything one attempt runs against: its `TestInfo`, its browser
  /// resources and the fixture pool built on them.
  fn prepare_attempt(
    &self,
    browser: &Arc<crate::runner::BrowserHandle>,
    custom_pool: &FixturePool,
    test: &crate::model::TestCase,
    attempt: u32,
    timeout: Duration,
    start: Instant,
  ) -> AttemptRun {
    let test_id = &test.id;
    let trace_composite: Arc<std::sync::Mutex<Option<String>>> = Arc::new(std::sync::Mutex::new(None));
    let test_info = self.new_test_info(test, attempt, timeout, start, &trace_composite);
    let trace_spec = self.config.trace.should_record(attempt, false).then(|| TraceSpec {
      key: test_id.execution_key(),
      title: test_id.full_name(),
      // The stream is named after the test, in this worker's artifacts
      // directory: that is the whole contract a viewer following a
      // running test relies on — it asks for `<testId>.json` there.
      name: test_id.stable_id(self.config.name.as_deref().unwrap_or_default()),
      traces_dir: crate::artifacts::traces_dir(&self.config.output_dir, self.id as usize),
      live: self.live_traces,
      capture: self.config.browser.use_options.trace.clone().unwrap_or_default(),
      composite: Arc::clone(&trace_composite),
    });
    let resources = Arc::new(TestBrowserResources::new(
      Arc::clone(browser),
      build_effective_context_config(&self.config, test),
      test_info.output_dir.clone(),
      trace_spec,
      Arc::clone(&self.pool),
      test.fixture_requests.iter().any(|f| f == "page"),
    ));
    let test_pool = custom_pool.child_with_defs(build_test_fixture_defs(&resources), FixtureScope::Test);
    test_pool.inject("test_info", Arc::clone(&test_info));
    let modifiers = Arc::new(crate::TestModifiers::default());
    modifiers.timeout_updates.send_replace(timeout);
    test_pool.inject("__test_modifiers", Arc::clone(&modifiers));
    AttemptRun {
      call_owner: Arc::from(test_info.call_owner()),
      test_info,
      resources,
      test_pool,
      modifiers,
      trace_composite,
    }
  }

  /// One attempt of the test, from fixture setup to the outcome, and
  /// whether it should be retried.
  async fn run_attempt(
    &self,
    browser: &Arc<crate::runner::BrowserHandle>,
    custom_pool: &FixturePool,
    assignment: &TestAssignment,
    max_attempts: u32,
  ) -> (TestOutcome, bool) {
    let test = &assignment.test;
    let test_id = &test.id;
    let attempt = assignment.attempt;
    let hooks = &assignment.hooks;
    let start = Instant::now();
    let started_at = SystemTime::now();
    let run = self.prepare_attempt(browser, custom_pool, test, attempt, self.timeout_for(test), start);

    let setup = match Box::pin(self.set_up_attempt(&run, test, attempt, &hooks.before_each)).await {
      Ok(setup) => setup,
      Err(e) => {
        let () = run.resources.close().await;
        let failure = TestFailure::wrap("failed to create page", &e);
        let outcome = TestOutcome {
          test_id: test_id.clone(),
          status: TestStatus::Failed,
          duration: start.elapsed(),
          attempt,
          max_attempts,
          errors: vec![failure.clone()],
          error: Some(failure),
          annotations: test.annotations.clone(),
          ..self.outcome_base(test, started_at)
        };
        return (outcome, attempt < max_attempts);
      },
    };
    let AttemptSetup {
      budget,
      budget_started,
      before_each_err,
      timed_out_while,
      mut page_for_artifacts,
      video_handle,
      api_steps: _api_steps,
    } = setup;

    let debug_hook = crate::debug::debug_hook();
    if let Some(hook) = &debug_hook {
      Self::debug_before_body(hook, browser, &run.resources, test_id).await;
    }

    let timeout_result = if timed_out_while.is_some() {
      Err(ferridriver::pause::Timedout)
    } else if let Some(err) = before_each_err {
      Ok(Err(err))
    } else {
      // Soft assertions raised anywhere in the body land on this test's
      // own collector and fail it at the end, instead of stopping here.
      ferridriver::pause::run_within_updates_from(
        budget,
        run.modifiers.timeout_updates.subscribe(),
        run.in_test_scope(run_caught((test.test_fn)(run.test_pool.clone()))),
      )
      .await
    };
    let timeout_dur = *run.modifiers.timeout_updates.borrow();
    // Read before teardown: closing the context fails the stuck call, which
    // then finishes and leaves the registry.
    let interrupted = if timeout_result.is_err() {
      ferridriver::trace::pending_calls(&run.call_owner, budget_started + timeout_dur)
    } else {
      Vec::new()
    };
    ferridriver::trace::forget_pending_calls(&run.call_owner);

    if let Some(hook) = &debug_hook {
      Self::debug_after_failure(hook, browser, &run.resources, test_id, &timeout_result, timeout_dur).await;
    }

    let after = AfterBudget::new(&self.config, timeout_dur);
    let mut teardown_stuck = Box::pin(run.run_after_each(&hooks.after_each, &after)).await;

    // Release the debugger's hold before the artifacts are collected: the
    // session it published points at this test's context, and the next
    // test must not inherit either it or the gate it armed.
    if let Some(hook) = &debug_hook {
      hook.test_finished().await;
    }

    if page_for_artifacts.is_none() {
      page_for_artifacts = run.test_pool.try_get_cached::<ferridriver::Page>("page");
    }
    let test_failed =
      timeout_result.as_ref().is_err() || timeout_result.as_ref().is_ok_and(std::result::Result::is_err);
    let ended = AttemptEnd {
      attempt,
      test_failed,
      timeout_result,
      timeout_dur,
      timed_out_while,
    };
    let artifacts = Box::pin(self.collect_artifacts(
      &run,
      &after,
      &mut teardown_stuck,
      page_for_artifacts.as_deref(),
      video_handle,
      &ended,
    ))
    .await;
    ferridriver::trace::forget_pending_calls(&run.call_owner);
    let settled = Settled {
      start,
      started_at,
      max_attempts,
      interrupted,
      artifacts,
      teardown_failure: teardown_stuck.map(|phase| after.exceeded_while(phase)),
    };
    Box::pin(self.settle_outcome(test, &run, ended, settled)).await
  }

  /// `--debug`: the body has not started and the context is live, which is
  /// the point Playwright publishes a test from
  /// (`runAfterCreateBrowserContext`). The hook arms rather than blocks —
  /// the first API call is where the run actually stops.
  ///
  /// The context is normally created by the `page` fixture, i.e. inside
  /// the body — too late for a client to attach before the first call.
  /// Debugging is the one mode that pays for creating it up front.
  async fn debug_before_body(
    hook: &Arc<dyn crate::debug::DebugHook>,
    browser: &Arc<crate::runner::BrowserHandle>,
    resources: &Arc<TestBrowserResources>,
    test_id: &crate::model::TestId,
  ) {
    if let Err(e) = resources.context().await {
      tracing::warn!(target: "ferridriver::worker", "--debug: no context to publish: {e}");
    }
    if let Some(live) = Self::debug_test(browser, resources, test_id, None).await {
      hook.test_starting(live).await;
    }
  }

  /// Hold here, before `afterEach` and before the context closes, so
  /// whoever attaches sees the page the failure left rather than its
  /// wreckage. Only on a failure: a passing test has nothing to look at.
  async fn debug_after_failure(
    hook: &Arc<dyn crate::debug::DebugHook>,
    browser: &Arc<crate::runner::BrowserHandle>,
    resources: &Arc<TestBrowserResources>,
    test_id: &crate::model::TestId,
    timeout_result: &Result<Result<(), TestFailure>, ferridriver::pause::Timedout>,
    timeout_dur: Duration,
  ) {
    let failure = match timeout_result {
      Ok(Err(e)) => Some(e.message.clone()),
      Err(_) => Some(format!("test timed out after {}ms", timeout_dur.as_millis())),
      Ok(Ok(())) => None,
    };
    if let Some(error) = failure
      && let Some(live) = Self::debug_test(browser, resources, test_id, Some(error)).await
    {
      hook.test_failed(live).await;
    }
  }

  /// Set up what the attempt's body runs with: the worker-scope fixtures
  /// outside the test's budget, then the test's budget, its `auto`
  /// fixtures, its page when the video is recorded, and its `beforeEach`
  /// hooks.
  ///
  /// # Errors
  ///
  /// Returns the failure to create the page the video records, which
  /// fails the attempt.
  async fn set_up_attempt(
    &self,
    run: &AttemptRun,
    test: &crate::model::TestCase,
    attempt: u32,
    before_each: &[crate::model::HookFn],
  ) -> Result<AttemptSetup, ferridriver::FerriError> {
    // Worker-scoped fixtures (the browser) are set up outside the test's
    // budget, as Playwright gives them a slot of their own.
    let mut before_each_err = run
      .test_pool
      .resolve_worker_dependencies(&test.fixture_requests)
      .await
      .err()
      .map(TestFailure::from);

    // Test-scoped fixture setup, beforeEach and the body then spend one
    // budget, as they do in Playwright (`workerMain.ts` runs them in the
    // test's default slot).
    let budget = ferridriver::pause::BudgetStart::now();
    let budget_started = Instant::now();
    // Calls are filed under the test that made them, so a timeout can name
    // the one it interrupted, on whichever browser it ran.
    ferridriver::trace::track_pending_calls();
    // The same identity reports each of those calls as a `pw:api` step.
    let api_steps = crate::api_steps::observe(&run.test_info);
    let mut timed_out_while: Option<&'static str> = None;
    if before_each_err.is_none() {
      match run.set_up_auto_fixtures(budget).await {
        Ok(result) => before_each_err = result.err().map(TestFailure::from),
        Err(ferridriver::pause::Timedout) => timed_out_while = Some("setting up fixtures"),
      }
    }

    let mut page_for_artifacts = None;
    let mut video_handle = None;
    if before_each_err.is_none() && timed_out_while.is_none() && self.config.video.mode.should_record(attempt) {
      match Box::pin(self.start_video(run, budget, attempt)).await {
        VideoStart::TimedOut => timed_out_while = Some("setting up fixtures"),
        VideoStart::Started { page, handle } => {
          page_for_artifacts = Some(page);
          video_handle = handle;
        },
        VideoStart::PageFailed(e) => return Err(e),
      }
    }

    if before_each_err.is_none() && timed_out_while.is_none() {
      match Box::pin(run.run_before_each(before_each, budget)).await {
        Ok(Ok(())) => {},
        Ok(Err(e)) => before_each_err = Some(e),
        Err(ferridriver::pause::Timedout) => timed_out_while = Some("running beforeEach hooks"),
      }
    }
    Ok(AttemptSetup {
      budget,
      budget_started,
      before_each_err,
      timed_out_while,
      page_for_artifacts,
      video_handle,
      api_steps,
    })
  }

  /// The test's timeout: its own or the config's, tripled when a `slow`
  /// annotation applies.
  fn timeout_for(&self, test: &crate::model::TestCase) -> Duration {
    let timeout = test
      .timeout
      .unwrap_or_else(|| Duration::from_millis(self.config.timeout));
    let is_slow = test.annotations.iter().any(|a| match a {
      TestAnnotation::Slow { condition: None, .. } => true,
      TestAnnotation::Slow {
        condition: Some(cond), ..
      } => evaluate_condition(cond, &self.config.browser),
      _ => false,
    });
    if is_slow { timeout * 3 } else { timeout }
  }

  /// Open the `page` for the video recording and start recording it, as
  /// the mode asks: eagerly when the recording is kept even on a pass,
  /// buffered when only a failure keeps it.
  async fn start_video(&self, run: &AttemptRun, budget: ferridriver::pause::BudgetStart, attempt: u32) -> VideoStart {
    let page = match ferridriver::pause::run_within_updates_from(
      budget,
      run.modifiers.timeout_updates.subscribe(),
      run.test_pool.get::<ferridriver::Page>("page"),
    )
    .await
    {
      Err(ferridriver::pause::Timedout) => return VideoStart::TimedOut,
      Ok(Err(e)) => return VideoStart::PageFailed(e),
      Ok(Ok(page)) => page,
    };
    let test_info = &run.test_info;
    let _ = std::fs::create_dir_all(&test_info.output_dir);
    let (video_width, video_height) = self.config.video.frame_size(page.viewport_size());
    let started = if self.config.video.mode.records_eagerly(attempt) {
      ferridriver::video::start_recording(&page, video_file(test_info, attempt), video_width, video_height, 80)
        .await
        .map(VideoHandle::Eager)
    } else {
      ferridriver::video::start_buffered_recording(&page, video_width, video_height, 80)
        .await
        .map(VideoHandle::Buffered)
    };
    let handle = match started {
      Ok(handle) => Some(handle),
      Err(e) => {
        tracing::warn!(target: "ferridriver::worker", "video start failed: {e}");
        None
      },
    };
    VideoStart::Started { page, handle }
  }

  /// Capture what the attempt leaves behind on the after-hooks budget:
  /// the failure screenshot, the video, the trace (with the failure and
  /// the screenshot recorded into it) and the page snapshot, then close
  /// the context.
  async fn collect_artifacts(
    &self,
    run: &AttemptRun,
    after: &AfterBudget,
    teardown_stuck: &mut Option<&'static str>,
    page: Option<&ferridriver::Page>,
    video_handle: Option<VideoHandle>,
    ended: &AttemptEnd,
  ) -> Artifacts {
    let screenshot = match page {
      Some(page)
        if self
          .config
          .screenshot
          .mode
          .should_capture(ended.test_failed, ended.attempt) =>
      {
        after_hooks_step(
          after.start,
          &after.updates,
          teardown_stuck,
          "capturing the failure screenshot",
          capture_screenshot(page, &self.config.screenshot),
        )
        .await
        .flatten()
      },
      _ => None,
    };
    let video = Box::pin(stop_video(video_handle, &run.test_info, ended));
    let video_path = after_hooks_step(after.start, &after.updates, teardown_stuck, "saving the video", video)
      .await
      .flatten();
    record_failure_in_trace(run, ended, screenshot.as_deref());
    let trace_path = Box::pin(self.stop_trace(run, after, teardown_stuck, ended)).await;
    // Playwright's `_takePageSnapshot`: an `expect` failure brings the
    // tree it judged; any other failure gets the page as it was left.
    let page_snapshot = if wants_page_snapshot(&ended.timeout_result, &run.test_info) && teardown_stuck.is_none() {
      Box::pin(after_hooks_step(
        after.start,
        &after.updates,
        teardown_stuck,
        "capturing the page snapshot",
        take_page_snapshot(run.resources.current_context().await),
      ))
      .await
      .flatten()
    } else {
      None
    };
    // Out of budget, the context is left to the browser's own shutdown:
    // closing it is the step most likely to wait on whatever wedged.
    Box::pin(after_hooks_step(
      after.start,
      &after.updates,
      teardown_stuck,
      "closing the browser context",
      run.resources.close(),
    ))
    .await;
    Artifacts {
      screenshot,
      video_path,
      trace_path,
      page_snapshot,
    }
  }

  /// Stop the per-test trace while the context is still alive: export
  /// to disk when the mode retains it, discard otherwise. Retention
  /// keys off the RAW body result (same signal as screenshot-on-failure
  /// and buffered video): an expected-failure test that fails as
  /// expected still retains its trace under retain-on-failure — the
  /// trace shows the deliberate failure.
  async fn stop_trace(
    &self,
    run: &AttemptRun,
    after: &AfterBudget,
    teardown_stuck: &mut Option<&'static str>,
    ended: &AttemptEnd,
  ) -> Option<std::path::PathBuf> {
    let test_info = &run.test_info;
    let started = run
      .trace_composite
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .take()
      .is_some();
    // Stop the UI live-trace poller from exporting a recorder that is
    // about to be torn down (the finished zip takes over at finish).
    if started {
      crate::ui_server::unregister_live_trace(&test_info.test_id.execution_key());
    }
    let ctx = match (started, run.resources.current_context().await) {
      (true, Some(ctx)) if teardown_stuck.is_none() => ctx,
      _ => return None,
    };
    let path = self
      .config
      .trace
      .should_write(ended.attempt, ended.test_failed)
      .then(|| {
        let _ = std::fs::create_dir_all(&test_info.output_dir);
        test_info.output_dir.join(format!(
          "{}-attempt{}.trace.zip",
          sanitize_filename(&test_info.test_id.name),
          ended.attempt
        ))
      });
    let tracing = ctx.tracing();
    let stop = std::future::IntoFuture::into_future(
      tracing
        .stop()
        .options(ferridriver::trace::TracingStopOptions { path: path.clone() }),
    );
    match Box::pin(after_hooks_step(
      after.start,
      &after.updates,
      teardown_stuck,
      "saving the trace",
      stop,
    ))
    .await
    {
      Some(Ok(())) => {
        if let Some(path) = &path {
          redact_trace(&self.config.secrets, path);
        }
        path
      },
      Some(Err(e)) => {
        tracing::warn!(target: "ferridriver::worker", "trace stop failed: {e}");
        None
      },
      None => None,
    }
  }

  /// The attempt's outcome from what it ended with and what it left, and
  /// whether to retry it.
  async fn settle_outcome(
    &self,
    test: &crate::model::TestCase,
    run: &AttemptRun,
    ended: AttemptEnd,
    settled: Settled,
  ) -> (TestOutcome, bool) {
    let test_info = &run.test_info;
    let test_id = &test.id;
    let AttemptEnd {
      attempt,
      test_failed,
      timeout_result,
      timeout_dur,
      timed_out_while,
    } = ended;
    let Settled {
      start,
      started_at,
      max_attempts,
      interrupted,
      artifacts,
      teardown_failure,
    } = settled;
    let Artifacts {
      screenshot,
      video_path,
      trace_path,
      page_snapshot,
    } = artifacts;
    let duration = start.elapsed();

    let attachments = failure_attachments(test_info, attempt, screenshot.as_deref(), trace_path);

    let raw = raw_result(
      timeout_result,
      screenshot,
      timeout_dur,
      timed_out_while,
      &describe_pending_calls(&interrupted, &test_info.config_dir),
    );
    let (raw_status, raw_error) = match raw {
      RawResult::Ended(status, error) => (status, error),
      RawResult::SkippedAtRuntime => {
        let mut annotations = test.annotations.clone();
        annotations.extend(test_info.get_annotations().await);
        let outcome = TestOutcome {
          test_id: test_id.clone(),
          status: TestStatus::Skipped,
          duration: start.elapsed(),
          attempt,
          max_attempts,
          annotations,
          ..self.outcome_base(test, started_at)
        };
        return (outcome, false);
      },
    };

    let expected_status = self.effective_expected_status(test, run).await;

    // Soft assertions are part of WHAT HAPPENED, so they settle before
    // `test.fail()` is consulted: a test that only failed softly did
    // fail, and `test.fail()` must be able to expect that.
    let soft_errors = test_info.drain_soft_errors();
    let error_context = write_error_context(
      test_info,
      attempt,
      raw_error.iter().chain(&soft_errors).collect::<Vec<_>>().as_slice(),
      page_snapshot.as_deref(),
      &ferridriver_trace::redact::Redactor::new(self.config.secrets.iter().cloned()),
    );
    let (status, error) = judge(raw_status, raw_error, &soft_errors, expected_status);

    let mut recorded = self
      .take_recorded(test_info, attachments, video_path.as_deref(), test_failed, attempt)
      .await;
    recorded.attachments.extend(error_context);

    // Merge compile-time annotations with runtime annotations.
    let mut annotations = test.annotations.clone();
    annotations.extend(test_info.get_annotations().await);

    // What the test printed belongs to the test, not to the process:
    // reporters, the HTML report and the UI's terminal read it here.
    let output = test_info
      .output
      .lock()
      .map(|mut held| std::mem::take(&mut *held))
      .unwrap_or_default();

    // `errors` leads with the hard failure and then the soft assertions
    // the test collected: reporters that show every error read it, and
    // `error` stays the first for the ones that show one.
    let mut errors: Vec<TestFailure> = error.iter().cloned().collect();
    errors.extend(soft_errors);
    let (status, error) = with_teardown_failure(status, error, &mut errors, teardown_failure);
    let outcome = TestOutcome {
      test_id: test_id.clone(),
      status,
      duration,
      attempt,
      max_attempts,
      error,
      errors,
      attachments: recorded.attachments,
      snapshots: recorded.snapshots,
      steps: recorded.steps,
      stdout: output.stdout,
      stderr: output.stderr,
      annotations,
      // The effective expectation, not the declared one: `test.fail()`
      // called inside the body arms after the base was built.
      expected_status,
      ..self.outcome_base(test, started_at)
    };

    let should_retry = crate::model::outcome_kind(&[outcome.status], outcome.expected_status)
      == crate::model::TestOutcomeKind::Unexpected
      && attempt < max_attempts
      && !test_info.non_retriable.load(std::sync::atomic::Ordering::Relaxed);
    (outcome, should_retry)
  }

  /// The expectation the attempt is judged against: the declared one, a
  /// `fail` annotation that applies, or `test.fail()` called at runtime.
  async fn effective_expected_status(&self, test: &crate::model::TestCase, run: &AttemptRun) -> ExpectedStatus {
    let mut expected_status = expected_status_for(test, &self.config.browser);
    // Read runtime modifiers set by test body (via NAPI TestInfo.skip/fail/slow/setTimeout).
    // These are injected into the fixture pool by the NAPI test_fn closure.
    if let Ok(modifiers) = run.test_pool.get::<crate::TestModifiers>("__test_modifiers").await {
      if modifiers.expected_failure.load(std::sync::atomic::Ordering::Relaxed) {
        expected_status = ExpectedStatus::Fail;
      }
      // timeout_override: already elapsed for this attempt, but log for debugging.
      if let Ok(guard) = modifiers.timeout_override.lock()
        && let Some(ms) = *guard
      {
        tracing::debug!(target: "ferridriver::worker", "test.setTimeout({ms}ms) called at runtime");
      }
    }
    expected_status
  }

  /// Take what the test recorded on its `TestInfo`: its steps, its
  /// attachments after `attachments`, its snapshots, and the video when
  /// the mode keeps it.
  async fn take_recorded(
    &self,
    test_info: &TestInfo,
    mut attachments: Vec<Attachment>,
    video_path: Option<&std::path::Path>,
    test_failed: bool,
    attempt: u32,
  ) -> Recorded {
    // A browser call a timeout cut short never reports its own end.
    test_info.close_open_steps("step never completed (test aborted)");
    // Collect tracked test steps and attachments.
    let steps = crate::model::nest_steps(
      test_info
        .steps
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone(),
    );
    let info_attachments = test_info.attachments.lock().await.clone();
    attachments.extend(info_attachments);
    attachments.extend(
      test_info
        .snapshot_attachments
        .lock()
        .map(|mut held| std::mem::take(&mut *held))
        .unwrap_or_default(),
    );
    let snapshots = test_info
      .snapshots
      .lock()
      .map(|mut held| std::mem::take(&mut *held))
      .unwrap_or_default();

    // Attach or clean up video recording.
    // For buffered mode, video_path is only Some when the test failed (already filtered).
    // For eager mode, we keep or delete based on the mode.
    if let Some(path) = video_path {
      // The buffered path only produced a file when the mode's retain
      // rule already said yes, so asking again is the same answer.
      let keep = self.config.video.mode.should_retain(test_failed, attempt);
      if keep && path.exists() {
        attachments.push(Attachment {
          name: "video".into(),
          content_type: ferridriver::video::video_content_type_of(path).into(),
          body: AttachmentBody::Path(path.to_path_buf()),
          step_id: None,
        });
      } else {
        let _ = std::fs::remove_file(path);
      }
    }
    Recorded {
      steps,
      attachments,
      snapshots,
    }
  }
}

/// Everything one attempt of a test runs against.
struct AttemptRun {
  test_info: Arc<TestInfo>,
  resources: Arc<TestBrowserResources>,
  test_pool: FixturePool,
  modifiers: Arc<crate::TestModifiers>,
  /// The identity the attempt's browser calls are filed under.
  call_owner: Arc<str>,
  trace_composite: Arc<std::sync::Mutex<Option<String>>>,
}

impl AttemptRun {
  /// `fut` as the test's own code runs: its calls filed under the test,
  /// its soft assertions collected on it.
  fn in_test_scope<F: std::future::Future>(&self, fut: F) -> impl std::future::Future<Output = F::Output> {
    ferridriver::trace::with_call_owner(
      Arc::clone(&self.call_owner),
      ferridriver_expect::with_sink(
        Arc::clone(&self.test_info) as Arc<dyn ferridriver_expect::SoftSink>,
        fut,
      ),
    )
  }

  /// Playwright `auto: true` fixtures resolve regardless of whether the
  /// test body destructured them. Walk the full def graph for this scope
  /// (and any narrower parents) and pre-resolve.
  async fn set_up_auto_fixtures(
    &self,
    budget: ferridriver::pause::BudgetStart,
  ) -> Result<ferridriver::Result<()>, ferridriver::pause::Timedout> {
    ferridriver::pause::run_within_updates_from(budget, self.modifiers.timeout_updates.subscribe(), async {
      for name in self.test_pool.auto_fixture_names_for(FixtureScope::Test) {
        self.test_pool.resolve(&name).await?;
      }
      Ok::<(), ferridriver::FerriError>(())
    })
    .await
  }

  /// Run the `beforeEach` hooks in order on the test's budget, stopping
  /// at the first that fails or runs the budget out.
  async fn run_before_each(
    &self,
    hooks: &[crate::model::HookFn],
    budget: ferridriver::pause::BudgetStart,
  ) -> Result<Result<(), TestFailure>, ferridriver::pause::Timedout> {
    for (i, hook) in hooks.iter().enumerate() {
      let step_handle = self
        .test_info
        .begin_step(&hook_title("beforeEach", i, hooks.len()), StepCategory::Hook)
        .await;
      let Ok(result) = ferridriver::pause::run_within_updates_from(
        budget,
        self.modifiers.timeout_updates.subscribe(),
        self.in_test_scope(run_caught(hook(self.test_pool.clone(), Arc::clone(&self.test_info)))),
      )
      .await
      else {
        let limit = *self.modifiers.timeout_updates.borrow();
        step_handle.end(Some(format!("Test timeout of {}ms exceeded.", limit.as_millis())));
        return Err(ferridriver::pause::Timedout);
      };
      step_handle.end(result.as_ref().err().map(|e| e.message.clone()));
      if let Err(e) = result {
        return Ok(Err(e));
      }
    }
    Ok(Ok(()))
  }

  /// Run the `afterEach` hooks in order on the after-hooks budget. A
  /// failing hook is logged and the rest still run; one that runs the
  /// budget out stops them, and the phase it stopped in is returned.
  async fn run_after_each(&self, hooks: &[crate::model::HookFn], after: &AfterBudget) -> Option<&'static str> {
    for (i, hook) in hooks.iter().enumerate() {
      let step_handle = self
        .test_info
        .begin_step(&hook_title("afterEach", i, hooks.len()), StepCategory::Hook)
        .await;
      let Ok(result) = ferridriver::pause::run_within_updates_from(
        after.start,
        after.updates.clone(),
        self.in_test_scope(run_caught(hook(self.test_pool.clone(), Arc::clone(&self.test_info)))),
      )
      .await
      else {
        let message = format!("afterEach timeout of {}ms exceeded.", after.limit.as_millis());
        tracing::warn!(target: "ferridriver::worker", "{message}");
        step_handle.end(Some(message));
        return Some("running afterEach hooks");
      };
      step_handle.end(result.as_ref().err().map(|e| e.message.clone()));
      if let Err(e) = result {
        tracing::warn!(target: "ferridriver::worker", "afterEach error: {e}");
      }
    }
    None
  }
}

/// After hooks get a budget of their own, the larger of the configured
/// and the test's timeout (Playwright's `calculateMaxTimeout`), so a test
/// that timed out still cleans up and a hung hook still ends. Artifact
/// capture and context teardown spend the same budget, as Playwright's
/// fixture teardown shares the after-hooks slot, so a page that wedged
/// the test cannot wedge the run.
struct AfterBudget {
  limit: Duration,
  start: ferridriver::pause::BudgetStart,
  updates: tokio::sync::watch::Receiver<Duration>,
  /// Held so `updates` stays open: a closed channel would end the budget.
  _limit: tokio::sync::watch::Sender<Duration>,
}

impl AfterBudget {
  fn new(config: &TestConfig, test_timeout: Duration) -> Self {
    let configured = Duration::from_millis(config.timeout);
    let limit = if configured.is_zero() || test_timeout.is_zero() {
      Duration::ZERO
    } else {
      configured.max(test_timeout)
    };
    let (sender, updates) = tokio::sync::watch::channel(limit);
    Self {
      limit,
      start: ferridriver::pause::BudgetStart::now(),
      updates,
      _limit: sender,
    }
  }

  /// The failure of a teardown that ran this budget out in `phase`.
  fn exceeded_while(&self, phase: &str) -> TestFailure {
    TestFailure {
      message: format!(
        "Tearing down the test exceeded the after-hooks timeout of {}ms while {phase}.",
        self.limit.as_millis()
      ),
      stack: None,
      diff: None,
      screenshot: None,
      error_context: None,
    }
  }
}

/// What [`Worker::set_up_attempt`] leaves for the hooks and the body.
struct AttemptSetup {
  budget: ferridriver::pause::BudgetStart,
  budget_started: Instant,
  before_each_err: Option<TestFailure>,
  timed_out_while: Option<&'static str>,
  page_for_artifacts: Option<Arc<ferridriver::Page>>,
  video_handle: Option<VideoHandle>,
  /// Reports the attempt's browser calls as `pw:api` steps until dropped.
  api_steps: ferridriver::trace::OwnerObserverGuard,
}

/// How the attempt's hooks and body ended.
struct AttemptEnd {
  attempt: u32,
  /// The raw body result failed, whatever `test.fail()` expects.
  test_failed: bool,
  timeout_result: Result<Result<(), TestFailure>, ferridriver::pause::Timedout>,
  /// The test's timeout as it stood at the end, `test.setTimeout` included.
  timeout_dur: Duration,
  /// The phase the test's budget ran out in, when it ran out before the body.
  timed_out_while: Option<&'static str>,
}

/// What the attempt left once torn down.
struct Settled {
  start: Instant,
  started_at: SystemTime,
  max_attempts: u32,
  /// The browser calls a timeout interrupted.
  interrupted: Vec<ferridriver::trace::PendingCall>,
  artifacts: Artifacts,
  teardown_failure: Option<TestFailure>,
}

/// What a test recorded on its `TestInfo` while it ran.
struct Recorded {
  steps: Vec<crate::model::TestStep>,
  attachments: Vec<Attachment>,
  snapshots: Vec<crate::snapshot_record::SnapshotComparison>,
}

/// What [`Worker::collect_artifacts`] captured.
struct Artifacts {
  screenshot: Option<Vec<u8>>,
  video_path: Option<std::path::PathBuf>,
  trace_path: Option<std::path::PathBuf>,
  page_snapshot: Option<String>,
}

enum VideoHandle {
  Eager(ferridriver::video::VideoRecordingHandle),
  Buffered(ferridriver::video::BufferedRecordingHandle),
}

enum VideoStart {
  /// The test's budget ran out opening the page.
  TimedOut,
  /// The page could not be created, which fails the attempt.
  PageFailed(ferridriver::FerriError),
  /// The page is open; the recording when it could be started.
  Started {
    page: Arc<ferridriver::Page>,
    handle: Option<VideoHandle>,
  },
}

/// A `beforeAll` / `afterAll` hook as the step it is reported as.
struct SuiteHookStep {
  test_id: crate::model::TestId,
  step_id: String,
  title: String,
  /// How the step's error reads.
  describe: fn(&TestFailure) -> String,
}

/// `beforeEach`, or `beforeEach [1]` when the suite has several.
fn hook_title(kind: &str, index: usize, count: usize) -> String {
  if count == 1 {
    kind.to_string()
  } else {
    format!("{kind} [{index}]")
  }
}

/// Whether a `skip` or `fixme` annotation applies to this run.
fn should_skip(test: &crate::model::TestCase, browser_config: &crate::config::BrowserConfig) -> bool {
  test.annotations.iter().any(|a| match a {
    TestAnnotation::Skip { condition: None, .. } | TestAnnotation::Fixme { condition: None, .. } => true,
    TestAnnotation::Skip {
      condition: Some(cond), ..
    }
    | TestAnnotation::Fixme {
      condition: Some(cond), ..
    } => evaluate_condition(cond, browser_config),
    _ => false,
  })
}

/// The declared expectation, turned to failure by a `fail` annotation
/// whose condition holds.
fn expected_status_for(test: &crate::model::TestCase, browser_config: &crate::config::BrowserConfig) -> ExpectedStatus {
  let fails = test.annotations.iter().any(|a| match a {
    TestAnnotation::Fail { condition: None, .. } => true,
    TestAnnotation::Fail {
      condition: Some(cond), ..
    } => evaluate_condition(cond, browser_config),
    _ => false,
  });
  if fails {
    ExpectedStatus::Fail
  } else {
    test.expected_status
  }
}
