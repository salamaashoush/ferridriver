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
use std::time::{Duration, Instant};

use rustc_hash::FxHashMap;
use tokio::sync::{mpsc, Mutex};

use crate::config::TestConfig;
use crate::dispatcher::{SerialBatch, TestAssignment, WorkItem};
use crate::fixture::{FixturePool, FixtureScope};
use crate::model::{
  Attachment, AttachmentBody, ExpectedStatus, Hooks, TestAnnotation, TestFailure, TestInfo,
  TestOutcome, TestStatus,
};
use crate::reporter::{EventBus, ReporterEvent};

/// Result of a single test execution within a worker.
pub struct WorkerTestResult {
  pub outcome: TestOutcome,
  pub should_retry: bool,
  pub test_fn: crate::model::TestFn,
  pub test_id: crate::model::TestId,
  pub fixture_requests: Vec<String>,
  pub suite_key: String,
  pub hooks: Arc<Hooks>,
}

/// Per-suite state tracked on this worker.
struct SuiteState {
  before_all_ran: bool,
  before_all_failed: bool,
  hooks: Arc<Hooks>,
}

/// A worker that owns a browser and processes tests sequentially.
pub struct Worker {
  pub id: u32,
  config: Arc<TestConfig>,
  event_bus: EventBus,
}

impl Worker {
  pub fn new(id: u32, config: Arc<TestConfig>, event_bus: EventBus) -> Self {
    Self { id, config, event_bus }
  }

  pub async fn run(
    &self,
    browser: Arc<ferridriver::Browser>,
    custom_fixture_pool: FixturePool,
    rx: async_channel::Receiver<WorkItem>,
    result_tx: mpsc::Sender<WorkerTestResult>,
  ) {
    self
      .event_bus
      .emit(ReporterEvent::WorkerStarted { worker_id: self.id })
      .await;

    let mut active_suites: FxHashMap<String, SuiteState> = FxHashMap::default();

    while let Ok(item) = rx.recv().await {
      match item {
        WorkItem::Single(assignment) => {
          let result = self
            .run_single(&browser, &custom_fixture_pool, &mut active_suites, assignment)
            .await;
          if result_tx.send(result).await.is_err() {
            break;
          }
        }
        WorkItem::Serial(batch) => {
          let results = self
            .run_serial_batch(&browser, &custom_fixture_pool, &mut active_suites, batch)
            .await;
          for result in results {
            if result_tx.send(result).await.is_err() {
              break;
            }
          }
        }
      }
    }

    // Run afterAll for every suite that had beforeAll on this worker.
    for state in active_suites.values() {
      if state.before_all_ran {
        for hook in &state.hooks.after_all {
          if let Err(e) = hook(custom_fixture_pool.clone()).await {
            tracing::warn!(target: "ferridriver::worker", "afterAll error: {e}");
          }
        }
      }
    }

    custom_fixture_pool.teardown_all().await;

    self
      .event_bus
      .emit(ReporterEvent::WorkerFinished { worker_id: self.id })
      .await;
  }

  /// Run a serial batch: all tests in order, skip rest on failure.
  async fn run_serial_batch(
    &self,
    browser: &Arc<ferridriver::Browser>,
    custom_pool: &FixturePool,
    active_suites: &mut FxHashMap<String, SuiteState>,
    batch: SerialBatch,
  ) -> Vec<WorkerTestResult> {
    let mut results = Vec::with_capacity(batch.assignments.len());
    let mut serial_failed = false;

    for assignment in batch.assignments {
      if serial_failed {
        // Skip remaining tests in the serial suite.
        let test = &assignment.test;
        let outcome = TestOutcome {
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
          }),
          attachments: Vec::new(),
          steps: Vec::new(),
          stdout: String::new(),
          stderr: String::new(),
        };
        self
          .event_bus
          .emit(ReporterEvent::TestFinished {
            test_id: test.id.clone(),
            outcome: outcome.clone(),
          })
          .await;
        results.push(WorkerTestResult {
          outcome,
          should_retry: false,
          test_fn: Arc::clone(&test.test_fn),
          test_id: test.id.clone(),
          fixture_requests: test.fixture_requests.clone(),
          suite_key: assignment.suite_key,
          hooks: assignment.hooks,
        });
        continue;
      }

      let result = self
        .run_single(browser, custom_pool, active_suites, assignment)
        .await;
      if result.outcome.status == TestStatus::Failed
        || result.outcome.status == TestStatus::TimedOut
      {
        serial_failed = true;
      }
      results.push(result);
    }

    results
  }

  /// Run a single test with full hook lifecycle.
  async fn run_single(
    &self,
    browser: &Arc<ferridriver::Browser>,
    custom_pool: &FixturePool,
    active_suites: &mut FxHashMap<String, SuiteState>,
    assignment: TestAssignment,
  ) -> WorkerTestResult {
    let test = &assignment.test;
    let test_id = test.id.clone();
    let test_fn = Arc::clone(&test.test_fn);
    let fixture_requests = test.fixture_requests.clone();
    let attempt = assignment.attempt;
    let max_retries = test.retries.unwrap_or(self.config.retries);
    let max_attempts = max_retries + 1;
    let suite_key = assignment.suite_key.clone();

    tracing::debug!(
      target: "ferridriver::worker",
      worker = self.id,
      test = test_id.full_name(),
      attempt,
      max_attempts,
      "dispatching test",
    );
    let hooks = Arc::clone(&assignment.hooks);

    // ── beforeAll (once per suite on this worker) ──
    let suite_state = active_suites
      .entry(suite_key.clone())
      .or_insert_with(|| SuiteState {
        before_all_ran: false,
        before_all_failed: false,
        hooks: Arc::clone(&hooks),
      });

    if !suite_state.before_all_ran && !hooks.before_all.is_empty() {
      for hook in &hooks.before_all {
        if let Err(e) = hook(custom_pool.clone()).await {
          tracing::error!(target: "ferridriver::worker", "beforeAll failed for {suite_key}: {e}");
          suite_state.before_all_failed = true;
          break;
        }
      }
      suite_state.before_all_ran = true;
    }

    // If beforeAll failed, skip this test.
    if suite_state.before_all_failed {
      let outcome = TestOutcome {
        test_id: test_id.clone(),
        status: TestStatus::Skipped,
        duration: Duration::ZERO,
        attempt,
        max_attempts,
        error: Some(TestFailure {
          message: format!("skipped: beforeAll failed for suite '{suite_key}'"),
          stack: None,
          diff: None,
          screenshot: None,
        }),
        attachments: Vec::new(),
        steps: Vec::new(),
        stdout: String::new(),
        stderr: String::new(),
      };
      self
        .event_bus
        .emit(ReporterEvent::TestFinished {
          test_id: test_id.clone(),
          outcome: outcome.clone(),
        })
        .await;
      return WorkerTestResult {
        outcome,
        should_retry: false,
        test_fn,
        test_id,
        fixture_requests,
        suite_key,
        hooks,
      };
    }

    // Check for skip/fixme.
    if test.annotations.iter().any(|a| {
      matches!(a, TestAnnotation::Skip { .. } | TestAnnotation::Fixme { .. })
    }) {
      let outcome = TestOutcome {
        test_id: test_id.clone(),
        status: TestStatus::Skipped,
        duration: Duration::ZERO,
        attempt,
        max_attempts,
        error: None,
        attachments: Vec::new(),
        steps: Vec::new(),
        stdout: String::new(),
        stderr: String::new(),
      };
      self
        .event_bus
        .emit(ReporterEvent::TestFinished {
          test_id: test_id.clone(),
          outcome: outcome.clone(),
        })
        .await;
      return WorkerTestResult {
        outcome,
        should_retry: false,
        test_fn,
        test_id,
        fixture_requests,
        suite_key,
        hooks,
      };
    }

    self
      .event_bus
      .emit(ReporterEvent::TestStarted {
        test_id: test_id.clone(),
        attempt,
      })
      .await;

    // Timeout with slow multiplier.
    let mut timeout_dur = test
      .timeout
      .unwrap_or(Duration::from_millis(self.config.timeout));
    if test.annotations.iter().any(|a| matches!(a, TestAnnotation::Slow)) {
      timeout_dur *= 3;
    }

    let start = Instant::now();

    // Create fresh isolated context + page.
    let ctx = browser.new_context();
    let page_result = ctx.new_page().await;

    // Create TestInfo for this test execution.
    let test_info = Arc::new(TestInfo {
      test_id: test_id.clone(),
      title_path: {
        let mut path = Vec::new();
        path.push(test_id.file.clone());
        if let Some(ref s) = test_id.suite {
          path.push(s.clone());
        }
        path.push(test_id.name.clone());
        path
      },
      retry: attempt.saturating_sub(1),
      worker_index: self.id,
      parallel_index: self.id,
      repeat_each_index: 0,
      output_dir: self.config.output_dir.join(test_id.full_name()),
      snapshot_dir: std::path::PathBuf::from("__snapshots__"),
      attachments: Arc::new(Mutex::new(Vec::new())),
      steps: Arc::new(Mutex::new(Vec::new())),
      soft_errors: Arc::new(Mutex::new(Vec::new())),
      timeout: timeout_dur,
      tags: test
        .annotations
        .iter()
        .filter_map(|a| match a {
          TestAnnotation::Tag(t) => Some(t.clone()),
          _ => None,
        })
        .collect(),
      start_time: start,
      event_bus: Some(self.event_bus.clone()),
    });

    let result = match page_result {
      Ok(page) => {
        let test_pool = custom_pool.child(FixtureScope::Test);
        test_pool.inject("browser", Arc::clone(browser)).await;
        test_pool.inject("context", Arc::new(ctx.clone())).await;
        test_pool.inject("page", Arc::new(page.clone())).await;
        test_pool.inject("test_info", Arc::clone(&test_info)).await;

        // ── beforeEach hooks ──
        let mut before_each_err = None;
        for hook in &hooks.before_each {
          if let Err(e) = hook(test_pool.clone(), Arc::clone(&test_info)).await {
            before_each_err = Some(e);
            break;
          }
        }

        let r = if let Some(err) = before_each_err {
          Ok(Err(err))
        } else {
          tokio::time::timeout(timeout_dur, (test.test_fn)(test_pool.clone())).await
        };

        // ── afterEach hooks (ALWAYS run, even on failure) ──
        for hook in &hooks.after_each {
          if let Err(e) = hook(test_pool.clone(), Arc::clone(&test_info)).await {
            tracing::warn!(target: "ferridriver::worker", "afterEach error: {e}");
          }
        }

        // Screenshot on failure (before context close).
        let screenshot = if r.as_ref().is_err() || r.as_ref().is_ok_and(|r| r.is_err()) {
          capture_screenshot(&page).await
        } else {
          None
        };

        let _ = ctx.close().await;
        (r, screenshot)
      }
      Err(e) => {
        let _ = ctx.close().await;
        (
          Ok(Err(TestFailure {
            message: format!("failed to create page: {e}"),
            stack: None,
            diff: None,
            screenshot: None,
          })),
          None,
        )
      }
    };

    let duration = start.elapsed();
    let (timeout_result, screenshot) = result;

    let mut attachments = Vec::new();
    if let Some(ref png) = screenshot {
      attachments.push(Attachment {
        name: "screenshot-on-failure".into(),
        content_type: "image/png".into(),
        body: AttachmentBody::Bytes(png.clone()),
      });
    }

    let (raw_status, raw_error) = match timeout_result {
      Ok(Ok(())) => (TestStatus::Passed, None),
      Ok(Err(mut failure)) => {
        if failure.screenshot.is_none() {
          failure.screenshot = screenshot;
        }
        (TestStatus::Failed, Some(failure))
      }
      Err(_) => (
        TestStatus::TimedOut,
        Some(TestFailure {
          message: format!("test timed out after {timeout_dur:?}"),
          stack: None,
          diff: None,
          screenshot,
        }),
      ),
    };

    // Expected failure inversion (test.fail() annotation).
    let (status, error) = match (&raw_status, &test.expected_status) {
      (TestStatus::Failed | TestStatus::TimedOut, ExpectedStatus::Fail) => {
        (TestStatus::Passed, None)
      }
      (TestStatus::Passed, ExpectedStatus::Fail) => (
        TestStatus::Failed,
        Some(TestFailure {
          message: "expected test to fail, but it passed".into(),
          stack: None,
          diff: None,
          screenshot: None,
        }),
      ),
      _ => (raw_status, raw_error),
    };

    // Collect soft assertion errors.
    let soft_errs = test_info.drain_soft_errors().await;
    let (status, error) = if !soft_errs.is_empty() && status == TestStatus::Passed {
      let msg = soft_errs
        .iter()
        .map(|e| format!("  - {}", e.message))
        .collect::<Vec<_>>()
        .join("\n");
      (
        TestStatus::Failed,
        Some(TestFailure {
          message: format!("{} soft assertion(s) failed:\n{msg}", soft_errs.len()),
          stack: None,
          diff: None,
          screenshot: None,
        }),
      )
    } else {
      (status, error)
    };

    // Collect tracked test steps and attachments.
    let steps = test_info.steps.lock().await.clone();
    let info_attachments = test_info.attachments.lock().await.clone();
    attachments.extend(info_attachments);

    let outcome = TestOutcome {
      test_id: test_id.clone(),
      status,
      duration,
      attempt,
      max_attempts,
      error,
      attachments,
      steps,
      stdout: String::new(),
      stderr: String::new(),
    };

    self
      .event_bus
      .emit(ReporterEvent::TestFinished {
        test_id: test_id.clone(),
        outcome: outcome.clone(),
      })
      .await;

    let should_retry =
      outcome.status != TestStatus::Passed && outcome.status != TestStatus::Skipped && attempt < max_attempts;

    WorkerTestResult {
      outcome,
      should_retry,
      test_fn,
      test_id,
      fixture_requests,
      suite_key,
      hooks,
    }
  }
}

async fn capture_screenshot(page: &ferridriver::Page) -> Option<Vec<u8>> {
  let opts = ferridriver::options::ScreenshotOptions {
    full_page: Some(true),
    format: Some("png".into()),
    quality: None,
  };
  page.screenshot(opts).await.ok()
}
