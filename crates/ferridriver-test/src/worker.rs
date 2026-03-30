//! Worker: owns a browser instance directly (no fixture pool overhead for builtins),
//! creates fresh context+page per test for isolation, captures screenshots on failure.
//!
//! Performance model (matching Playwright):
//! - One Browser per worker (launched once, worker-scoped)
//! - Fresh BrowserContext+Page per test (19ms, but provides full isolation)
//! - Browser held directly — bypasses fixture pool DAG resolution and lock contention
//! - Fixture pool only used for custom user fixtures

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::config::TestConfig;
use crate::dispatcher::TestAssignment;
use crate::fixture::{FixturePool, FixtureScope};
use crate::model::{
  Attachment, AttachmentBody, TestAnnotation, TestFailure, TestOutcome, TestStatus,
};
use crate::reporter::{EventBus, ReporterEvent};

/// Result of a single test execution within a worker.
pub struct WorkerTestResult {
  pub outcome: TestOutcome,
  pub should_retry: bool,
  pub test_fn: crate::model::TestFn,
  pub test_id: crate::model::TestId,
  pub fixture_requests: Vec<String>,
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

  /// Run the worker loop.
  ///
  /// The browser is passed in pre-launched (all browsers are pre-warmed in parallel
  /// by the runner before dispatching tests). This eliminates sequential browser
  /// launch overhead.
  pub async fn run(
    &self,
    browser: Arc<ferridriver::Browser>,
    custom_fixture_pool: FixturePool,
    rx: async_channel::Receiver<TestAssignment>,
    result_tx: mpsc::Sender<WorkerTestResult>,
  ) {
    self
      .event_bus
      .emit(ReporterEvent::WorkerStarted { worker_id: self.id })
      .await;

    while let Ok(assignment) = rx.recv().await {
      let result = self.run_test(&browser, &custom_fixture_pool, assignment).await;
      if result_tx.send(result).await.is_err() {
        break;
      }
    }

    custom_fixture_pool.teardown_all().await;

    self
      .event_bus
      .emit(ReporterEvent::WorkerFinished { worker_id: self.id })
      .await;
  }

  async fn run_test(
    &self,
    browser: &Arc<ferridriver::Browser>,
    custom_pool: &FixturePool,
    assignment: TestAssignment,
  ) -> WorkerTestResult {
    let test = &assignment.test;
    let test_id = test.id.clone();
    let test_fn = Arc::clone(&test.test_fn);
    let fixture_requests = test.fixture_requests.clone();
    let attempt = assignment.attempt;
    let max_retries = test.retries.unwrap_or(self.config.retries);
    let max_attempts = max_retries + 1;

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

    // Create fresh isolated context + page using the standard path.
    // BrowserState is now optimized: cached SessionKey, direct AnyPage return,
    // viewport set in parallel with domain enables.
    let ctx = browser.new_context();
    let page_result = ctx.new_page().await;

    let result = match page_result {
      Ok(page) => {
        // Inject into lightweight fixture pool (no DAG resolution needed).
        let test_pool = custom_pool.child(FixtureScope::Test);
        test_pool.inject("browser", Arc::clone(browser)).await;
        test_pool.inject("page", Arc::new(page.clone())).await;

        let r = tokio::time::timeout(timeout_dur, (test.test_fn)(test_pool)).await;

        // Screenshot on failure (before context close).
        let screenshot = if r.as_ref().is_err() || r.as_ref().is_ok_and(|r| r.is_err()) {
          capture_screenshot(&page).await
        } else {
          None
        };

        // Close context (disposes isolated browser context, all pages, cookies).
        let _ = ctx.close().await;

        (r, screenshot)
      }
      Err(e) => {
        let _ = ctx.close().await;
        let r = Ok(Err(TestFailure {
          message: format!("failed to create page: {e}"),
          stack: None,
          diff: None,
          screenshot: None,
        }));
        (r, None)
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

    let (status, error) = match timeout_result {
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

    let outcome = TestOutcome {
      test_id: test_id.clone(),
      status,
      duration,
      attempt,
      max_attempts,
      error,
      attachments,
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

    let should_retry =
      outcome.status != TestStatus::Passed && outcome.status != TestStatus::Skipped && attempt < max_attempts;

    WorkerTestResult {
      outcome,
      should_retry,
      test_fn,
      test_id,
      fixture_requests,
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
