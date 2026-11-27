//! Test runner orchestrator: overlaps browser launch with test dispatch,
//! handles retries with flaky detection.

use std::sync::Arc;
use std::time::Instant;

use rustc_hash::FxHashMap;
use tokio::sync::mpsc;

use crate::config::{CliOverrides, TestConfig};
use crate::dispatcher::Dispatcher;
use crate::exit;
use crate::fixture::{FixturePool, FixtureScope, builtin_fixtures, validate_dag};
use crate::model::{Hooks, TestHooks, TestPlan, TestStatus};
use crate::reporter::{EventBus, EventBusBuilder, ReporterDriver, ReporterEvent, ReporterSet};
use crate::shard;
use crate::worker::{Worker, WorkerTestResult};

use ferridriver::Browser;
use ferridriver::options::LaunchPlan;
use ferridriver::state::ConnectMode;

/// One run's event plumbing: the bus tests emit into, plus the reporters
/// draining it — the session's, and any this run added.
/// See [`TestRunner::start_run_bus`].
pub struct RunBus {
  pub bus: EventBus,
  reporters: Option<tokio::task::JoinHandle<ReporterSet>>,
  extra_reporters: Option<tokio::task::JoinHandle<ReporterSet>>,
}

mod browser;
mod projects;
#[cfg(test)]
mod tests;
mod watch_ui;

pub use browser::BrowserHandle;
use browser::{SharedBrowser, build_launch_plan, same_launch};
pub(crate) use projects::select_projects;
pub use projects::{ExecuteSummary, PlanNarrower, ProjectHooks, ProjectPlans, ProjectRun, ProjectStream};
use projects::{shuffle_work, with_shuffle_seed};
pub use watch_ui::{PlanBuild, WatchPlanFactory};

/// Top-level test runner.
pub struct TestRunner {
  config: Arc<TestConfig>,
  hooks: TestHooks,
  reporters: ReporterSet,
  overrides: CliOverrides,
  /// Shared browser for watch and UI modes (persists across runs).
  shared_browser: Option<SharedBrowser>,
  /// When set, `execute()` does not emit `RunStarted` / `RunFinished`. The
  /// multi-project orchestrator turns this on for every per-project run so a
  /// single aggregate run boundary is emitted once around all projects,
  /// rather than one pair per project (which would reset terminal counters
  /// and finalize reporters mid-run).
  suppress_run_boundary: bool,
  /// Cooperative cancel for an in-flight `execute` (UI Stop).
  run_stop: RunStop,
  /// Something is watching this run's traces as they are recorded, so
  /// events are flushed per line instead of buffered.
  live_traces: bool,
  /// Plans discovered per project, when discovery itself differs per
  /// project rather than the projects narrowing one shared plan.
  ///
  /// Spec files are discovered once and narrowed — a project's `testDir`
  /// and `grep` select from the same corpus. BDD is not like that: a
  /// project names its own feature globs, its own step graph and its own
  /// tag expression, so its scenarios are a different discovery, not a
  /// subset of someone else's. Empty for every other host, which then
  /// pays nothing for this.
  /// Shared, so a watch or UI cycle can refresh it in place: the plan
  /// factory those loops call hands back one plan, and rebinding a field
  /// on the runner from inside the closure is not something its
  /// signature allows.
  project_plans: ProjectPlans,
  /// Worker numbers for the run in flight, shared with every project of
  /// it — see the reservation in [`Self::execute_with_summary`].
  worker_ids: Arc<std::sync::atomic::AtomicU32>,
  /// The run's worker slots when projects share them; a runner on its
  /// own makes its own.
  run_slots: Option<Arc<crate::dispatcher::RunSlots>>,
  /// The project's roster, when the run that started it ranked it
  /// before it began.
  project_roster: Option<Arc<crate::dispatcher::WorkerRoster>>,
  /// Test durations from earlier runs, shared by every project of a run.
  timings: Option<Arc<crate::timings::Timings>>,
}

/// Drop and annotate what a reporter's `preprocess` asked for, under
/// `project_name`. A case is named by its stable id under that project,
/// so excluding a test in one project leaves the same test running in
/// another.
///
/// Applied after the run's own filters, which means an exclusion does
/// not rebalance `--shard`. Playwright's answer to that is
/// `TestRun.skipSharding()`, which a reporter that reshapes the corpus
/// is expected to call and which the runner honours.
fn apply_run_edits(plan: &mut TestPlan, project_name: &str, edits: &crate::reporter::TestRunEdits) {
  if edits.excluded.is_empty() && edits.annotations.is_empty() {
    return;
  }
  for suite in &mut plan.suites {
    suite
      .tests
      .retain(|test| !edits.excluded.contains(&test.id.stable_id(project_name)));
    for test in &mut suite.tests {
      let id = test.id.stable_id(project_name);
      for (target, annotation) in &edits.annotations {
        if *target == id {
          test.annotations.push(annotation.clone());
        }
      }
    }
  }
  plan.total_tests = plan.suites.iter().map(|suite| suite.tests.len()).sum();
}

/// Playwright's `onEnd` returning `{ status }`: a reporter may decide
/// how the run ended, and the exit code follows it. Read once, after
/// the reporters have drained and finalized.
fn apply_status_override(reporters: &ReporterSet, exit_code: i32) -> i32 {
  match reporters.status_override() {
    Some(crate::reporter::RunStatus::Passed) => exit::PASSED,
    Some(_) => exit::worst(exit_code, exit::FAILED),
    None => exit_code,
  }
}

/// Cooperative cancel signal for an in-flight [`TestRunner::execute`].
/// Requesting a stop trips the dispatcher's hard-stop: workers drop
/// queued items and exit after their current test, and `execute`
/// unwinds normally — contexts close, traces stop, live-trace entries
/// unregister. Dropping the `execute` future instead would detach the
/// spawned worker tasks: the in-flight tests would keep driving the
/// shared browser behind an "idle" UI and overlap the next run.
#[derive(Clone, Default)]
pub struct RunStop {
  requested: Arc<std::sync::atomic::AtomicBool>,
  notify: Arc<tokio::sync::Notify>,
}

impl RunStop {
  /// Ask the in-flight run to stop after the tests currently executing.
  pub fn request(&self) {
    self.requested.store(true, std::sync::atomic::Ordering::SeqCst);
    // Every project running at once waits on this stop, and each has to
    // see it: `notify_one` woke one project and left the rest running
    // their queues out behind a UI that had stopped.
    self.notify.notify_waiters();
  }

  fn reset(&self) {
    self.requested.store(false, std::sync::atomic::Ordering::SeqCst);
  }

  /// Whether a stop was asked for — the difference between a run that
  /// finished and one that was cut short.
  fn is_requested(&self) -> bool {
    self.requested.load(std::sync::atomic::Ordering::SeqCst)
  }

  async fn wait(&self) {
    loop {
      let notified = self.notify.notified();
      tokio::pin!(notified);
      // Registered before the flag is read, so a request landing between
      // the read and the await still wakes this waiter.
      notified.as_mut().enable();
      if self.requested.load(std::sync::atomic::Ordering::SeqCst) {
        return;
      }
      notified.await;
    }
  }
}

impl TestRunner {
  /// Build a runner with no programmatic suite hooks. For runners that need
  /// `before_all` / `after_all` closures, use [`TestRunner::with_hooks`].
  #[must_use]
  pub fn new(config: TestConfig, overrides: CliOverrides) -> Self {
    Self::with_hooks(config, TestHooks::default(), overrides)
  }

  /// Build a runner with programmatic suite hooks supplied at construction.
  #[must_use]
  pub fn with_hooks(config: TestConfig, hooks: TestHooks, overrides: CliOverrides) -> Self {
    let reporters = crate::reporter::create_reporters(&config.reporter, &config);
    Self {
      config: Arc::new(config),
      hooks,
      reporters,
      overrides,
      shared_browser: None,
      suppress_run_boundary: false,
      run_stop: RunStop::default(),
      live_traces: false,
      project_plans: ProjectPlans::default(),
      worker_ids: Arc::new(std::sync::atomic::AtomicU32::new(0)),
      run_slots: None,
      project_roster: None,
      timings: None,
    }
  }

  /// Give each named project the plan it runs, replacing the shared one.
  ///
  /// A project with no entry here still narrows the plan passed to
  /// [`Self::run`], so a host can mix the two — which is what keeps a
  /// config that declares no per-project discovery on exactly the path
  /// it was on before, building one plan and one bundle.
  pub fn set_project_plans(&self, plans: FxHashMap<String, TestPlan>) {
    self.project_plans.set(plans);
  }

  /// The store [`Self::set_project_plans`] writes to, so a watch cycle's
  /// plan factory can refresh it as the corpus changes on disk.
  #[must_use]
  pub fn project_plans(&self) -> ProjectPlans {
    self.project_plans.clone()
  }

  /// Playwright's `Reporter.preprocess`: hand every reporter the corpus
  /// before the run and apply what they ask for.
  ///
  /// The view is the projects this run will EXECUTE. Playwright also
  /// prepends the unfiltered dependency projects and marks them
  /// read-only so a reporter cannot edit a setup project; here they are
  /// simply not in the tree, which enforces the same rule by absence.
  ///
  /// Returns the edits, so each project can drop what was excluded
  /// under ITS name — the plan is shared across projects, and the same
  /// test excluded in one project still runs in another.
  async fn preprocess_corpus(&mut self, plan: &TestPlan) -> Result<crate::reporter::TestRunEdits, String> {
    let mut edits = crate::reporter::TestRunEdits::default();
    if self.reporters.is_empty() {
      return Ok(edits);
    }
    let runs: Vec<ProjectRun> = if self.config.projects.is_empty() {
      self.project_runs()
    } else {
      let selected = select_projects(
        &self.config.projects,
        &self.overrides.project_filter,
        self.overrides.include_non_default_projects,
      )?;
      self
        .project_runs()
        .into_iter()
        .enumerate()
        .filter(|(idx, _)| selected.contains(idx))
        .map(|(_, run)| run)
        .collect()
    };
    let narrowed: Vec<TestPlan> = runs
      .iter()
      .map(|run| {
        let mut narrowed = run.narrow(plan);
        self.apply_run_filters(&mut narrowed, &run.name)?;
        Ok(narrowed)
      })
      .collect::<Result<_, String>>()?;
    let project_plans: Vec<crate::reporter::api::ProjectPlan<'_>> = runs
      .iter()
      .zip(narrowed.iter())
      .map(|(run, plan)| crate::reporter::api::ProjectPlan {
        name: run.name.as_str(),
        config: run.config.as_ref(),
        project: run.project_config(),
        plan,
      })
      .collect();
    let preamble = crate::reporter::api::RunPreamble::build(&self.config, &project_plans);
    self.reporters.preprocess(&preamble, &mut edits).await?;
    if edits.skip_sharding {
      // The reporter has taken sharding over itself.
      self.overrides.shard = None;
    }
    Ok(edits)
  }

  /// Handle for cancelling an in-flight `execute` cooperatively (see
  /// [`RunStop`]).
  #[must_use]
  pub fn stop_handle(&self) -> RunStop {
    self.run_stop.clone()
  }

  /// Append an additional reporter after construction (e.g., NAPI `ResultCollector`).
  pub fn add_reporter(&mut self, reporter: Box<dyn crate::reporter::Reporter>) {
    self.reporters.add(reporter);
  }

  /// The resolved configuration this runner runs with.
  #[must_use]
  pub fn config(&self) -> &TestConfig {
    &self.config
  }

  /// The command-line choices this runner was built with.
  #[must_use]
  pub fn overrides(&self) -> &CliOverrides {
    &self.overrides
  }

  /// A runner for one caller-driven run: this runner's browser, hooks
  /// and stop signal under a different config and overrides. Reporters
  /// stay with this runner — the caller owns the run's event bus.
  #[must_use]
  pub fn with_run_options(&self, config: Arc<TestConfig>, overrides: CliOverrides) -> Self {
    Self {
      config,
      hooks: self.hooks.clone(),
      reporters: ReporterSet::default(),
      overrides,
      shared_browser: self.shared_browser.clone(),
      suppress_run_boundary: self.suppress_run_boundary,
      run_stop: self.run_stop.clone(),
      live_traces: self.live_traces,
      project_plans: self.project_plans.clone(),
      worker_ids: Arc::clone(&self.worker_ids),
      run_slots: self.run_slots.clone(),
      project_roster: None,
      timings: self.timings.clone(),
    }
  }

  /// The projects this runner covers: its `[[test.projects]]` merged
  /// onto the config, or the config itself when it declares none.
  #[must_use]
  pub fn project_runs(&self) -> Vec<ProjectRun> {
    if self.config.projects.is_empty() {
      return vec![ProjectRun {
        name: self.config.name.clone().unwrap_or_default(),
        config: Arc::clone(&self.config),
        project: None,
        plan: None,
      }];
    }
    self
      .config
      .projects
      .iter()
      .map(|project| ProjectRun {
        name: project.name.clone(),
        config: Arc::new(self.config.merge_project(project)),
        project: Some(project.clone()),
        plan: self.project_plans.get(&project.name),
      })
      .collect()
  }

  /// Cancel the run in flight (the UI's Stop). Workers drop what is
  /// queued and finish what they started, so `execute` still unwinds
  /// cleanly.
  pub fn request_stop(&self) {
    self.run_stop.request();
  }

  /// Clear a previous cancellation before starting a new run.
  pub fn reset_stop(&self) {
    self.run_stop.reset();
  }

  /// Follow this run's traces as they are recorded (a UI is watching).
  pub fn set_live_traces(&mut self, live: bool) {
    self.live_traces = live;
  }

  /// Build the event bus for one run, wiring the configured reporters
  /// plus any this run adds (`runTests({ reporters })`).
  ///
  /// The configured reporters are taken for the duration and handed back
  /// by [`Self::finish_run_bus`] — the same take/restore the watch loop
  /// does, so reporter state survives across runs. `extra` belongs to
  /// the one run: it is finalized and dropped with it.
  ///
  /// A caller that watches the stream itself subscribes per project
  /// instead (see [`ProjectStream`]), so this takes no subscriber.
  pub fn start_run_bus(&mut self, extra: ReporterSet) -> RunBus {
    let mut builder = EventBusBuilder::new();
    let reporters = (!self.reporters.is_empty()).then(|| {
      let subscription = builder.subscribe();
      let reporters = std::mem::take(&mut self.reporters);
      tokio::spawn(ReporterDriver::new(reporters, subscription).run())
    });
    let extra_reporters = (!extra.is_empty()).then(|| {
      let subscription = builder.subscribe();
      tokio::spawn(ReporterDriver::new(extra, subscription).run())
    });
    RunBus {
      bus: builder.build(),
      reporters,
      extra_reporters,
    }
  }

  /// Close a run's bus, wait for its reporters to drain, and take the
  /// configured ones back.
  pub async fn finish_run_bus(&mut self, run: RunBus) {
    run.bus.close();
    if let Some(reporters) = run.reporters
      && let Ok(reporters) = reporters.await
    {
      self.reporters = reporters;
    }
    if let Some(extra) = run.extra_reporters {
      let _ = extra.await;
    }
  }

  /// Publish the configured `baseUrl` through
  /// [`crate::config::run_base_url`] so
  /// URL-resolving consumers outside the config path (BDD step
  /// definitions) see it. When no `baseUrl` is configured, the
  /// webServer startup path exports the first server's URL instead.
  fn publish_base_url(&self) {
    if let Some(url) = &self.config.base_url {
      crate::config::publish_run_base_url(url);
    }
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
    if let Err(message) = self.validate_selection(&plan) {
      eprintln!("Error: {message}");
      return exit::USAGE;
    }
    if self.overrides.list_only {
      return self.print_list(&plan);
    }
    // Playwright's `config.expect.timeout`: make the configured default
    // visible to every bare `expect()` in this process. A project with
    // its own `expect` block narrows it again per test, in the worker.
    ferridriver_expect::set_default_expect_timeout(std::time::Duration::from_millis(
      self.config.resolved_expect(None).timeout_ms(),
    ));
    ferridriver::actions::set_stable_check(match self.config.stable_check {
      ferridriver_config::test::StableCheck::Playwright => ferridriver::actions::StableCheck::Playwright,
      ferridriver_config::test::StableCheck::Frame => ferridriver::actions::StableCheck::Frame,
    });
    self.publish_base_url();
    // A fresh run numbers its workers from zero again (watch and UI
    // sessions run many).
    self.worker_ids.store(0, std::sync::atomic::Ordering::SeqCst);
    let global_timeout = self.config.global_timeout;
    let inner = async move {
      // ── Multi-project path ──
      if !self.config.projects.is_empty() {
        return Box::pin(self.run_projects(plan)).await;
      }

      // ── Single-project path ──
      let mut plan = plan;
      plan.expand_repetitions(self.config.repeat_each);
      match self.preprocess_corpus(&plan).await {
        Ok(edits) => {
          let project = self.config.name.clone().unwrap_or_default();
          apply_run_edits(&mut plan, &project, &edits);
        },
        Err(message) => {
          eprintln!("Error: reporter preprocess failed: {message}");
          return exit::FAILED;
        },
      }

      let mut builder = EventBusBuilder::new();
      let driver_handle = if self.reporters.is_empty() {
        None
      } else {
        let reporter_sub = builder.subscribe();
        let reporters = std::mem::take(&mut self.reporters);
        let driver = ReporterDriver::new(reporters, reporter_sub);
        Some(tokio::spawn(driver.run()))
      };
      let bus = builder.build();

      let exit_code = self.execute(plan, bus.clone()).await;

      // Explicitly close senders so the driver's recv() returns None.
      // Cannot rely on Drop — tokio::spawn defers task deallocation,
      // keeping Arc<EventBusInner> alive after JoinHandle::await returns.
      bus.close();

      if let Some(driver_handle) = driver_handle
        && let Ok(reporters) = driver_handle.await
      {
        self.reporters = reporters;
      }

      apply_status_override(&self.reporters, exit_code)
    };

    if global_timeout > 0 {
      if let Ok(code) = tokio::time::timeout(std::time::Duration::from_millis(global_timeout), inner).await {
        code
      } else {
        tracing::error!(
          target: "ferridriver::runner",
          global_timeout_ms = global_timeout,
          "global timeout exceeded — aborting run",
        );
        eprintln!("Error: global timeout of {global_timeout}ms exceeded");
        exit::FAILED
      }
    } else {
      inner.await
    }
  }

  /// [`Self::run`] for a one-shot command. The first SIGINT or SIGTERM
  /// stops the run after the tests in flight, which then reports as
  /// interrupted and exits [`exit::INTERRUPTED`]; a second exits at once.
  ///
  /// Not part of `run` itself: once a signal handler is installed the
  /// default disposition never comes back, and a watch loop between runs
  /// must still die on Ctrl+C.
  pub async fn run_with_signals(&mut self, plan: TestPlan) -> i32 {
    let stop = self.run_stop.clone();
    let listener = tokio::spawn(async move {
      wait_for_termination().await;
      eprintln!("\nInterrupted: finishing the tests in flight (interrupt again to stop now)");
      stop.request();
      wait_for_termination().await;
      std::process::exit(exit::INTERRUPTED);
    });
    let code = Box::pin(self.run(plan)).await;
    listener.abort();
    code
  }

  /// Core execution engine. Emits events on the provided `EventBus`.
  ///
  /// Takes `&self` — no reporter ownership, no mutable state. The caller
  /// controls who subscribes to the bus (reporters, TUI, external consumers).
  ///
  /// The bus is consumed by value and dropped when execution completes,
  /// closing all subscriber channels and signaling consumers to finalize.
  pub async fn execute(&self, plan: TestPlan, event_bus: EventBus) -> i32 {
    self.execute_with_summary(plan, event_bus).await.exit_code
  }

  /// Narrow `plan` by what the command line and the config asked for
  /// (test ids, locations, shard, grep, tag), with ids computed under
  /// `project`. Applied by every `execute`, and by the multi-project
  /// orchestrator when it counts the run — a total that ignored `--grep`
  /// reported more tests than ever ran.
  fn apply_run_filters(&self, plan: &mut TestPlan, project: &str) -> Result<(), String> {
    if !self.overrides.test_ids.is_empty() {
      crate::discovery::filter_by_test_ids(plan, project, &self.overrides.test_ids);
    }
    // The positional files, as Playwright filters a project's files by
    // them. A file:line argument narrows its file to those lines below.
    if !self.overrides.test_files.is_empty() {
      let cwd = std::env::current_dir().unwrap_or_default();
      let files = crate::discovery::FileFilter::new(&self.overrides.test_files, &cwd);
      crate::discovery::filter_by_file(plan, |file| files.matches(file));
    }
    if !self.overrides.locations.is_empty() {
      crate::discovery::filter_by_locations(plan, &self.overrides.locations);
    }
    if let Some(shard_arg) = &self.overrides.shard {
      shard::filter_by_shard(
        plan,
        &crate::model::ShardInfo {
          current: shard_arg.current,
          total: shard_arg.total,
        },
        project,
        self.overrides.shard_assignment.as_ref(),
      )?;
    }
    // Apply grep: CLI overrides take precedence, then config-level grep.
    let grep = self.overrides.grep.as_ref().or(self.config.config_grep.as_ref());
    let grep_inv = self
      .overrides
      .grep_invert
      .as_ref()
      .or(self.config.config_grep_invert.as_ref());
    if let Some(grep) = grep {
      crate::discovery::filter_by_grep(plan, grep, false);
    }
    if let Some(grep_inv) = grep_inv {
      crate::discovery::filter_by_grep(plan, grep_inv, true);
    }
    if let Some(tag) = &self.overrides.tag {
      crate::discovery::filter_by_tag(plan, tag);
    }
    Ok(())
  }

  /// Everything that decides which tests of `plan` run, in the order
  /// `execute` applies it: the run filters, `repeatEach`, `.only` and
  /// `--last-failed`. `--list` goes through exactly this, so what it
  /// prints is what a run with the same flags executes.
  fn select_tests(&self, plan: &mut TestPlan, project: &str) -> Result<(), String> {
    self.apply_run_filters(plan, project)?;
    plan.expand_repetitions(self.config.repeat_each);
    crate::discovery::filter_by_only(plan);
    if self.overrides.last_failed {
      let rerun_path = self.config.output_dir.join("@rerun.txt");
      crate::discovery::filter_by_rerun(plan, &rerun_path);
    }
    Ok(())
  }

  /// Core execution engine, returning the full per-run tally. `execute()` is
  /// the thin `i32` wrapper; the multi-project orchestrator uses the summary
  /// to aggregate counts across concurrently-run projects.
  #[tracing::instrument(skip_all, fields(workers = self.config.worker_count(), tests = plan.total_tests))]
  pub async fn execute_with_summary(&self, mut plan: TestPlan, event_bus: EventBus) -> ExecuteSummary {
    let project = self.config.name.clone().unwrap_or_default();
    if let Err(summary) = self.prepare_plan(&mut plan, &project) {
      return summary;
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
      return ExecuteSummary::default();
    }

    // Never launch more workers than tests — extra workers launch browsers for nothing.
    let num_workers = self
      .config
      .worker_count()
      .min(u32::try_from(total_tests).unwrap_or(u32::MAX))
      .max(1);

    // Custom `#[fixture]` definitions, collected once and seeded into every
    // worker's fixture pool so tests can resolve them via `ctx.get`.
    let custom_fixtures = crate::discovery::collect_rust_fixtures();
    if let Err(summary) = self.check_fixtures(&custom_fixtures, &event_bus, total_tests) {
      return summary;
    }

    // ── Web server lifecycle ──
    // Follows Playwright's pattern: start servers, publish the base URL.
    let web_server_manager = match self.start_web_servers(&event_bus, total_tests).await {
      Ok(manager) => manager,
      Err(summary) => return summary,
    };

    let reporting_enabled = event_bus.has_subscribers();
    // Boundary events (`RunStarted` / `RunFinished`) are emitted once per
    // `execute()` for the single-project path, but suppressed when the
    // multi-project orchestrator drives many `execute()` calls into one
    // shared bus — it emits a single aggregate boundary itself.
    let emit_boundary = reporting_enabled && !self.suppress_run_boundary;
    if emit_boundary {
      let project_plans = [crate::reporter::api::ProjectPlan {
        name: self.config.name.as_deref().unwrap_or_default(),
        config: &self.config,
        project: None,
        plan: &plan,
      }];
      event_bus.emit(ReporterEvent::RunStarted {
        total_tests,
        num_workers,
        metadata: self.run_metadata(),
        start_time: std::time::SystemTime::now(),
        preamble: Arc::new(crate::reporter::api::RunPreamble::build(&self.config, &project_plans)),
      });
    }

    let start = Instant::now();

    // ── Global setup ──
    if let Err(e) = self.run_global_setup().await {
      Self::report_global_setup_failure(e, &event_bus, emit_boundary, total_tests, start);
      return ExecuteSummary {
        exit_code: 1,
        total: total_tests,
        failed: total_tests,
        ..Default::default()
      };
    }

    let workers = match Box::pin(self.run_workers(&plan, &event_bus, custom_fixtures, num_workers, &project)).await {
      Ok(workers) => workers,
      Err(summary) => return summary,
    };

    // ── Global teardown (always runs, even if tests failed) ──
    self.run_global_teardown().await;

    let duration = start.elapsed();
    let counts = RunCounts::of(&workers.attempt_history);
    self.drop_passing_output(&workers.attempt_history);

    // ── Web server teardown ──
    if let Some(mgr) = web_server_manager {
      mgr.stop().await;
    }

    // The loose trace files were only interesting while their tests were
    // running; the ones worth keeping are zipped into each test's own
    // output directory by now. Outermost runner only, for the reason
    // above.
    if !self.suppress_run_boundary {
      crate::artifacts::sweep(&self.config.output_dir);
    }

    if emit_boundary {
      event_bus.emit(ReporterEvent::RunFinished {
        total: total_tests,
        passed: counts.passed,
        failed: counts.failed,
        skipped: counts.skipped,
        flaky: counts.flaky,
        duration,
        status: self.run_status(counts.failed > 0 || workers.infrastructure_failed),
      });
    }

    ExecuteSummary {
      exit_code: self.run_exit_code(&counts, workers.infrastructure_failed || workers.launch_failed),
      total: total_tests,
      passed: counts.passed,
      failed: counts.failed,
      skipped: counts.skipped,
      flaky: counts.flaky,
    }
  }

  /// Queue the plan's work and run it on workers that come and go with
  /// it, collecting every result.
  ///
  /// # Errors
  ///
  /// Returns the summary of a run whose browser selection or connection
  /// is invalid; nothing ran.
  async fn run_workers(
    &self,
    plan: &TestPlan,
    event_bus: &EventBus,
    custom_fixtures: FxHashMap<String, crate::fixture::FixtureDef>,
    num_workers: u32,
    project: &str,
  ) -> Result<WorkersRun, ExecuteSummary> {
    let (launch_plan, connection) = match self.browser_launch() {
      Ok(launch) => launch,
      Err(message) => {
        eprintln!("Error: {message}");
        return Err(ExecuteSummary {
          exit_code: exit::USAGE,
          ..ExecuteSummary::default()
        });
      },
    };

    let dispatcher = Arc::new(Dispatcher::new());
    let (timings, owns_timings) = match &self.timings {
      Some(timings) => (Arc::clone(timings), false),
      None => (
        Arc::new(crate::timings::Timings::load(&crate::identity::root(&self.config))),
        true,
      ),
    };
    let (slots, roster) = self.queue_work(Self::work_units(plan), &dispatcher, &timings, num_workers);

    // ── Spawn workers with lazy browser launch ──
    // Each worker holds a `BrowserHandle` that launches the browser on first
    // fixture access. Tests that never resolve `browser`/`context`/`page`
    // (config-only tests, request-only tests) skip the launch entirely —
    // critical in CI where Chromium's first-launch can exceed 30s.
    let (result_tx, result_rx) = mpsc::channel::<WorkerTestResult>(256);
    let shared_browser = self.matching_shared_browser(&launch_plan, &connection);
    let launch_failed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (workers_per_browser, run_shared_handles) = shared_browser_pool(
      &launch_plan,
      &connection,
      num_workers,
      shared_browser.is_some(),
      self.config.context_reuse,
      &launch_failed,
    );
    let spawner = WorkerSpawner {
      config: Arc::clone(&self.config),
      event_bus: event_bus.has_subscribers().then(|| event_bus.clone()),
      live_traces: self.live_traces,
      worker_ids: Arc::clone(&self.worker_ids),
      project_worker_seq: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
      workers_per_browser,
      dispatcher: Arc::clone(&dispatcher),
      result_tx,
      custom_fixtures,
      shared_browser,
      run_shared_handles: run_shared_handles.clone(),
      launch_plan,
      connection,
      // Set by any worker whose browser failed to launch: the tests it
      // could not run fail, and the run reports an infrastructure failure
      // rather than a verdict on them.
      launch_failed,
      slots,
      roster: Arc::clone(&roster),
    };
    let mut worker_handles: Vec<_> = roster
      .enlist(&dispatcher.receiver())
      .into_iter()
      .map(|place| spawner.spawn(place))
      .collect();

    let attempt_history = Box::pin(self.collect_results(
      ResultCollection {
        dispatcher: &dispatcher,
        roster: &roster,
        timings: &timings,
        project,
        total_tests: plan.total_tests,
        spawner: &spawner,
      },
      result_rx,
      &mut worker_handles,
    ))
    .await;

    if owns_timings && let Err(error) = timings.save() {
      tracing::warn!(target: "ferridriver::runner", "could not keep test durations: {error}");
    }

    let infrastructure_failed = join_workers(worker_handles, event_bus).await;
    for handle in run_shared_handles {
      if let Err(error) = handle.close().await {
        tracing::warn!(target: "ferridriver::runner", "shared browser cleanup failed: {error}");
      }
    }
    Ok(WorkersRun {
      attempt_history,
      infrastructure_failed,
      launch_failed: spawner.launch_failed.load(std::sync::atomic::Ordering::SeqCst),
    })
  }

  /// Narrow `plan` to what this run executes, after the `--forbid-only`
  /// check, and clear what a fresh run starts without.
  ///
  /// # Errors
  ///
  /// Returns the summary of a run that cannot start: a filter that does
  /// not apply, or an `.only` that `--forbid-only` refuses.
  fn prepare_plan(&self, plan: &mut TestPlan, project: &str) -> Result<(), ExecuteSummary> {
    let refused = |exit_code| ExecuteSummary {
      exit_code,
      ..Default::default()
    };
    // ── Forbid-only check, before `.only` narrows the plan ──
    if self.config.forbid_only || self.overrides.forbid_only {
      let mut candidates = plan.clone();
      if let Err(message) = self.apply_run_filters(&mut candidates, project) {
        eprintln!("Error: {message}");
        return Err(refused(exit::USAGE));
      }
      if let Err(e) = crate::discovery::check_forbid_only(&candidates) {
        eprint!("{e}");
        return Err(refused(exit::FAILED));
      }
    }

    if let Err(message) = self.select_tests(plan, project) {
      eprintln!("Error: {message}");
      return Err(refused(exit::USAGE));
    }

    // ── preserve_output: "never" — wipe output_dir at run start ──
    if self.config.preserve_output == "never" && !self.suppress_run_boundary {
      let _ = std::fs::remove_dir_all(&self.config.output_dir);
    }

    // Worker scratch directories are per-run: whatever is left is from a
    // run that was killed before it could zip its traces. Only the
    // outermost runner may do this — projects run concurrently and share
    // one output directory, so a project sweeping here would delete the
    // trace another project is still writing.
    if !self.suppress_run_boundary {
      crate::artifacts::sweep(&self.config.output_dir);
    }
    Ok(())
  }

  /// Validate the fixture graph the run's tests will resolve against.
  ///
  /// # Errors
  ///
  /// Returns the summary of a run that cannot start: a browser selection
  /// the built-in fixtures reject, or fixtures that do not form a DAG.
  fn check_fixtures(
    &self,
    custom_fixtures: &FxHashMap<String, crate::fixture::FixtureDef>,
    event_bus: &EventBus,
    total_tests: usize,
  ) -> Result<(), ExecuteSummary> {
    let mut fixture_defs = match builtin_fixtures(&self.config.browser) {
      Ok(definitions) => definitions,
      Err(error) => {
        tracing::error!("invalid browser selection: {error}");
        eprintln!("Error: invalid browser selection: {error}");
        return Err(ExecuteSummary {
          exit_code: exit::USAGE,
          total: total_tests,
          failed: total_tests,
          ..Default::default()
        });
      },
    };
    for (name, def) in custom_fixtures {
      fixture_defs.insert(name.clone(), def.clone());
    }
    if let Err(e) = validate_dag(&fixture_defs) {
      tracing::error!(target: "ferridriver::fixture", "fixture DAG error: {e}");
      // Nothing has run yet; without this the run exits non-zero with
      // no output at all, the reason only reachable through RUST_LOG.
      event_bus.emit(ReporterEvent::RunError {
        error: Box::new(crate::model::TestFailure::msg(format!("Invalid fixtures: {e}"))),
      });
      return Err(ExecuteSummary {
        exit_code: exit::FAILED,
        total: total_tests,
        failed: total_tests,
        ..Default::default()
      });
    }
    Ok(())
  }

  /// Start the configured web servers, publishing the first one's URL as
  /// the base URL when none is configured.
  ///
  /// # Errors
  ///
  /// Returns the summary of a run whose servers did not come up; the
  /// failure is reported on `event_bus`.
  async fn start_web_servers(
    &self,
    event_bus: &EventBus,
    total_tests: usize,
  ) -> Result<Option<crate::server::WebServerManager>, ExecuteSummary> {
    if self.config.web_server.is_empty() {
      return Ok(None);
    }
    match crate::server::WebServerManager::start(&self.config.web_server).await {
      Ok(mgr) => {
        if let Some(url) = mgr.first_url()
          && self.config.base_url.is_none()
        {
          crate::config::publish_run_base_url(&url);
          tracing::info!(target: "ferridriver::runner", "webServer base_url={url}");
        }
        Ok(Some(mgr))
      },
      Err(e) => {
        tracing::error!(target: "ferridriver::runner", "webServer start failed: {e}");
        // Before the run boundary: only the error itself is known,
        // and a report with no tests and this error beats a silent one.
        event_bus.emit(ReporterEvent::RunError {
          error: Box::new(crate::model::TestFailure::msg(format!("webServer start failed: {e}"))),
        });
        Err(ExecuteSummary {
          exit_code: exit::INFRASTRUCTURE,
          total: total_tests,
          failed: total_tests,
          ..Default::default()
        })
      },
    }
  }

  /// The run's `metadata`, with the shuffle seed and, per
  /// `captureGitInfo`, the git state.
  fn run_metadata(&self) -> serde_json::Value {
    let mut run_metadata = with_shuffle_seed(self.config.metadata.clone(), self.overrides.shuffle.as_deref());
    if self.config.capture_git_info {
      let info = crate::git_info::GitInfo::capture();
      let git_value = serde_json::to_value(&info).unwrap_or(serde_json::Value::Null);
      match &mut run_metadata {
        serde_json::Value::Object(map) => {
          map.insert("git".into(), git_value);
        },
        other => {
          *other = serde_json::json!({ "git": git_value });
        },
      }
    }
    run_metadata
  }

  /// Run every global setup function in order.
  ///
  /// # Errors
  ///
  /// Returns the first setup failure; the ones after it do not run.
  async fn run_global_setup(&self) -> Result<(), crate::model::TestFailure> {
    if self.hooks.global_setup_fns.is_empty() {
      return Ok(());
    }
    let global_pool = FixturePool::new(FxHashMap::default(), FixtureScope::Global);
    for setup_fn in &self.hooks.global_setup_fns {
      setup_fn(global_pool.clone()).await?;
    }
    Ok(())
  }

  /// Report a global setup failure: logged, and on the run's own
  /// boundary as a run error and a run of nothing but failures.
  fn report_global_setup_failure(
    e: crate::model::TestFailure,
    event_bus: &EventBus,
    emit_boundary: bool,
    total_tests: usize,
    start: Instant,
  ) {
    tracing::error!(target: "ferridriver::runner", "global setup failed: {e}");
    if !emit_boundary {
      return;
    }
    // A setup failure belongs to no test; without this channel
    // every report would show a run of zero failures.
    event_bus.emit(ReporterEvent::RunError {
      error: Box::new(crate::model::TestFailure {
        message: format!("global setup failed: {e}"),
        stack: e.stack,
        diff: None,
        screenshot: None,
        error_context: None,
      }),
    });
    event_bus.emit(ReporterEvent::RunFinished {
      total: total_tests,
      passed: 0,
      failed: total_tests,
      skipped: 0,
      flaky: 0,
      duration: start.elapsed(),
      status: crate::reporter::RunStatus::Failed,
    });
  }

  /// How a run that has finished reads: failed, stopped, or passed.
  fn run_status(&self, failed: bool) -> crate::reporter::RunStatus {
    if failed {
      crate::reporter::RunStatus::Failed
    } else if self.run_stop.is_requested() {
      crate::reporter::RunStatus::Interrupted
    } else {
      crate::reporter::RunStatus::Passed
    }
  }

  /// Run every global teardown function, logging the ones that fail.
  async fn run_global_teardown(&self) {
    if self.hooks.global_teardown_fns.is_empty() {
      return;
    }
    let global_pool = FixturePool::new(FxHashMap::default(), FixtureScope::Global);
    for teardown_fn in &self.hooks.global_teardown_fns {
      if let Err(e) = teardown_fn(global_pool.clone()).await {
        tracing::error!(target: "ferridriver::runner", "global teardown error: {e}");
      }
    }
  }

  /// The plan as work items: one per parallel test, one batch per serial
  /// group.
  fn work_units(plan: &TestPlan) -> Vec<crate::dispatcher::WorkItem> {
    // ── Dispatcher — enqueue suites with hooks + mode context ──
    let mut units: Vec<crate::dispatcher::WorkItem> = Vec::new();
    // Serial suites sharing a scope (a `describe` nested in a serial one)
    // are one group: one batch, one worker, in declaration order.
    let mut serial_batches: FxHashMap<(String, String, u32), usize> = FxHashMap::default();
    let mut merged_batches: Vec<usize> = Vec::new();
    for suite in &plan.suites {
      let repeat_each_index = suite.tests.first().map_or(0, |test| test.id.repeat_each_index);
      let suite_key = format!("{}::{}::repeat:{repeat_each_index}", suite.file, suite.name);
      let hooks = Arc::new(Hooks {
        before_all: suite.hooks.before_all.clone(),
        after_all: suite.hooks.after_all.clone(),
        before_each: suite.hooks.before_each.clone(),
        after_each: suite.hooks.after_each.clone(),
      });
      let assign = |mode: crate::model::SuiteMode| {
        suite
          .tests
          .iter()
          .map(|test| crate::dispatcher::TestAssignment {
            test: test.clone(),
            attempt: 1,
            suite_key: suite_key.clone(),
            hooks: Arc::clone(&hooks),
            suite_mode: mode,
          })
          .collect::<Vec<_>>()
      };

      match suite.serial_label() {
        None => {
          units.extend(
            assign(crate::model::SuiteMode::Parallel)
              .into_iter()
              .map(|assignment| crate::dispatcher::WorkItem::Single(Box::new(assignment))),
          );
        },
        Some(label) => {
          let assignments = assign(crate::model::SuiteMode::Serial);
          let group = (suite.file.clone(), label.to_string(), repeat_each_index);
          if let Some(&index) = serial_batches.get(&group)
            && let Some(crate::dispatcher::WorkItem::Serial(batch)) = units.get_mut(index)
          {
            batch.assignments.extend(assignments);
            merged_batches.push(index);
          } else {
            serial_batches.insert(group, units.len());
            units.push(crate::dispatcher::WorkItem::Serial(crate::dispatcher::SerialBatch {
              suite_key: suite_key.clone(),
              assignments,
              hooks: Arc::clone(&hooks),
            }));
          }
        },
      }
    }
    for index in merged_batches {
      if let Some(crate::dispatcher::WorkItem::Serial(batch)) = units.get_mut(index) {
        batch
          .assignments
          .sort_by_key(|assignment| assignment.test.id.line.unwrap_or(usize::MAX));
      }
    }
    units
  }

  /// Order the work (shuffled, or longest first) and queue it on
  /// `dispatcher` through this run's roster, which is returned with the
  /// slots its workers draw from.
  fn queue_work(
    &self,
    mut units: Vec<crate::dispatcher::WorkItem>,
    dispatcher: &Dispatcher,
    timings: &crate::timings::Timings,
    num_workers: u32,
  ) -> (Arc<crate::dispatcher::RunSlots>, Arc<crate::dispatcher::WorkerRoster>) {
    let project = self.config.name.clone().unwrap_or_default();
    if let Some(seed) = &self.overrides.shuffle {
      shuffle_work(&mut units, seed);
    } else {
      // Longest first, so the run does not end on one long test while the
      // other workers wait. A test that never ran has no length to go by
      // and goes first, in plan order.
      units.sort_by_cached_key(|unit| {
        timings
          .estimate(unit, &project)
          .map_or((false, std::cmp::Reverse(0)), |ms| (true, std::cmp::Reverse(ms)))
      });
    }
    let slots = self
      .run_slots
      .clone()
      .unwrap_or_else(|| crate::dispatcher::RunSlots::new(num_workers));
    let roster = self
      .project_roster
      .clone()
      .unwrap_or_else(|| crate::dispatcher::WorkerRoster::new(num_workers as usize, num_workers as usize));
    roster.begin(num_workers as usize);
    for unit in units {
      roster.enqueue(timings.expected(unit.tests(), &project), || match unit {
        crate::dispatcher::WorkItem::Single(assignment) => dispatcher.enqueue_single(*assignment),
        crate::dispatcher::WorkItem::Serial(batch) => dispatcher.enqueue_serial(batch),
      });
    }
    slots.started(&roster);
    (slots, roster)
  }

  /// The launch plan and connection the configured browser resolves to.
  ///
  /// # Errors
  ///
  /// Returns the reason the browser selection or connection is invalid.
  fn browser_launch(&self) -> Result<(LaunchPlan, ConnectMode), String> {
    let launch_plan = build_launch_plan(&self.config.browser)?;
    let connection = match self.config.browser.resolve_instance() {
      Ok(connection) => connection.unwrap_or(ConnectMode::Launch),
      Err(error) => {
        tracing::error!("invalid browser connection: {error}");
        return Err(format!("invalid browser connection: {error}"));
      },
    };
    Ok((launch_plan, connection))
  }

  /// A session's browser is only this run's browser when this run would
  /// have launched the same one: projects differ in backend, channel and
  /// headedness, and borrowing the session's Chromium for a `WebKit`
  /// project would run the tests on the wrong engine.
  fn matching_shared_browser(&self, launch_plan: &LaunchPlan, connection: &ConnectMode) -> Option<Arc<Browser>> {
    let shared_browser = self
      .shared_browser
      .as_ref()
      .filter(|shared| shared.connection == *connection && same_launch(&shared.plan, launch_plan))
      .map(|shared| Arc::clone(&shared.browser));
    if shared_browser.is_none() && self.shared_browser.is_some() {
      tracing::debug!(
        target: "ferridriver::runner",
        project = self.config.name.as_deref().unwrap_or_default(),
        "session browser does not match this run's launch plan; launching its own",
      );
    }
    shared_browser
  }

  /// Collect results as workers send them, re-dispatching retries and
  /// enlisting workers as the queue needs them, until every worker has
  /// left. Returns every test's statuses with what it was declared to
  /// end in: a `test.fail()` test that fails is a pass, and only the pair
  /// says so.
  async fn collect_results(
    &self,
    cx: ResultCollection<'_>,
    mut result_rx: mpsc::Receiver<WorkerTestResult>,
    worker_handles: &mut Vec<tokio::task::JoinHandle<ferridriver::Result<()>>>,
  ) -> AttemptHistory {
    let ResultCollection {
      dispatcher,
      roster,
      timings,
      project,
      total_tests,
      spawner,
    } = cx;
    let mut attempt_history = AttemptHistory::default();
    let mut final_count = 0usize;
    let mut failure_count = 0usize;
    let max_failures = if self.config.fail_fast {
      1 // fail_fast = stop after first failure
    } else {
      self.config.max_failures as usize // 0 = unlimited
    };

    // `retryStrategy: 'isolated'`: a retry waits here until every test has
    // either finished or joined it, then they run one at a time.
    let isolated = self.config.retry_strategy == crate::config::RetryStrategy::Isolated;
    let mut waiting_retries: std::collections::VecDeque<WorkerTestResult> = std::collections::VecDeque::new();
    let retry = |result: WorkerTestResult| requeue(dispatcher, roster, timings, project, result);

    let mut stop_requested = false;
    loop {
      let result = tokio::select! {
        biased;
        result = result_rx.recv() => result,
        // A worker sends its last result before it leaves, so with every
        // worker gone and nothing left to read, the run is over.
        () = roster.emptied() => {
          if result_rx.is_empty() {
            break;
          }
          continue;
        },
        () = self.run_stop.wait(), if !stop_requested => {
          // UI Stop: trip the dispatcher's hard-stop so workers drop
          // queued items and exit after their current test, then keep
          // draining results until the workers finish — the run unwinds
          // normally (contexts closed, traces stopped).
          stop_requested = true;
          dispatcher.stop();
          continue;
        },
      };
      let Some(result) = result else { break };
      record_attempt(&mut attempt_history, timings, project, &result.outcome);

      if result.should_retry {
        tracing::debug!(
          target: "ferridriver::runner",
          test = result.test.id.full_name(),
          attempt = result.outcome.attempt,
          "retrying failed test",
        );
        if isolated {
          waiting_retries.push_back(result);
        } else {
          retry(result);
        }
      } else {
        final_count += 1;
        // Track failures for max_failures / fail_fast.
        if crate::model::outcome_kind(&[result.outcome.status], result.outcome.expected_status)
          == crate::model::TestOutcomeKind::Unexpected
        {
          failure_count += 1;
        }
      }

      // Stop early if max_failures reached. Use `stop()` (hard cancel)
      // rather than `close()` so workers drop the buffered queue instead
      // of draining it.
      if max_failures > 0 && failure_count >= max_failures {
        tracing::info!(
          target: "ferridriver::runner",
          failure_count,
          max_failures,
          "max failures reached, stopping",
        );
        dispatcher.stop();
      }

      if final_count >= total_tests {
        dispatcher.close();
      } else if final_count
        + waiting_retries
          .iter()
          .map(|waiting| 1 + waiting.serial_tail.len())
          .sum::<usize>()
        >= total_tests
        && let Some(next) = waiting_retries.pop_front()
      {
        retry(next);
      }
      if !dispatcher.is_stopped() {
        worker_handles.extend(
          roster
            .enlist(&dispatcher.receiver())
            .into_iter()
            .map(|place| spawner.spawn(place)),
        );
      }
    }
    attempt_history
  }

  /// `preserve_output: "failures-only"`: delete the output directories of
  /// the tests that did not fail.
  fn drop_passing_output(&self, attempt_history: &AttemptHistory) {
    if self.config.preserve_output != "failures-only" {
      return;
    }
    for (test_key, (attempts, expected)) in attempt_history {
      if crate::model::outcome_kind(attempts, *expected) != crate::model::TestOutcomeKind::Unexpected {
        let test_output_dir = self.config.output_dir.join(crate::worker::test_artifact_dir_name(
          test_key,
          self.config.name.as_deref().unwrap_or_default(),
        ));
        if test_output_dir.exists() {
          let _ = std::fs::remove_dir_all(&test_output_dir);
        }
      }
    }
  }

  /// The run's exit status from its counts and how it ended.
  fn run_exit_code(&self, counts: &RunCounts, infrastructure_failed: bool) -> i32 {
    let mut exit_code = if counts.failed > 0 || (self.config.fail_on_flaky_tests && counts.flaky > 0) {
      exit::FAILED
    } else {
      exit::PASSED
    };
    if infrastructure_failed {
      exit_code = exit::worst(exit_code, exit::INFRASTRUCTURE);
    }
    if self.run_stop.is_requested() {
      exit_code = exit::worst(exit_code, exit::INTERRUPTED);
    }
    if exit_code == exit::FAILED && counts.failed == 0 && counts.flaky > 0 && self.config.fail_on_flaky_tests {
      let flaky = counts.flaky;
      tracing::warn!(
        target: "ferridriver::runner",
        flaky,
        "fail_on_flaky_tests: flagging exit 1 for {flaky} flaky test(s)",
      );
    }
    exit_code
  }
}

/// Queue a failed test again for its next attempt, with the serial tests
/// that never started behind it.
fn requeue(
  dispatcher: &Dispatcher,
  roster: &crate::dispatcher::WorkerRoster,
  timings: &crate::timings::Timings,
  project: &str,
  result: WorkerTestResult,
) {
  let expected = timings.expected(
    std::iter::once(&result.test).chain(result.serial_tail.iter().map(|tail| &tail.test)),
    project,
  );
  roster.enqueue(expected, || {
    dispatcher.retry_shared(crate::dispatcher::Retry {
      test: result.test,
      attempt: result.outcome.attempt + 1,
      suite_key: result.suite_key,
      hooks: result.hooks,
      suite_mode: result.suite_mode,
      serial_tail: result.serial_tail,
    });
  });
}

/// Add an attempt to its test's history, and its duration to the
/// timings unless it was skipped.
fn record_attempt(
  attempt_history: &mut AttemptHistory,
  timings: &crate::timings::Timings,
  project: &str,
  outcome: &crate::model::TestOutcome,
) {
  let entry = attempt_history
    .entry(outcome.test_id.clone())
    .or_insert_with(|| (Vec::new(), outcome.expected_status));
  entry.0.push(outcome.status);
  entry.1 = outcome.expected_status;
  if outcome.status != TestStatus::Skipped {
    timings.record(outcome.test_id.stable_id(project), outcome.duration);
  }
}

/// Wait for every worker to finish. Returns whether any failed to clean
/// up, each failure reported on `event_bus` as a run error.
async fn join_workers(
  worker_handles: Vec<tokio::task::JoinHandle<ferridriver::Result<()>>>,
  event_bus: &EventBus,
) -> bool {
  let mut infrastructure_failed = false;
  for handle in worker_handles {
    let outcome = handle
      .await
      .map_err(|error| error.to_string())
      .and_then(|result| result.map_err(|error| error.to_string()));
    if let Err(error) = outcome {
      infrastructure_failed = true;
      event_bus.emit(ReporterEvent::RunError {
        error: Box::new(crate::model::TestFailure::msg(format!(
          "worker cleanup failed: {error}"
        ))),
      });
    }
  }
  infrastructure_failed
}

/// What a run's workers did.
struct WorkersRun {
  attempt_history: AttemptHistory,
  /// A worker failed to clean up.
  infrastructure_failed: bool,
  /// A worker's browser failed to launch.
  launch_failed: bool,
}

/// Every test's statuses, with what it was declared to end in.
type AttemptHistory = FxHashMap<crate::model::TestId, (Vec<TestStatus>, crate::model::ExpectedStatus)>;

/// The final tally of a run, flaky detection included.
struct RunCounts {
  passed: usize,
  failed: usize,
  skipped: usize,
  flaky: usize,
}

impl RunCounts {
  fn of(attempt_history: &AttemptHistory) -> Self {
    let mut counts = Self {
      passed: 0,
      failed: 0,
      skipped: 0,
      flaky: 0,
    };
    for (attempts, expected) in attempt_history.values() {
      match crate::model::outcome_kind(attempts, *expected) {
        crate::model::TestOutcomeKind::Expected => counts.passed += 1,
        crate::model::TestOutcomeKind::Flaky => {
          counts.flaky += 1;
          counts.passed += 1;
        },
        crate::model::TestOutcomeKind::Skipped => counts.skipped += 1,
        crate::model::TestOutcomeKind::Unexpected => counts.failed += 1,
      }
    }
    counts
  }
}

/// What [`TestRunner::collect_results`] reads while the run goes.
struct ResultCollection<'a> {
  dispatcher: &'a Arc<Dispatcher>,
  roster: &'a Arc<crate::dispatcher::WorkerRoster>,
  timings: &'a Arc<crate::timings::Timings>,
  project: &'a str,
  total_tests: usize,
  spawner: &'a WorkerSpawner,
}

/// Pool workers across lazy shared browser processes (up to
/// `workers_per_browser` workers per process, default 4) for local Chromium
/// and `WebKit` launches.
///
/// Sharing cuts cold-start contention and memory, while bounding fan-in per
/// browser keeps Chrome's single browser IO/UI thread from saturating at high
/// worker counts. `BiDi`/Firefox is excluded because Firefox runs with
/// `dom.ipc.processCount = 1`, which would serialize workers onto one content
/// thread. Set `FERRIDRIVER_SHARE_BROWSER=0` to disable, or
/// `FERRIDRIVER_WORKERS_PER_BROWSER=N` to tune pool size.
fn shared_browser_pool(
  launch_plan: &LaunchPlan,
  connection: &ConnectMode,
  num_workers: u32,
  has_shared_browser: bool,
  context_reuse: bool,
  launch_failed: &Arc<std::sync::atomic::AtomicBool>,
) -> (usize, Vec<Arc<BrowserHandle>>) {
  const DEFAULT_WORKERS_PER_SHARED_BROWSER: usize = 4;
  let share_enabled = !matches!(std::env::var("FERRIDRIVER_SHARE_BROWSER").as_deref(), Ok("0" | "false"));
  let workers_per_browser = std::env::var("FERRIDRIVER_WORKERS_PER_BROWSER")
    .ok()
    .and_then(|v| v.parse::<usize>().ok())
    .filter(|&n| n > 0)
    .unwrap_or(DEFAULT_WORKERS_PER_SHARED_BROWSER);
  let handles = if share_enabled
    && !has_shared_browser
    && num_workers > 1
    && workers_per_browser > 1
    && *connection == ConnectMode::Launch
    && (matches!(
      launch_plan.backend,
      ferridriver::backend::BackendKind::CdpPipe | ferridriver::backend::BackendKind::CdpWs
    ) || (!cfg!(windows) && launch_plan.backend == ferridriver::backend::BackendKind::WebKit))
  {
    let pool_size = (num_workers as usize).div_ceil(workers_per_browser);
    (0..pool_size)
      .map(|_| {
        Arc::new(
          BrowserHandle::with_connection(launch_plan.clone(), connection.clone())
            .reporting_launch_failure(Arc::clone(launch_failed))
            .recycling_contexts(context_reuse),
        )
      })
      .collect()
  } else {
    Vec::new()
  };
  (workers_per_browser, handles)
}

/// Starts the run's workers. Worker numbers are handed out per RUN, not
/// per runner: projects execute concurrently, and two workers numbered 0
/// would share a scratch directory, a `.playwright-artifacts-0` the UI
/// reads, and — via the per-worker script session — the "test currently
/// running" that `test.step()` and `test.info()` resolve against.
///
/// Workers come and go with the work, as Playwright's do: one leaves once
/// the queue is empty, handing its slot to a project still running, and
/// work queued later (a retry) enlists new ones.
struct WorkerSpawner {
  config: Arc<TestConfig>,
  event_bus: Option<EventBus>,
  live_traces: bool,
  worker_ids: Arc<std::sync::atomic::AtomicU32>,
  project_worker_seq: Arc<std::sync::atomic::AtomicUsize>,
  workers_per_browser: usize,
  dispatcher: Arc<Dispatcher>,
  result_tx: mpsc::Sender<WorkerTestResult>,
  custom_fixtures: FxHashMap<String, crate::fixture::FixtureDef>,
  shared_browser: Option<Arc<Browser>>,
  run_shared_handles: Vec<Arc<BrowserHandle>>,
  launch_plan: LaunchPlan,
  connection: ConnectMode,
  launch_failed: Arc<std::sync::atomic::AtomicBool>,
  slots: Arc<crate::dispatcher::RunSlots>,
  roster: Arc<crate::dispatcher::WorkerRoster>,
}

impl WorkerSpawner {
  fn spawn(&self, place: crate::dispatcher::RosterPlace) -> tokio::task::JoinHandle<ferridriver::Result<()>> {
    let config = Arc::clone(&self.config);
    let event_bus = self.event_bus.clone();
    let live_traces = self.live_traces;
    let worker_ids = Arc::clone(&self.worker_ids);
    let project_worker_seq = Arc::clone(&self.project_worker_seq);
    let workers_per_browser = self.workers_per_browser;
    let rx = self.dispatcher.receiver();
    let tx = self.result_tx.clone();
    let custom_fixtures = self.custom_fixtures.clone();
    let shared = self.shared_browser.clone();
    let run_shared_handles = self.run_shared_handles.clone();
    let plan = self.launch_plan.clone();
    let connection = self.connection.clone();
    let stop_flag = self.dispatcher.stop_flag();
    let launch_failed = Arc::clone(&self.launch_failed);
    let slots = Arc::clone(&self.slots);
    let roster = Arc::clone(&self.roster);

    tokio::spawn(async move {
      // Held until the browser has closed: giving the slot back at the
      // last test would overlap the next worker's launch with a live
      // browser.
      let lease = slots.take(&roster).await;
      place.attach_pending_grant(lease.pending_grant());
      let run_shared = if run_shared_handles.is_empty() {
        None
      } else {
        // Assign sequentially in chunks of `workers_per_browser` so a run
        // that only acquires a few slots launches only the needed browsers.
        let seq = project_worker_seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let bucket = (seq / workers_per_browser) % run_shared_handles.len();
        Some(Arc::clone(&run_shared_handles[bucket]))
      };
      let mut pending = None;
      loop {
        let id = worker_ids.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let worker = Worker::new(id, lease.index(), Arc::clone(&config), event_bus.clone(), live_traces);
        let custom_pool = FixturePool::new(custom_fixtures.clone(), FixtureScope::Worker);
        let browser_handle = if let Some(b) = &shared {
          Arc::new(BrowserHandle::from_shared(Arc::clone(b)))
        } else if let Some(shared_handle) = &run_shared
          && shared_handle.try_get().is_none_or(|b| b.is_connected())
        {
          Arc::new(shared_handle.borrow_shared_scoped(u64::from(id) + 1))
        } else {
          Arc::new(
            BrowserHandle::with_connection(plan.clone(), connection.clone())
              .reporting_launch_failure(Arc::clone(&launch_failed))
              .recycling_contexts(config.context_reuse),
          )
        };
        let exit = Box::pin(worker.run(
          browser_handle,
          custom_pool,
          rx.clone(),
          &place,
          tx.clone(),
          Arc::clone(&stop_flag),
          pending,
        ))
        .await?;
        pending = exit.pending;
        if (pending.is_none() && !exit.restart) || stop_flag.load(std::sync::atomic::Ordering::SeqCst) {
          break;
        }
      }
      Ok::<(), ferridriver::FerriError>(())
    })
  }
}

/// The next SIGINT or SIGTERM.
async fn wait_for_termination() {
  #[cfg(unix)]
  {
    use tokio::signal::unix::{SignalKind, signal};
    if let Ok(mut terminate) = signal(SignalKind::terminate()) {
      tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        _ = terminate.recv() => {},
      }
      return;
    }
  }
  let _ = tokio::signal::ctrl_c().await;
}
