//! Test runner orchestrator: overlaps browser launch with test dispatch,
//! handles retries with flaky detection.

use std::sync::Arc;
use std::time::Instant;

use rustc_hash::FxHashMap;
use tokio::sync::mpsc;

use crate::config::{CliOverrides, ProjectConfig, TestConfig};
use crate::dispatcher::Dispatcher;
use crate::fixture::{FixturePool, FixtureScope, builtin_fixtures, validate_dag};
use crate::model::{Hooks, TestPlan, TestStatus};
use crate::reporter::{EventBus, EventBusBuilder, ReporterDriver, ReporterEvent, ReporterSet};
use crate::shard;
use crate::worker::{Worker, WorkerTestResult};

use ferridriver::Browser;
use ferridriver::backend::BackendKind;
use ferridriver::options::LaunchOptions;

/// Top-level test runner.
pub struct TestRunner {
  config: Arc<TestConfig>,
  reporters: ReporterSet,
  overrides: CliOverrides,
  /// Shared browser for watch mode (persists across runs).
  shared_browser: Option<Arc<Browser>>,
}

impl TestRunner {
  pub fn new(config: TestConfig, overrides: CliOverrides) -> Self {
    let reporters = crate::reporter::create_reporters(
      &config.reporter,
      &config.output_dir,
      config.has_bdd,
      config.quiet,
      config.report_slow_tests.clone(),
    );
    Self {
      config: Arc::new(config),
      reporters,
      overrides,
      shared_browser: None,
    }
  }

  /// Append an additional reporter after construction (e.g., NAPI ResultCollector).
  pub fn add_reporter(&mut self, reporter: Box<dyn crate::reporter::Reporter>) {
    self.reporters.add(reporter);
  }

  /// Run the full test plan. Returns exit code (0 = all passed).
  ///
  /// When `config.projects` is non-empty, topologically sorts projects by
  /// dependencies and runs each with a merged config. Otherwise runs the
  /// plan directly (single-project path).
  ///
  /// Convenience wrapper: creates an `EventBus`, subscribes a `ReporterDriver`,
  /// and delegates to `execute()`. For real-time external observation (TUI, WebSocket),
  /// use `execute()` directly with a custom bus.
  pub async fn run(&mut self, plan: TestPlan) -> i32 {
    // ── Multi-project path ──
    if !self.config.projects.is_empty() {
      return self.run_projects(plan).await;
    }

    // ── Single-project path ──
    let mut builder = EventBusBuilder::new();
    let reporter_sub = builder.subscribe();
    let bus = builder.build();

    let reporters = std::mem::take(&mut self.reporters);
    let driver = ReporterDriver::new(reporters, reporter_sub);
    let driver_handle = tokio::spawn(driver.run());

    let exit_code = self.execute(plan, bus.clone()).await;

    // Explicitly close senders so the driver's recv() returns None.
    // Cannot rely on Drop — tokio::spawn defers task deallocation,
    // keeping Arc<EventBusInner> alive after JoinHandle::await returns.
    bus.close();

    if let Ok(reporters) = driver_handle.await {
      self.reporters = reporters;
    }

    exit_code
  }

  /// Run multiple projects in dependency order.
  ///
  /// Each project creates a merged config and runs the full execute pipeline
  /// with its own browser instance. Results are aggregated — if any project
  /// fails, the overall exit code is non-zero.
  ///
  /// Follows Playwright's project semantics:
  /// - Projects are topologically sorted by `dependencies`
  /// - A project only runs after all its dependencies have passed
  /// - `teardown` projects run after the project and all its dependents complete
  /// - If a dependency fails, dependent projects are skipped
  async fn run_projects(&mut self, plan: TestPlan) -> i32 {
    let projects = self.config.projects.clone();

    let sorted = match topo_sort_projects(&projects) {
      Ok(order) => order,
      Err(e) => {
        tracing::error!(target: "ferridriver::runner", "project dependency error: {e}");
        return 1;
      },
    };

    tracing::info!(
      target: "ferridriver::runner",
      projects = sorted.len(),
      order = ?sorted.iter().map(|i| &projects[*i].name).collect::<Vec<_>>(),
      "running projects in dependency order",
    );

    let mut exit_code = 0i32;
    let mut failed_projects: rustc_hash::FxHashSet<String> = rustc_hash::FxHashSet::default();
    // Track completed projects for teardown scheduling.
    let mut completed_projects: rustc_hash::FxHashSet<String> = rustc_hash::FxHashSet::default();
    // Collect teardown projects to run after all dependents finish.
    let mut pending_teardowns: Vec<usize> = Vec::new();

    for &idx in &sorted {
      let project = &projects[idx];

      // Skip if any dependency failed.
      let dep_failed = project.dependencies.iter().any(|dep| failed_projects.contains(dep));
      if dep_failed {
        tracing::warn!(
          target: "ferridriver::runner",
          project = project.name,
          "skipping — dependency failed",
        );
        failed_projects.insert(project.name.clone());
        continue;
      }

      // Check if this is a teardown-only project (referenced by another project's `teardown` field).
      // Teardown projects are deferred until after their parent and all dependents complete.
      let is_teardown = projects.iter().any(|p| p.teardown.as_deref() == Some(&project.name));
      if is_teardown && !completed_projects.contains(&project.name) {
        // Haven't been explicitly scheduled yet — defer.
        pending_teardowns.push(idx);
        continue;
      }

      let project_exit = self.run_single_project(project, &plan).await;

      completed_projects.insert(project.name.clone());

      if project_exit != 0 {
        exit_code = 1;
        failed_projects.insert(project.name.clone());
      }

      // Run any teardown projects whose parent just completed.
      if let Some(ref teardown_name) = project.teardown {
        if let Some(td_idx) = projects.iter().position(|p| p.name == *teardown_name) {
          let td_project = &projects[td_idx];
          tracing::info!(
            target: "ferridriver::runner",
            project = td_project.name,
            parent = project.name,
            "running teardown project",
          );
          let td_exit = self.run_single_project(td_project, &plan).await;
          completed_projects.insert(td_project.name.clone());
          // Remove from pending if it was deferred.
          pending_teardowns.retain(|&i| i != td_idx);
          if td_exit != 0 {
            exit_code = 1;
          }
        }
      }
    }

    // Run any remaining deferred teardown projects.
    for td_idx in pending_teardowns {
      let td_project = &projects[td_idx];
      if completed_projects.contains(&td_project.name) {
        continue;
      }
      tracing::info!(
        target: "ferridriver::runner",
        project = td_project.name,
        "running deferred teardown project",
      );
      let td_exit = self.run_single_project(td_project, &plan).await;
      if td_exit != 0 {
        exit_code = 1;
      }
    }

    exit_code
  }

  /// Run a single project with merged config.
  async fn run_single_project(&mut self, project: &ProjectConfig, base_plan: &TestPlan) -> i32 {
    let merged_config = self.config.merge_project(project);

    // Clone and filter the plan for this project.
    let mut plan = base_plan.clone();
    filter_plan_for_project(&mut plan, &merged_config, project);

    if plan.total_tests == 0 {
      tracing::debug!(
        target: "ferridriver::runner",
        project = project.name,
        "no tests matched, skipping",
      );
      return 0;
    }

    tracing::info!(
      target: "ferridriver::runner",
      project = project.name,
      tests = plan.total_tests,
      "running project",
    );

    // Create a sub-runner with merged config. Reuse our reporters + overrides.
    let reporters = std::mem::take(&mut self.reporters);

    let mut builder = EventBusBuilder::new();
    let reporter_sub = builder.subscribe();
    let bus = builder.build();

    let driver = ReporterDriver::new(reporters, reporter_sub);
    let driver_handle = tokio::spawn(driver.run());

    // Build a temporary runner with the merged config.
    let sub_runner = TestRunner {
      config: Arc::new(merged_config),
      reporters: ReporterSet::default(),
      overrides: self.overrides.clone(),
      shared_browser: self.shared_browser.clone(),
    };

    let exit_code = sub_runner.execute(plan, bus.clone()).await;
    bus.close();

    if let Ok(reporters) = driver_handle.await {
      self.reporters = reporters;
    }

    exit_code
  }

  /// Core execution engine. Emits events on the provided `EventBus`.
  ///
  /// Takes `&self` — no reporter ownership, no mutable state. The caller
  /// controls who subscribes to the bus (reporters, TUI, external consumers).
  ///
  /// The bus is consumed by value and dropped when execution completes,
  /// closing all subscriber channels and signaling consumers to finalize.
  #[tracing::instrument(skip_all, fields(workers = self.config.workers, tests = plan.total_tests))]
  pub async fn execute(&self, mut plan: TestPlan, event_bus: EventBus) -> i32 {
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
    // Apply grep: CLI overrides take precedence, then config-level grep.
    let grep = self.overrides.grep.as_ref().or(self.config.config_grep.as_ref());
    let grep_inv = self
      .overrides
      .grep_invert
      .as_ref()
      .or(self.config.config_grep_invert.as_ref());
    if let Some(grep) = grep {
      crate::discovery::filter_by_grep(&mut plan, grep, false);
    }
    if let Some(grep_inv) = grep_inv {
      crate::discovery::filter_by_grep(&mut plan, grep_inv, true);
    }
    if let Some(tag) = &self.overrides.tag {
      crate::discovery::filter_by_tag(&mut plan, tag);
    }

    // ── Forbid-only check ──
    if self.config.forbid_only || self.overrides.forbid_only {
      if let Err(e) = crate::discovery::check_forbid_only(&plan) {
        eprint!("{e}");
        return 1;
      }
    }

    // ── Only filtering: if any test/suite has Only, keep only those ──
    crate::discovery::filter_by_only(&mut plan);

    // ── Last-failed rerun filter ──
    if self.overrides.last_failed {
      let rerun_path = self.config.output_dir.join("@rerun.txt");
      crate::discovery::filter_by_rerun(&mut plan, &rerun_path);
    }

    // ── preserve_output: "never" — wipe output_dir at run start ──
    if self.config.preserve_output == "never" {
      let _ = std::fs::remove_dir_all(&self.config.output_dir);
    }

    let total_tests = plan.total_tests;
    tracing::debug!(
      target: "ferridriver::runner",
      total_tests,
      suites = plan.suites.len(),
      "test plan after filtering",
    );
    if total_tests == 0 {
      tracing::info!(target: "ferridriver::runner", "no tests found");
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

    // Never launch more workers than tests — extra workers launch browsers for nothing.
    let num_workers = (self.config.workers as usize).min(total_tests).max(1) as u32;

    // ── Validate fixture DAG ──
    {
      let fixture_defs = builtin_fixtures(&self.config.browser);
      if let Err(e) = validate_dag(&fixture_defs) {
        tracing::error!(target: "ferridriver::fixture", "fixture DAG error: {e}");
        return 1;
      }
    }

    // ── Web server lifecycle ──
    // Follows Playwright's pattern: start servers, set FERRIDRIVER_BASE_URL env var.
    let web_server_manager = if !self.config.web_server.is_empty() {
      match crate::server::WebServerManager::start(&self.config.web_server).await {
        Ok(mgr) => {
          if let Some(url) = mgr.first_url() {
            if self.config.base_url.is_none() {
              // SAFETY: set_var is called before worker threads are spawned,
              // so no concurrent reads can race.
              #[allow(unsafe_code)]
              unsafe {
                std::env::set_var("FERRIDRIVER_BASE_URL", &url)
              };
              tracing::info!(target: "ferridriver::runner", "webServer base_url={url}");
            }
          }
          Some(mgr)
        },
        Err(e) => {
          tracing::error!(target: "ferridriver::runner", "webServer start failed: {e}");
          return 1;
        },
      }
    } else {
      None
    };

    event_bus
      .emit(ReporterEvent::RunStarted {
        total_tests,
        num_workers,
        metadata: self.config.metadata.clone(),
      })
      .await;

    let start = Instant::now();

    // ── Global setup ──
    if !self.config.global_setup_fns.is_empty() {
      let global_pool = FixturePool::new(FxHashMap::default(), FixtureScope::Global);
      for setup_fn in &self.config.global_setup_fns {
        if let Err(e) = setup_fn(global_pool.clone()).await {
          tracing::error!(target: "ferridriver::runner", "global setup failed: {e}");
          event_bus
            .emit(ReporterEvent::RunFinished {
              total: total_tests,
              passed: 0,
              failed: total_tests,
              skipped: 0,
              flaky: 0,
              duration: start.elapsed(),
            })
            .await;
          return 1;
        }
      }
    }

    // ── Collect tests, apply repeatEach ──
    let repeat_each = self.config.repeat_each.max(1);
    let total_executions = total_tests * repeat_each as usize;

    // ── Dispatcher — enqueue suites with hooks + mode context ──
    let dispatcher = Arc::new(Dispatcher::new());
    for _rep in 0..repeat_each {
      for suite in &plan.suites {
        let suite_key = format!("{}::{}", suite.file, suite.name);
        let hooks = Arc::new(Hooks {
          before_all: suite.hooks.before_all.clone(),
          after_all: suite.hooks.after_all.clone(),
          before_each: suite.hooks.before_each.clone(),
          after_each: suite.hooks.after_each.clone(),
        });

        match suite.mode {
          crate::model::SuiteMode::Parallel => {
            for test in &suite.tests {
              let assignment = crate::dispatcher::TestAssignment {
                test: crate::model::TestCase {
                  id: test.id.clone(),
                  test_fn: Arc::clone(&test.test_fn),
                  fixture_requests: test.fixture_requests.clone(),
                  annotations: test.annotations.clone(),
                  timeout: test.timeout,
                  retries: test.retries,
                  expected_status: test.expected_status.clone(),
                  use_options: test.use_options.clone(),
                },
                attempt: 1,
                suite_key: suite_key.clone(),
                hooks: Arc::clone(&hooks),
                suite_mode: crate::model::SuiteMode::Parallel,
              };
              dispatcher.enqueue_single(assignment);
            }
          },
          crate::model::SuiteMode::Serial => {
            let assignments: Vec<_> = suite
              .tests
              .iter()
              .map(|test| crate::dispatcher::TestAssignment {
                test: crate::model::TestCase {
                  id: test.id.clone(),
                  test_fn: Arc::clone(&test.test_fn),
                  fixture_requests: test.fixture_requests.clone(),
                  annotations: test.annotations.clone(),
                  timeout: test.timeout,
                  retries: test.retries,
                  expected_status: test.expected_status.clone(),
                  use_options: test.use_options.clone(),
                },
                attempt: 1,
                suite_key: suite_key.clone(),
                hooks: Arc::clone(&hooks),
                suite_mode: crate::model::SuiteMode::Serial,
              })
              .collect();
            dispatcher.enqueue_serial(crate::dispatcher::SerialBatch {
              suite_key: suite_key.clone(),
              assignments,
              hooks: Arc::clone(&hooks),
            });
          },
        }
      }
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
      let shared = self.shared_browser.clone();
      let opts = launch_opts.clone();

      let handle = tokio::spawn(async move {
        // Use shared browser (watch mode) or launch a new one per worker.
        let browser = if let Some(b) = shared {
          b
        } else {
          match Browser::launch(opts).await {
            Ok(b) => Arc::new(b),
            Err(e) => {
              tracing::error!(target: "ferridriver::worker", "worker {worker_id}: browser launch failed: {e}");
              return;
            },
          }
        };
        Box::pin(worker.run(browser, custom_pool, rx, tx)).await;
      });
      worker_handles.push(handle);
    }
    drop(result_tx);

    // ── Collect results with retry re-dispatch ──
    let mut attempt_history: FxHashMap<String, Vec<TestStatus>> = FxHashMap::default();
    let mut final_count = 0usize;
    let mut failure_count = 0usize;
    let max_failures = if self.config.fail_fast {
      1 // fail_fast = stop after first failure
    } else {
      self.config.max_failures as usize // 0 = unlimited
    };

    while let Some(result) = result_rx.recv().await {
      let test_key = result.outcome.test_id.full_name();
      attempt_history
        .entry(test_key)
        .or_default()
        .push(result.outcome.status.clone());

      if result.should_retry {
        tracing::debug!(
          target: "ferridriver::runner",
          test = result.test_id.full_name(),
          attempt = result.outcome.attempt,
          "retrying failed test",
        );
        dispatcher.retry_shared(
          &result.test_fn,
          &result.test_id,
          result.fixture_requests.clone(),
          result.outcome.attempt + 1,
          result.suite_key.clone(),
          Arc::clone(&result.hooks),
        );
      } else {
        final_count += 1;
        // Track failures for max_failures / fail_fast.
        if matches!(result.outcome.status, TestStatus::Failed | TestStatus::TimedOut) {
          failure_count += 1;
        }
      }

      // Stop early if max_failures reached.
      if max_failures > 0 && failure_count >= max_failures {
        tracing::info!(
          target: "ferridriver::runner",
          failure_count,
          max_failures,
          "max failures reached, stopping",
        );
        dispatcher.close();
      }

      if final_count >= total_executions {
        dispatcher.close();
      }
    }

    for handle in worker_handles {
      let _ = handle.await;
    }

    // ── Global teardown (always runs, even if tests failed) ──
    if !self.config.global_teardown_fns.is_empty() {
      let global_pool = FixturePool::new(FxHashMap::default(), FixtureScope::Global);
      for teardown_fn in &self.config.global_teardown_fns {
        if let Err(e) = teardown_fn(global_pool.clone()).await {
          tracing::error!(target: "ferridriver::runner", "global teardown error: {e}");
        }
      }
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
        },
        TestStatus::Skipped => skipped += 1,
        _ => failed += 1,
      }
    }

    // ── preserve_output: "failures-only" — delete output dirs for passing tests ──
    if self.config.preserve_output == "failures-only" {
      for (test_key, attempts) in &attempt_history {
        let status = crate::retry::RetryPolicy::final_status(attempts);
        if matches!(status, TestStatus::Passed | TestStatus::Skipped | TestStatus::Flaky) {
          let test_output_dir = self.config.output_dir.join(test_key);
          if test_output_dir.exists() {
            let _ = std::fs::remove_dir_all(&test_output_dir);
          }
        }
      }
    }

    // ── Web server teardown ──
    if let Some(mgr) = web_server_manager {
      mgr.stop().await;
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

    if failed > 0 { 1 } else { 0 }
  }

  /// Run in watch mode: re-run tests on file changes with interactive keyboard controls.
  ///
  /// Launches a browser once and reuses it across all runs. Watches the project
  /// directory for file changes and dispatches re-runs based on change type.
  ///
  /// # Arguments
  ///
  /// * `plan_factory` — Closure that generates a `TestPlan`. Receives an optional slice
  ///   of changed file paths — when `Some`, the factory should only re-process those files
  ///   (e.g., re-parse only changed `.feature` files). When `None`, generate the full plan.
  /// * `watch_root` — Root directory to watch for file changes.
  pub async fn run_watch<F>(&mut self, plan_factory: F, watch_root: std::path::PathBuf) -> i32
  where
    F: Fn(Option<&[std::path::PathBuf]>) -> TestPlan,
  {
    use crate::watch::FileWatcher;

    // Launch browser once — reuse across all watch cycles.
    let launch_opts = build_launch_options(&self.config.browser);
    let browser = match Browser::launch(launch_opts).await {
      Ok(b) => Arc::new(b),
      Err(e) => {
        eprintln!("Failed to launch browser: {e}");
        return 1;
      },
    };
    self.shared_browser = Some(Arc::clone(&browser));

    // Start file watcher — uses test_match globs for classification, test_ignore for filtering.
    let watcher = match FileWatcher::new(&watch_root, &self.config.test_match, &self.config.test_ignore) {
      Ok(w) => w,
      Err(e) => {
        eprintln!("Failed to start file watcher: {e}");
        return 1;
      },
    };

    // Try TUI (requires TTY). Falls back to non-interactive for CI/pipes.
    let tui_result = crate::tui::WatchTui::new();

    match tui_result {
      Ok((mut tui, tui_tx)) => {
        self
          .run_watch_tui(&mut tui, tui_tx, &watcher, &plan_factory, &browser)
          .await;
        tui.shutdown();
      },
      Err(e) => {
        // Non-TTY fallback: file changes only, no keyboard, normal terminal output.
        tracing::debug!(target: "ferridriver::watch", "TUI unavailable ({e}), running non-interactive");
        self.run_watch_headless(&watcher, &plan_factory).await;
      },
    }

    // Cleanup.
    self.shared_browser = None;
    let _ = browser.close(None).await;

    0
  }

  /// Execute a plan while draining TUI messages in real-time.
  ///
  /// Creates a fresh `EventBus` + `ReporterDriver` per run cycle. The driver
  /// runs in a spawned task; `execute()` and `tui.drain_while_running()` run
  /// concurrently via `tokio::join!`, so the TUI renders events as they arrive.
  /// Execute a plan while draining TUI messages in real-time.
  /// Returns true if the user cancelled (q/Ctrl+C during run).
  async fn run_with_tui_drain(&mut self, plan: TestPlan, tui: &mut crate::tui::WatchTui) -> bool {
    let mut builder = EventBusBuilder::new();
    let reporter_sub = builder.subscribe();
    let bus = builder.build();

    let reporters = std::mem::take(&mut self.reporters);
    let driver = ReporterDriver::new(reporters, reporter_sub);
    let driver_handle = tokio::spawn(driver.run());

    // Execute tests and drain TUI concurrently via select!.
    // If the user presses q/Ctrl+C, drain returns Cancelled and
    // select! drops the execute future (cancelling it).
    let cancelled = tokio::select! {
      _ = self.execute(plan, bus.clone()) => {
        tui.flush();
        false
      }
      result = tui.drain_while_running() => {
        matches!(result, crate::tui::DrainResult::Cancelled)
      }
    };

    bus.close();
    if let Ok(reporters) = driver_handle.await {
      self.reporters = reporters;
    }

    cancelled
  }

  /// TUI watch loop: ratatui inline viewport with status bar + key controls.
  async fn run_watch_tui<F>(
    &mut self,
    tui: &mut crate::tui::WatchTui,
    tui_tx: tokio::sync::mpsc::UnboundedSender<crate::tui::TuiMessage>,
    watcher: &crate::watch::FileWatcher,
    plan_factory: &F,
    _browser: &Arc<Browser>,
  ) where
    F: Fn(Option<&[std::path::PathBuf]>) -> TestPlan,
  {
    use crate::interactive::WatchCommand;

    let mut grep_filter: Option<String> = None;

    // Replace ALL reporters with TUI reporter + rerun.
    // Persist across watch cycles via run_with_tui_drain's take/restore.
    self.reporters.replace(vec![
      Box::new(crate::tui_reporter::TuiReporter::new(
        tui_tx.clone(),
        self.config.has_bdd,
      )),
      Box::new(crate::reporter::rerun::RerunReporter::new(
        self.config.output_dir.join("@rerun.txt"),
      )),
    ]);

    // Initial run — TUI drains messages in real-time.
    let plan = plan_factory(None);
    if self.run_with_tui_drain(plan, tui).await {
      return; // User cancelled during initial run.
    }
    tui.set_status(crate::tui::WatchStatus::Idle);

    // Watch loop — TUI handles both key input and message display.
    loop {
      tokio::select! {
        change = watcher.recv() => {
          let Some(change) = change else { break };
          let mut all_changes = vec![change];
          all_changes.extend(watcher.drain_deduped());

          let (run_all, changed_paths) = classify_changes(&all_changes);
          if !run_all && changed_paths.is_empty() { continue; }

          let mut plan = build_plan_for_changes(plan_factory, run_all, &changed_paths);
          // Apply active filter to file-change re-runs.
          if let Some(ref pattern) = grep_filter {
            crate::discovery::filter_by_grep(&mut plan, pattern, false);
          }
          if plan.total_tests == 0 { continue; }

          if self.run_with_tui_drain(plan, tui).await { break; }
          tui.set_status(crate::tui::WatchStatus::Idle);
        }

        cmd = tui.next_command() => {
          let Some(cmd) = cmd else { break };
          match cmd {
            WatchCommand::Quit => break,
            WatchCommand::RunAll => {
              grep_filter = None;
              tui.active_filter = None;
              if self.run_with_tui_drain(plan_factory(None), tui).await { break; }
              tui.set_status(crate::tui::WatchStatus::Idle);
            }
            WatchCommand::RunFailed => {
              let mut plan = plan_factory(None);
              let rerun_path = self.config.output_dir.join("@rerun.txt");
              if rerun_path.exists() {
                crate::discovery::filter_by_rerun(&mut plan, &rerun_path);
              }
              // Apply active filter on top of failed filter.
              if let Some(ref pattern) = grep_filter {
                crate::discovery::filter_by_grep(&mut plan, pattern, false);
              }
              if plan.total_tests > 0
                && self.run_with_tui_drain(plan, tui).await { break; }
              tui.set_status(crate::tui::WatchStatus::Idle);
            }
            WatchCommand::Rerun => {
              let mut plan = plan_factory(None);
              if let Some(ref pattern) = grep_filter {
                crate::discovery::filter_by_grep(&mut plan, pattern, false);
              }
              if self.run_with_tui_drain(plan, tui).await { break; }
              tui.set_status(crate::tui::WatchStatus::Idle);
            }
            WatchCommand::FilterByName(pattern) => {
              if !pattern.is_empty() {
                grep_filter = Some(pattern.clone());
                let mut plan = plan_factory(None);
                crate::discovery::filter_by_grep(&mut plan, &pattern, false);
                if self.run_with_tui_drain(plan, tui).await { break; }
              }
              tui.set_status(crate::tui::WatchStatus::Idle);
            }
          }
        }
      }
    }
  }

  /// Non-interactive watch: file changes only, no keyboard, normal terminal output.
  async fn run_watch_headless<F>(&mut self, watcher: &crate::watch::FileWatcher, plan_factory: &F)
  where
    F: Fn(Option<&[std::path::PathBuf]>) -> TestPlan,
  {
    // Initial run.
    let plan = plan_factory(None);
    let _ = self.run(plan).await;
    eprintln!("\n\x1b[2mWatching for changes (non-interactive)...\x1b[0m\n");

    loop {
      let Some(change) = watcher.recv().await else { break };
      let mut all_changes = vec![change];
      all_changes.extend(watcher.drain_deduped());

      let (run_all, changed_paths) = classify_changes(&all_changes);
      if !run_all && changed_paths.is_empty() {
        continue;
      }

      eprintln!("\n\x1b[2mChange detected, re-running...\x1b[0m\n");

      let plan = build_plan_for_changes(plan_factory, run_all, &changed_paths);
      if plan.total_tests == 0 {
        eprintln!("No tests matched changed files.");
        continue;
      }

      let _ = self.run(plan).await;
      eprintln!("\n\x1b[2mWatching for changes (non-interactive)...\x1b[0m\n");
    }
  }
}

/// Classify file changes into run-all vs specific changed files.
fn classify_changes(changes: &[crate::watch::ChangeKind]) -> (bool, Vec<std::path::PathBuf>) {
  use crate::watch::ChangeKind;
  let mut run_all = false;
  let mut changed_paths = Vec::new();
  for change in changes {
    match change {
      ChangeKind::SourceFile(_) | ChangeKind::StepFile(_) | ChangeKind::Config => {
        run_all = true;
      },
      ChangeKind::TestFile(p) | ChangeKind::FeatureFile(p) => {
        changed_paths.push(p.clone());
      },
    }
  }
  (run_all, changed_paths)
}

/// Build a test plan, optionally filtered to changed files.
fn build_plan_for_changes(
  plan_factory: &dyn Fn(Option<&[std::path::PathBuf]>) -> TestPlan,
  run_all: bool,
  changed_paths: &[std::path::PathBuf],
) -> TestPlan {
  let changed = if run_all { None } else { Some(changed_paths) };
  let mut plan = plan_factory(changed);

  // Filter plan to changed files if applicable.
  if !run_all && !changed_paths.is_empty() {
    let changed_names: rustc_hash::FxHashSet<&str> = changed_paths
      .iter()
      .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
      .collect();
    for suite in &mut plan.suites {
      suite
        .tests
        .retain(|t| changed_names.iter().any(|name| t.id.file.contains(name)));
    }
    plan.suites.retain(|s| !s.tests.is_empty());
    plan.total_tests = plan.suites.iter().map(|s| s.tests.len()).sum();
  }

  plan
}

/// Topologically sort projects by `dependencies`. Returns indices in execution order.
///
/// Uses Kahn's algorithm. Returns `Err` if there's a cycle or a missing dependency.
fn topo_sort_projects(projects: &[ProjectConfig]) -> Result<Vec<usize>, String> {
  let name_to_idx: FxHashMap<&str, usize> = projects.iter().enumerate().map(|(i, p)| (p.name.as_str(), i)).collect();

  // Build adjacency list + in-degree.
  let n = projects.len();
  let mut in_degree = vec![0usize; n];
  let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];

  for (i, project) in projects.iter().enumerate() {
    for dep_name in &project.dependencies {
      let &dep_idx = name_to_idx
        .get(dep_name.as_str())
        .ok_or_else(|| format!("project '{}' depends on unknown project '{dep_name}'", project.name))?;
      adj[dep_idx].push(i);
      in_degree[i] += 1;
    }
  }

  // Kahn's algorithm.
  let mut queue: std::collections::VecDeque<usize> = in_degree
    .iter()
    .enumerate()
    .filter(|(_, d)| **d == 0)
    .map(|(i, _)| i)
    .collect();

  let mut order = Vec::with_capacity(n);
  while let Some(node) = queue.pop_front() {
    order.push(node);
    for next in &adj[node] {
      in_degree[*next] -= 1;
      if in_degree[*next] == 0 {
        queue.push_back(*next);
      }
    }
  }

  if order.len() != n {
    return Err("circular dependency detected among projects".into());
  }

  Ok(order)
}

/// Filter a test plan for a specific project's scope.
///
/// Applies project-level test_match, test_dir, grep, grep_invert, and tag filters.
fn filter_plan_for_project(plan: &mut TestPlan, config: &TestConfig, project: &ProjectConfig) {
  // Filter by test_dir: only keep suites whose file starts with test_dir.
  if let Some(ref test_dir) = config.test_dir {
    plan.suites.retain(|s| s.file.starts_with(test_dir.as_str()));
  }

  // Apply project-level grep filter (already merged into config.config_grep).
  if let Some(ref grep) = config.config_grep {
    crate::discovery::filter_by_grep(plan, grep, false);
  }
  if let Some(ref grep_inv) = config.config_grep_invert {
    crate::discovery::filter_by_grep(plan, grep_inv, true);
  }

  // Apply project-level tag filter.
  if let Some(ref tags) = project.tag {
    for tag in tags {
      crate::discovery::filter_by_tag(plan, tag);
    }
  }

  // Recount after filtering.
  plan.suites.retain(|s| !s.tests.is_empty());
  plan.total_tests = plan.suites.iter().map(|s| s.tests.len()).sum();
}

fn build_launch_options(browser_config: &crate::config::BrowserConfig) -> LaunchOptions {
  // BrowserConfig is already normalized (browser↔backend consistent).
  let backend = match browser_config.backend.as_str() {
    "cdp-raw" => BackendKind::CdpRaw,
    #[cfg(target_os = "macos")]
    "webkit" => BackendKind::WebKit,
    "bidi" => BackendKind::Bidi,
    _ => BackendKind::CdpPipe,
  };

  let browser_type = match browser_config.browser.as_str() {
    "firefox" => Some(ferridriver::options::BrowserType::Firefox),
    "webkit" => Some(ferridriver::options::BrowserType::WebKit),
    "chromium" => Some(ferridriver::options::BrowserType::Chromium),
    _ => None,
  };

  let mut args = browser_config.args.clone();
  // Proxy launch args.
  if let Some(ref proxy) = browser_config.context.proxy {
    args.push(format!("--proxy-server={}", proxy.server));
    if let Some(ref bypass) = proxy.bypass {
      args.push(format!("--proxy-bypass-list={bypass}"));
    }
  }
  // Ignore HTTPS errors launch arg.
  if browser_config.context.ignore_https_errors {
    args.push("--ignore-certificate-errors".to_string());
  }

  LaunchOptions {
    backend,
    browser: browser_type,
    headless: browser_config.headless,
    executable_path: browser_config.executable_path.clone(),
    args,
    viewport: browser_config
      .viewport
      .as_ref()
      .map(|v| ferridriver::options::ViewportConfig {
        width: v.width,
        height: v.height,
        ..Default::default()
      }),
    ..Default::default()
  }
}
