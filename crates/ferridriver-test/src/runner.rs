//! Test runner orchestrator: overlaps browser launch with test dispatch,
//! handles retries with flaky detection.

use std::sync::Arc;
use std::time::Instant;

use rustc_hash::FxHashMap;
use tokio::sync::mpsc;

use crate::config::{CliOverrides, TestConfig};
use crate::dispatcher::Dispatcher;
use crate::fixture::{builtin_fixtures, validate_dag, FixturePool, FixtureScope};
use crate::model::{TestPlan, TestStatus};
use crate::reporter::{EventBus, ReporterEvent, ReporterSet};
use crate::shard;
use crate::worker::{Worker, WorkerTestResult};

use ferridriver::backend::BackendKind;
use ferridriver::options::LaunchOptions;
use ferridriver::Browser;

/// Top-level test runner.
pub struct TestRunner {
  config: Arc<TestConfig>,
  reporters: ReporterSet,
  overrides: CliOverrides,
}

impl TestRunner {
  pub fn new(config: TestConfig, reporters: ReporterSet, overrides: CliOverrides) -> Self {
    Self {
      config: Arc::new(config),
      reporters,
      overrides,
    }
  }

  /// Run the full test plan. Returns exit code (0 = all passed).
  pub async fn run(&mut self, mut plan: TestPlan) -> i32 {
    // ── Filtering ──
    if let Some(shard_arg) = &self.overrides.shard {
      shard::filter_by_shard(
        &mut plan,
        &crate::model::ShardInfo {
          current: shard_arg.current,
          total: shard_arg.total,
        },
      );
    }
    if let Some(grep) = &self.overrides.grep {
      crate::discovery::filter_by_grep(&mut plan, grep, false);
    }
    if let Some(grep_inv) = &self.overrides.grep_invert {
      crate::discovery::filter_by_grep(&mut plan, grep_inv, true);
    }
    if let Some(tag) = &self.overrides.tag {
      crate::discovery::filter_by_tag(&mut plan, tag);
    }

    let total_tests = plan.total_tests;
    if total_tests == 0 {
      tracing::info!("no tests found");
      return 0;
    }

    if self.overrides.list_only {
      for suite in &plan.suites {
        for test in &suite.tests {
          println!("  {}", test.id.full_name());
        }
      }
      println!("\n  {total_tests} test(s) found");
      return 0;
    }

    let num_workers = self.config.workers;

    // ── Validate fixture DAG ──
    {
      let fixture_defs = builtin_fixtures(&self.config.browser);
      if let Err(e) = validate_dag(&fixture_defs) {
        tracing::error!("fixture DAG error: {e}");
        return 1;
      }
    }

    // ── Event bus + reporter consumer ──
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<ReporterEvent>();
    let event_bus = EventBus::new(event_tx);

    let reporters = std::mem::take(&mut self.reporters);
    let reporter_handle = tokio::spawn(async move {
      let mut reporters = reporters;
      while let Some(event) = event_rx.recv().await {
        reporters.emit(&event).await;
      }
      reporters.finalize().await;
      reporters
    });

    event_bus
      .emit(ReporterEvent::RunStarted {
        total_tests,
        num_workers,
      })
      .await;

    let start = Instant::now();

    // ── Collect tests, apply repeatEach ──
    let repeat_each = self.config.repeat_each.max(1);
    let all_tests: Vec<_> = plan.suites.into_iter().flat_map(|s| s.tests).collect();
    let total_executions = all_tests.len() * repeat_each as usize;

    // ── Dispatcher ──
    let dispatcher = Arc::new(Dispatcher::new());
    for _rep in 0..repeat_each {
      dispatcher.enqueue_all_shared(&all_tests).await;
    }

    // ── Spawn workers with overlapped browser launch ──
    // Each worker launches its own browser and immediately starts processing tests.
    // This overlaps browser launch with test execution — workers that launch faster
    // start running tests while slower workers are still launching.
    // This saves ~80-100ms vs launching all browsers before dispatching.
    let (result_tx, mut result_rx) = mpsc::channel::<WorkerTestResult>(256);

    let mut worker_handles = Vec::new();
    let launch_opts = build_launch_options(&self.config.browser);

    for worker_id in 0..num_workers {
      let worker = Worker::new(worker_id, Arc::clone(&self.config), event_bus.clone());
      let rx = dispatcher.receiver();
      let tx = result_tx.clone();
      let custom_pool = FixturePool::new(FxHashMap::default(), FixtureScope::Worker);
      let opts = launch_opts.clone();

      let handle = tokio::spawn(async move {
        // Launch browser inside the worker task — overlaps with other workers.
        match Browser::launch(opts).await {
          Ok(browser) => {
            worker.run(Arc::new(browser), custom_pool, rx, tx).await;
          }
          Err(e) => {
            tracing::error!("worker {worker_id}: browser launch failed: {e}");
          }
        }
      });
      worker_handles.push(handle);
    }
    drop(result_tx);

    // ── Collect results with retry re-dispatch ──
    let mut attempt_history: FxHashMap<String, Vec<TestStatus>> = FxHashMap::default();
    let mut final_count = 0usize;

    while let Some(result) = result_rx.recv().await {
      let test_key = result.outcome.test_id.full_name();
      attempt_history
        .entry(test_key)
        .or_default()
        .push(result.outcome.status.clone());

      if result.should_retry {
        dispatcher
          .retry_shared(
            &result.test_fn,
            &result.test_id,
            result.fixture_requests.clone(),
            result.outcome.attempt + 1,
          )
          .await;
      } else {
        final_count += 1;
      }

      if final_count >= total_executions {
        dispatcher.close();
      }
    }

    for handle in worker_handles {
      let _ = handle.await;
    }

    let duration = start.elapsed();

    // ── Final stats with flaky detection ──
    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut skipped = 0usize;
    let mut flaky = 0usize;

    for attempts in attempt_history.values() {
      match crate::retry::RetryPolicy::final_status(attempts) {
        TestStatus::Passed => passed += 1,
        TestStatus::Flaky => {
          flaky += 1;
          passed += 1;
        }
        TestStatus::Skipped => skipped += 1,
        _ => failed += 1,
      }
    }

    event_bus
      .emit(ReporterEvent::RunFinished {
        total: total_tests,
        passed,
        failed,
        skipped,
        flaky,
        duration,
      })
      .await;

    drop(event_bus);
    if let Ok(reporters) = reporter_handle.await {
      self.reporters = reporters;
    }

    if failed > 0 { 1 } else { 0 }
  }
}

fn build_launch_options(browser_config: &crate::config::BrowserConfig) -> LaunchOptions {
  let backend = match browser_config.backend.as_str() {
    "cdp-raw" => BackendKind::CdpRaw,
    #[cfg(target_os = "macos")]
    "webkit" => BackendKind::WebKit,
    _ => BackendKind::CdpPipe,
  };
  LaunchOptions {
    backend,
    headless: browser_config.headless,
    executable_path: browser_config.executable_path.clone(),
    args: browser_config.args.clone(),
    viewport: browser_config.viewport.as_ref().map(|v| ferridriver::options::ViewportConfig {
      width: v.width,
      height: v.height,
      ..Default::default()
    }),
    ..Default::default()
  }
}
