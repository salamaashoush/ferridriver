//! Test runner orchestrator: overlaps browser launch with test dispatch,
//! handles retries with flaky detection.

use std::sync::Arc;
use std::time::Instant;

use rustc_hash::FxHashMap;
use tokio::sync::mpsc;

use crate::config::{CliOverrides, ProjectConfig, TestConfig};
use crate::dispatcher::Dispatcher;
use crate::exit;
use crate::fixture::{FixturePool, FixtureScope, builtin_fixtures, validate_dag};
use crate::model::{Hooks, TestHooks, TestPlan, TestStatus};
use crate::reporter::{EventBus, EventBusBuilder, ReporterDriver, ReporterEvent, ReporterSet};
use crate::shard;
use crate::worker::{Worker, WorkerTestResult};

use ferridriver::Browser;
use ferridriver::options::LaunchPlan;
use ferridriver::state::{BrowserState, ConnectMode};

/// One run's event plumbing: the bus tests emit into, plus the reporters
/// draining it — the session's, and any this run added.
/// See [`TestRunner::start_run_bus`].
pub struct RunBus {
  pub bus: EventBus,
  reporters: Option<tokio::task::JoinHandle<ReporterSet>>,
  extra_reporters: Option<tokio::task::JoinHandle<ReporterSet>>,
}

/// The browser a long-lived session keeps between runs, and the plan it
/// was launched from. A project configured for another browser (a
/// different backend, channel, or headedness) must not silently borrow
/// it — [`same_launch`] is what decides.
#[derive(Clone)]
struct SharedBrowser {
  browser: Arc<Browser>,
  plan: LaunchPlan,
  connection: ConnectMode,
}

/// Whether two plans would launch the same browser process.
///
/// Only the fields that reach the child matter; context-level options
/// (viewport, locale) ride on the pages, not the process.
fn same_launch(a: &LaunchPlan, b: &LaunchPlan) -> bool {
  a.backend == b.backend
    && a.kind == b.kind
    && a.headless == b.headless
    && a.channel == b.channel
    && a.executable_path == b.executable_path
    && a.user_data_dir == b.user_data_dir
    && a.args == b.args
    && a.env == b.env
}

/// Plans discovered per project, shared between the runner and whatever
/// rebuilds them.
///
/// Written once for a one-shot run and once per cycle under watch / UI.
/// Read once per project per run, so the lock is never on a hot path.
#[derive(Clone, Default)]
pub struct ProjectPlans(Arc<std::sync::RwLock<FxHashMap<String, Arc<TestPlan>>>>);

impl ProjectPlans {
  /// Replace every plan.
  pub fn set(&self, plans: FxHashMap<String, TestPlan>) {
    *self.0.write().unwrap_or_else(std::sync::PoisonError::into_inner) =
      plans.into_iter().map(|(name, plan)| (name, Arc::new(plan))).collect();
  }

  /// The plan `name` runs, if it has one of its own.
  #[must_use]
  pub fn get(&self, name: &str) -> Option<Arc<TestPlan>> {
    self
      .0
      .read()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .get(name)
      .map(Arc::clone)
  }

  /// Whether any project has a plan of its own.
  #[must_use]
  pub fn is_empty(&self) -> bool {
    self
      .0
      .read()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .is_empty()
  }
}

/// One project of a run: the config it executes under, and the name its
/// test ids are hashed with. A config without `[[test.projects]]` is
/// itself one project.
#[derive(Clone)]
pub struct ProjectRun {
  pub name: String,
  pub config: Arc<TestConfig>,
  project: Option<ProjectConfig>,
  /// This project's own plan, when discovery itself differs per project
  /// (see [`TestRunner::set_project_plans`]). `None` narrows the shared
  /// plan instead.
  plan: Option<Arc<TestPlan>>,
}

impl ProjectRun {
  /// The `[[test.projects]]` entry this run came from, if any.
  #[must_use]
  pub fn project_config(&self) -> Option<&ProjectConfig> {
    self.project.as_ref()
  }

  /// `plan` narrowed to the tests this project runs — or this project's
  /// own plan, narrowed, when it has one.
  #[must_use]
  pub fn narrow(&self, plan: &TestPlan) -> TestPlan {
    let mut narrowed = self.plan.as_deref().unwrap_or(plan).clone();
    if let Some(project) = &self.project {
      filter_plan_for_project(&mut narrowed, &self.config, project);
    }
    narrowed.expand_repetitions(self.config.repeat_each);
    narrowed
  }
}

/// A project's own event bus, plus the tasks draining it. The bus is
/// closed and those tasks awaited when the project finishes, so nothing
/// a consumer forwards arrives after the run it belongs to has ended.
pub struct ProjectStream {
  pub bus: EventBus,
  pub drains: Vec<tokio::task::JoinHandle<()>>,
}

/// How a caller takes part in a multi-project run.
///
/// [`TestRunner::run`] needs neither: every project reports onto the one
/// bus its reporters read. A consumer that must tell projects apart —
/// the UI computes a test's id from the project that ran it — asks for a
/// stream per project, and narrows each project's plan itself.
#[derive(Default)]
pub struct ProjectHooks<'a> {
  pub stream: Option<&'a (dyn Fn(&str) -> ProjectStream + Send + Sync)>,
  pub narrow: Option<&'a PlanNarrower>,
}

/// Narrows a named project's plan in place.
pub type PlanNarrower = dyn Fn(&str, &mut TestPlan) + Send + Sync;

/// Aggregate outcome of one `execute()` pass. The multi-project orchestrator
/// sums these across concurrently-run projects to emit a single `RunFinished`.
#[derive(Clone, Copy, Default)]
pub struct ExecuteSummary {
  pub exit_code: i32,
  pub total: usize,
  pub passed: usize,
  pub failed: usize,
  pub skipped: usize,
  pub flaky: usize,
}

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

fn announce_ui_url(url: &str) -> std::io::Result<()> {
  use std::io::Write as _;
  let mut out = std::io::stdout().lock();
  writeln!(out, "\n  ferridriver UI mode\n\n  {url}\n")?;
  out.flush()
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

  /// Run multiple projects in dependency order, reporting to the
  /// configured reporters.
  async fn run_projects(&mut self, plan: TestPlan) -> i32 {
    let edits = match self.preprocess_corpus(&plan).await {
      Ok(edits) => edits,
      Err(message) => {
        eprintln!("Error: reporter preprocess failed: {message}");
        return 1;
      },
    };

    let mut builder = EventBusBuilder::new();
    let driver_handle = if self.reporters.is_empty() {
      None
    } else {
      let sub = builder.subscribe();
      let reporters = std::mem::take(&mut self.reporters);
      Some(tokio::spawn(ReporterDriver::new(reporters, sub).run()))
    };
    let bus = builder.build();

    let narrow = move |name: &str, plan: &mut TestPlan| apply_run_edits(plan, name, &edits);
    let summary = Box::pin(self.execute_projects_with_summary(
      plan,
      bus.clone(),
      ProjectHooks {
        narrow: Some(&narrow),
        ..ProjectHooks::default()
      },
    ))
    .await;

    bus.close();
    if let Some(driver_handle) = driver_handle
      && let Ok(reporters) = driver_handle.await
    {
      self.reporters = reporters;
    }
    apply_status_override(&self.reporters, summary.exit_code)
  }

  /// Execute `plan` once per project, in dependency order, onto `bus`.
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
  ///
  /// A config that declares no projects is itself the one project.
  pub async fn execute_projects_with_summary(
    &self,
    plan: TestPlan,
    bus: EventBus,
    hooks: ProjectHooks<'_>,
  ) -> ExecuteSummary {
    self.worker_ids.store(0, std::sync::atomic::Ordering::SeqCst);
    if self.config.projects.is_empty() {
      return self.execute_single_project(plan, bus, &hooks).await;
    }
    let projects = self.config.projects.clone();

    // Projects share one output directory and run concurrently, so the
    // scratch directories are swept here, around all of them, rather than
    // by each project's own run.
    crate::artifacts::sweep(&self.config.output_dir);

    let (scheduled, cli_teardown_idx) = match self.scheduled_projects() {
      Ok(scheduled) => scheduled,
      Err(e) => {
        tracing::error!(target: "ferridriver::runner", "{e}");
        eprintln!("Error: {e}");
        return ExecuteSummary {
          exit_code: exit::USAGE,
          ..Default::default()
        };
      },
    };
    let prereqs = project_prerequisites(&projects, &scheduled, cli_teardown_idx);

    // ── Hoist web servers out of per-project execute ──
    // `merge_project` copies the top-level `web_server` list onto every
    // project; starting/stopping the same servers per project would bind the
    // same ports concurrently. Start them once here and clear the per-project
    // copies so each project's `execute()` skips its web-server lifecycle.
    let web_server_manager = match self.start_shared_web_servers(&bus).await {
      Ok(manager) => manager,
      Err(summary) => return summary,
    };

    let (planning, mut plans) = match self.plan_projects(&plan, &projects, &scheduled, &hooks) {
      Ok(planned) => planned,
      Err(message) => {
        eprintln!("Error: {message}");
        return ExecuteSummary {
          exit_code: exit::USAGE,
          ..Default::default()
        };
      },
    };
    let total_tests = planning.total_tests;

    // ── Single aggregate run boundary ──
    let reporting_enabled = bus.has_subscribers();

    // `workers` is the global concurrency budget; never launch more workers
    // than tests across all projects in flight.
    let num_workers = self
      .config
      .worker_count()
      .min(u32::try_from(total_tests).unwrap_or(u32::MAX))
      .max(1);
    if reporting_enabled {
      let project_plans: Vec<crate::reporter::api::ProjectPlan<'_>> = scheduled
        .iter()
        .filter_map(|idx| {
          Some(crate::reporter::api::ProjectPlan {
            name: projects[*idx].name.as_str(),
            config: planning.merged.get(idx)?.as_ref(),
            project: Some(&projects[*idx]),
            plan: plans.get(idx)?,
          })
        })
        .collect();
      bus.emit(ReporterEvent::RunStarted {
        total_tests,
        num_workers,
        metadata: with_shuffle_seed(self.config.metadata.clone(), self.overrides.shuffle.as_deref()),
        start_time: std::time::SystemTime::now(),
        preamble: Arc::new(crate::reporter::api::RunPreamble::build(&self.config, &project_plans)),
      });
    }
    let run_start = Instant::now();

    let mut agg = Box::pin(self.schedule_projects(
      ProjectRunContext {
        projects: &projects,
        planning: &planning,
        num_workers,
        hooks: &hooks,
        bus: &bus,
      },
      &mut plans,
      ProjectSchedule::new(prereqs, scheduled),
    ))
    .await;

    // ── Single aggregate RunFinished ──
    if reporting_enabled {
      bus.emit(ReporterEvent::RunFinished {
        total: total_tests,
        passed: agg.passed,
        failed: agg.failed,
        skipped: agg.skipped,
        flaky: agg.flaky,
        duration: run_start.elapsed(),
        status: if agg.failed > 0 {
          crate::reporter::RunStatus::Failed
        } else if self.run_stop.is_requested() {
          crate::reporter::RunStatus::Interrupted
        } else {
          crate::reporter::RunStatus::Passed
        },
      });
    }

    if let Some(mgr) = web_server_manager {
      mgr.stop().await;
    }

    // Every project is finished with its scratch directory now.
    crate::artifacts::sweep(&self.config.output_dir);

    if self.run_stop.is_requested() {
      agg.exit_code = exit::worst(agg.exit_code, exit::INTERRUPTED);
    }
    agg.total = total_tests;
    agg
  }

  /// The ready-set scheduler: spawn every dependency-ready project up to
  /// the parallelism cap, drive completions through a `JoinSet`, and
  /// re-evaluate readiness on each completion. Dependency ordering,
  /// teardown ordering and dependency-failure skipping all follow from
  /// the prerequisites in `schedule`.
  async fn schedule_projects(
    &self,
    cx: ProjectRunContext<'_>,
    plans: &mut FxHashMap<usize, TestPlan>,
    mut schedule: ProjectSchedule,
  ) -> ExecuteSummary {
    // `max_parallel_projects == 0` means unbounded (cap at the number of
    // scheduled projects).
    let cap = if self.config.max_parallel_projects == 0 {
      schedule.remaining.len().max(1)
    } else {
      self.config.max_parallel_projects as usize
    };
    let mut join_set: tokio::task::JoinSet<(usize, Option<ExecuteSummary>)> = tokio::task::JoinSet::new();
    let mut in_flight = 0usize;
    // Every project running draws its workers from these, the one with
    // the longest critical path first: its own expected work, and the
    // work of the projects that wait for it to finish.
    let worker_slots = crate::dispatcher::RunSlots::new(cx.num_workers);
    let timings = Arc::new(crate::timings::Timings::load(&crate::identity::root(&self.config)));
    let mut planned: FxHashMap<usize, Vec<u64>> = plans
      .iter()
      .map(|(&idx, plan)| {
        let name = cx.projects[idx].name.as_str();
        let tests = plan.suites.iter().flat_map(|suite| &suite.tests);
        (idx, tests.map(|test| timings.expected([test], name)).collect())
      })
      .collect();
    let after = work_waiting_on(cx.projects, &schedule.remaining, |idx| {
      planned.get(&idx).map_or(0, |work| {
        work.iter().sum::<u64>() / u64::from(self.project_cap(&cx, idx))
      })
    });
    let mut rosters: FxHashMap<usize, Arc<crate::dispatcher::WorkerRoster>> = FxHashMap::default();

    loop {
      // Launch every ready project up to the parallelism cap. Skips (no tests
      // or dependency failed) resolve immediately and may unblock others, so
      // keep scanning until no further progress is possible this round.
      // A stop (a signal, the UI's Stop) lets running projects finish their
      // current tests and starts nothing new.
      while in_flight < cap && !self.run_stop.is_requested() {
        let Some(idx) = schedule.next_ready() else { break };
        let name = cx.projects[idx].name.as_str();

        // Skip a normal project whose passing-required prerequisites did not
        // pass (dependency failure). Teardowns are never skipped this way.
        if schedule.is_blocked(idx) {
          tracing::warn!(target: "ferridriver::runner", project = name, "skipping — dependency failed");
          schedule.terminal.insert(idx);
          schedule.exit_code = exit::worst(schedule.exit_code, exit::FAILED);
          continue;
        }

        let Some(project_plan) = plans.remove(&idx).filter(|plan| plan.total_tests > 0) else {
          tracing::debug!(target: "ferridriver::runner", project = name, "no tests matched, skipping");
          schedule.terminal.insert(idx);
          schedule.passed.insert(idx);
          continue;
        };

        tracing::info!(
          target: "ferridriver::runner",
          project = name,
          tests = project_plan.total_tests,
          "running project",
        );

        let mut sub_runner = self.project_runner(&cx, idx, project_plan.total_tests);
        let roster = crate::dispatcher::WorkerRoster::planned(
          self.project_cap(&cx, idx).min(sub_runner.config.worker_count()) as usize,
          cx.num_workers as usize,
          after.get(&idx).copied().unwrap_or_default(),
          planned.remove(&idx).unwrap_or_default(),
        );
        worker_slots.starting(&roster);
        rosters.insert(idx, Arc::clone(&roster));
        sub_runner.run_slots = Some(Arc::clone(&worker_slots));
        sub_runner.project_roster = Some(roster);
        sub_runner.timings = Some(Arc::clone(&timings));
        let (project_bus, drains) = match cx.hooks.stream {
          Some(stream) => {
            let stream = stream(name);
            (stream.bus, stream.drains)
          },
          None => (cx.bus.clone(), Vec::new()),
        };
        let owns_bus = cx.hooks.stream.is_some();
        join_set.spawn(async move {
          let summary = sub_runner.execute_with_summary(project_plan, project_bus.clone()).await;
          // A bus of this project's own is this project's to close: its
          // drains must finish before the caller calls the run over.
          if owns_bus {
            project_bus.close();
            for drain in drains {
              let _ = drain.await;
            }
          }
          (idx, Some(summary))
        });
        in_flight += 1;
      }

      // Nothing running and nothing launchable — done (or a cycle the topo
      // sort already rejected, so `remaining` is unreachable prereqs).
      if in_flight == 0 {
        break;
      }

      // Await the next completion, then loop to launch newly-ready projects.
      if let Some(joined) = join_set.join_next().await {
        in_flight -= 1;
        schedule.finished(joined, &rosters, &worker_slots);
      }
    }

    if let Err(error) = timings.save() {
      tracing::warn!(target: "ferridriver::runner", "could not keep test durations: {error}");
    }
    let mut agg = schedule.agg;
    agg.exit_code = schedule.exit_code;
    agg
  }

  /// How many of the run's worker slots project `idx` may hold at once.
  fn project_cap(&self, cx: &ProjectRunContext<'_>, idx: usize) -> u32 {
    cx.planning
      .merged
      .get(&idx)
      .map_or_else(|| self.config.worker_count(), |config| config.worker_count())
      .clamp(1, cx.num_workers)
  }

  /// The runner one project runs on: its merged config, its overrides,
  /// and no run boundary of its own.
  fn project_runner(&self, cx: &ProjectRunContext<'_>, idx: usize, total_tests: usize) -> Self {
    let mut project_config = cx
      .planning
      .merged
      .get(&idx)
      .map_or_else(|| (*self.config).clone(), |c| (**c).clone());
    // A project's `workers` caps how many of the run's slots it holds
    // at once, as in Playwright; which ones it gets is decided test by
    // test, not up front.
    project_config.workers = Some(
      project_config
        .worker_count()
        .max(1)
        .min(cx.num_workers)
        .min(u32::try_from(total_tests).unwrap_or(u32::MAX)),
    );
    let overrides = if cx.planning.exempt.contains(&idx) {
      cx.planning.exempt_overrides.clone()
    } else {
      self.overrides.clone()
    };
    let mut sub_runner = self.with_run_options(Arc::new(project_config), overrides);
    sub_runner.suppress_run_boundary = true;
    sub_runner
  }

  /// Start the run's web servers once for every project, publishing the
  /// first one's URL as the base URL when none is configured.
  ///
  /// # Errors
  ///
  /// Returns the summary of a run that could not start: the failure is
  /// reported on `bus` as a run error and an empty finished run.
  async fn start_shared_web_servers(
    &self,
    bus: &EventBus,
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
        bus.emit(ReporterEvent::RunError {
          error: Box::new(crate::model::TestFailure::msg(format!("webServer start failed: {e}"))),
        });
        bus.emit(ReporterEvent::RunFinished {
          total: 0,
          passed: 0,
          failed: 0,
          skipped: 0,
          flaky: 0,
          duration: std::time::Duration::ZERO,
          status: crate::reporter::RunStatus::Failed,
        });
        Err(ExecuteSummary {
          exit_code: exit::INFRASTRUCTURE,
          ..Default::default()
        })
      },
    }
  }

  /// Each scheduled project's merged config and filtered plan, built up
  /// front so the run can report an accurate aggregate total and reuse
  /// them when it spawns the projects. The output directories of projects
  /// that never preserve output are cleared on the way.
  ///
  /// # Errors
  ///
  /// Returns the message of a run filter that does not apply.
  fn plan_projects(
    &self,
    plan: &TestPlan,
    projects: &[ProjectConfig],
    scheduled: &[usize],
    hooks: &ProjectHooks<'_>,
  ) -> Result<(ProjectPlanning, FxHashMap<usize, TestPlan>), String> {
    // Build each project's merged config + filtered plan up front so we can
    // both report an accurate aggregate total and reuse them when spawning.
    let mut merged: FxHashMap<usize, Arc<TestConfig>> = FxHashMap::default();
    let mut plans: FxHashMap<usize, TestPlan> = FxHashMap::default();
    let mut total_tests = 0usize;
    // A project others depend on runs whole, and only when one of them
    // has tests (see `dependents_of`); dependents are planned first.
    let dependents = dependents_of(projects, scheduled);
    let exempt_overrides = without_file_filters(&self.overrides);
    let exempt_runner = self.with_run_options(Arc::clone(&self.config), exempt_overrides.clone());
    let mut exempt: rustc_hash::FxHashSet<usize> = rustc_hash::FxHashSet::default();
    for &idx in scheduled.iter().rev() {
      let mut mc = self.config.merge_project(&projects[idx]);
      mc.web_server = Vec::new();
      let mut p = self
        .project_plans
        .get(&projects[idx].name)
        .map_or_else(|| plan.clone(), |own| (*own).clone());
      filter_plan_for_project(&mut p, &mc, &projects[idx]);
      let needed_by = dependents.get(&idx).map_or(&[][..], Vec::as_slice);
      let filters = if needed_by.is_empty() {
        self
      } else {
        exempt.insert(idx);
        &exempt_runner
      };
      filters.apply_run_filters(&mut p, &projects[idx].name)?;
      if !needed_by.is_empty()
        && needed_by
          .iter()
          .all(|other| plans.get(other).is_none_or(|plan| plan.total_tests == 0))
      {
        p.suites.clear();
        p.total_tests = 0;
      }
      p.expand_repetitions(mc.repeat_each);
      if let Some(narrow) = hooks.narrow {
        narrow(&projects[idx].name, &mut p);
      }
      total_tests += p.total_tests;
      merged.insert(idx, Arc::new(mc));
      plans.insert(idx, p);
    }

    let output_dirs: std::collections::BTreeSet<_> = merged
      .values()
      .filter(|config| config.preserve_output == "never")
      .map(|config| &config.output_dir)
      .collect();
    for output_dir in output_dirs {
      let _ = std::fs::remove_dir_all(output_dir);
    }
    Ok((
      ProjectPlanning {
        merged,
        total_tests,
        exempt,
        exempt_overrides,
      },
      plans,
    ))
  }

  /// The projects a multi-project run schedules, in dependency order: the
  /// selected ones, their dependencies (unless `--no-deps`) and teardowns
  /// — Playwright's `buildProjectsClosure` — plus the `--teardown`
  /// project, whose index is returned beside them.
  fn scheduled_projects(&self) -> Result<(Vec<usize>, Option<usize>), String> {
    let projects = &self.config.projects;
    let sorted = topo_sort_projects(projects).map_err(|e| format!("project dependency error: {e}"))?;
    let mut allowed_indices: rustc_hash::FxHashSet<usize> = select_projects(
      projects,
      &self.overrides.project_filter,
      self.overrides.include_non_default_projects,
    )?
    .into_iter()
    .collect();
    if !self.overrides.no_deps {
      let mut frontier: Vec<usize> = allowed_indices.iter().copied().collect();
      while let Some(idx) = frontier.pop() {
        let deps = projects[idx].dependencies.iter().chain(projects[idx].teardown.iter());
        for dep_name in deps {
          if let Some(dep_idx) = projects.iter().position(|p| &p.name == dep_name)
            && allowed_indices.insert(dep_idx)
          {
            frontier.push(dep_idx);
          }
        }
      }
    }
    let sorted: Vec<usize> = sorted.into_iter().filter(|idx| allowed_indices.contains(idx)).collect();

    // `--teardown NAME` overrides any project-declared teardown by
    // forcing it onto the run regardless of explicit project filter.
    let cli_teardown_idx: Option<usize> = self
      .overrides
      .teardown
      .as_deref()
      .and_then(|name| projects.iter().position(|p| p.name == name));

    tracing::info!(
      target: "ferridriver::runner",
      projects = sorted.len(),
      order = ?sorted.iter().map(|i| &projects[*i].name).collect::<Vec<_>>(),
      "running projects in dependency order",
    );

    // Append CLI-supplied teardown so the scheduler tracks it like any other
    // project. It runs after every other selected project reaches a terminal
    // state, regardless of pass/fail — modelled below as a teardown with all
    // remaining projects as prerequisites.
    let mut scheduled: Vec<usize> = sorted;
    if let Some(td_idx) = cli_teardown_idx
      && !scheduled.contains(&td_idx)
    {
      scheduled.push(td_idx);
    }
    Ok((scheduled, cli_teardown_idx))
  }

  /// Every project a run covers, in the order it schedules them. A config
  /// without projects is itself the one project.
  fn scheduled_runs(&self) -> Result<Vec<ProjectRun>, String> {
    let runs = self.project_runs();
    if self.config.projects.is_empty() {
      return Ok(runs);
    }
    let (scheduled, _) = self.scheduled_projects()?;
    Ok(scheduled.into_iter().filter_map(|idx| runs.get(idx).cloned()).collect())
  }

  /// Fail fast on a selection that cannot be what the caller meant: a
  /// `--test-id` naming no test of this corpus (an orchestrator's listing
  /// and this checkout disagree), or a `--shard-assignment` splitting a
  /// serial group.
  ///
  /// # Errors
  /// The message naming what is wrong with the selection.
  pub fn validate_selection(&self, plan: &TestPlan) -> Result<(), String> {
    if self.overrides.test_ids.is_empty() && self.overrides.shard_assignment.is_none() {
      return Ok(());
    }
    let runs = self.scheduled_runs()?;
    if !self.overrides.test_ids.is_empty() {
      let mut known: rustc_hash::FxHashSet<String> = rustc_hash::FxHashSet::default();
      for run in &runs {
        let narrowed = run.narrow(plan);
        for suite in &narrowed.suites {
          known.extend(suite.tests.iter().map(|test| test.id.stable_id(&run.name)));
        }
      }
      let unknown: Vec<&str> = self
        .overrides
        .test_ids
        .iter()
        .filter(|id| !known.contains(id.as_str()))
        .map(String::as_str)
        .collect();
      if !unknown.is_empty() {
        return Err(format!(
          "{} test id(s) match no test in this suite: {}",
          unknown.len(),
          unknown.join(", ")
        ));
      }
    }
    self.list(plan).map(|_| ())
  }

  /// The tests this run would execute, project by project, after every
  /// filter the command line and config apply — without starting a web
  /// server, launching a browser or calling a reporter.
  ///
  /// # Errors
  /// When the project selection or the shard assignment is invalid.
  pub fn list(&self, plan: &TestPlan) -> Result<Vec<crate::listing::ListedTest>, String> {
    let runs = self.scheduled_runs()?;
    // As the run plans them: a project others depend on runs whole, and
    // only when one of them has tests (see `dependents_of`).
    let indices: Vec<usize> = runs
      .iter()
      .map(|run| {
        self
          .config
          .projects
          .iter()
          .position(|p| p.name == run.name)
          .unwrap_or(usize::MAX)
      })
      .collect();
    let dependents = dependents_of(&self.config.projects, &indices);
    let mut plans: FxHashMap<usize, TestPlan> = FxHashMap::default();
    for (run, &idx) in runs.iter().zip(&indices).rev() {
      let mut narrowed = run.narrow(plan);
      let needed_by = dependents.get(&idx).map_or(&[][..], Vec::as_slice);
      let overrides = if needed_by.is_empty() {
        self.overrides.clone()
      } else {
        without_file_filters(&self.overrides)
      };
      let runner = self.with_run_options(Arc::clone(&run.config), overrides);
      runner.select_tests(&mut narrowed, &run.name)?;
      if !needed_by.is_empty()
        && needed_by
          .iter()
          .all(|other| plans.get(other).is_none_or(|p| p.total_tests == 0))
      {
        narrowed.suites.clear();
        narrowed.total_tests = 0;
      }
      plans.insert(idx, narrowed);
    }
    let mut listed = Vec::new();
    for (run, idx) in runs.iter().zip(&indices) {
      if let Some(narrowed) = plans.get(idx) {
        listed.extend(crate::listing::list_plan(narrowed, &run.name, &run.config));
      }
    }
    Ok(listed)
  }

  /// `--list`: print what would run, as text or (`--format json`) as the
  /// listing document, and exit without running it.
  fn print_list(&self, plan: &TestPlan) -> i32 {
    match self.list(plan) {
      Ok(listed) => match crate::listing::print(&listed, &[], self.overrides.list_json) {
        Ok(()) => exit::PASSED,
        Err(e) => {
          eprintln!("Error: could not write the listing: {e}");
          exit::INFRASTRUCTURE
        },
      },
      Err(message) => {
        eprintln!("Error: {message}");
        exit::USAGE
      },
    }
  }

  /// The one-project case of [`Self::execute_projects_with_summary`]:
  /// the config is itself the project, so there is nothing to merge or
  /// schedule — only the caller's hooks to honour.
  async fn execute_single_project(&self, plan: TestPlan, bus: EventBus, hooks: &ProjectHooks<'_>) -> ExecuteSummary {
    let name = self.config.name.clone().unwrap_or_default();
    let mut plan = plan;
    if let Some(narrow) = hooks.narrow {
      narrow(&name, &mut plan);
    }
    let (project_bus, drains) = match hooks.stream {
      Some(stream) => {
        let stream = stream(&name);
        (stream.bus, stream.drains)
      },
      None => (bus, Vec::new()),
    };
    let summary = self.execute_with_summary(plan, project_bus.clone()).await;
    if hooks.stream.is_some() {
      project_bus.close();
      for drain in drains {
        let _ = drain.await;
      }
    }
    summary
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
    // When multiple workers launch Chromium locally without an already-shared
    // session browser, share one lazy Chromium process across all workers in
    // this run (each worker gets a scoped Browser view with isolated contexts).
    // Cuts cold-start contention and memory by N× while remaining 100% lazy for
    // fixtureless suites. Set FERRIDRIVER_SHARE_BROWSER=0 to disable.
    let share_enabled = !matches!(std::env::var("FERRIDRIVER_SHARE_BROWSER").as_deref(), Ok("0" | "false"));
    let run_shared_handle = if share_enabled
      && shared_browser.is_none()
      && num_workers > 1
      && connection == ConnectMode::Launch
      && matches!(
        launch_plan.backend,
        ferridriver::backend::BackendKind::CdpPipe | ferridriver::backend::BackendKind::CdpWs
      ) {
      Some(Arc::new(
        BrowserHandle::with_connection(launch_plan.clone(), connection.clone())
          .reporting_launch_failure(Arc::clone(&launch_failed))
          .recycling_contexts(self.config.context_reuse),
      ))
    } else {
      None
    };
    let spawner = WorkerSpawner {
      config: Arc::clone(&self.config),
      event_bus: event_bus.has_subscribers().then(|| event_bus.clone()),
      live_traces: self.live_traces,
      worker_ids: Arc::clone(&self.worker_ids),
      dispatcher: Arc::clone(&dispatcher),
      result_tx,
      custom_fixtures,
      shared_browser,
      run_shared_handle: run_shared_handle.clone(),
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
    if let Some(handle) = run_shared_handle
      && let Err(error) = handle.close().await
    {
      tracing::warn!(target: "ferridriver::runner", "shared browser cleanup failed: {error}");
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

  /// Launch the browser a watch or UI session reuses across its runs,
  /// reporting why when it cannot.
  async fn launch_session_browser(&mut self) -> Option<Arc<Browser>> {
    let launch_plan = match build_launch_plan(&self.config.browser) {
      Ok(plan) => plan,
      Err(message) => {
        eprintln!("Error: {message}");
        return None;
      },
    };
    let connection = match self.config.browser.resolve_instance() {
      Ok(connection) => connection.unwrap_or(ConnectMode::Launch),
      Err(error) => {
        tracing::error!("invalid browser connection: {error}");
        return None;
      },
    };
    let browser = match Box::pin(launch_with_plan(launch_plan.clone(), connection.clone())).await {
      Ok(b) => Arc::new(b),
      Err(e) => {
        eprintln!("Failed to launch browser: {e}");
        return None;
      },
    };
    self.shared_browser = Some(SharedBrowser {
      browser: Arc::clone(&browser),
      plan: launch_plan,
      connection,
    });
    Some(browser)
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
  pub async fn run_watch(&mut self, plan_factory: WatchPlanFactory, watch_root: std::path::PathBuf) -> i32 {
    use crate::watch::FileWatcher;

    self.publish_base_url();

    // Launch browser once — reuse across all watch cycles.
    let Some(browser) = Box::pin(self.launch_session_browser()).await else {
      return 1;
    };

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
        Box::pin(self.run_watch_headless(&watcher, &plan_factory)).await;
      },
    }

    // Cleanup.
    self.shared_browser = None;
    close_session_browser(&browser).await
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
  async fn run_watch_tui(
    &mut self,
    tui: &mut crate::tui::WatchTui,
    tui_tx: tokio::sync::mpsc::UnboundedSender<crate::tui::TuiMessage>,
    watcher: &crate::watch::FileWatcher,
    plan_factory: &WatchPlanFactory,
    _browser: &Arc<Browser>,
  ) {
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
    let plan = plan_or_report(plan_factory(None).await);
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

          let mut plan = build_plan_for_changes(plan_factory, run_all, &changed_paths).await;
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
              if self.run_with_tui_drain(plan_or_report(plan_factory(None).await), tui).await { break; }
              tui.set_status(crate::tui::WatchStatus::Idle);
            }
            WatchCommand::RunFailed => {
              let mut plan = plan_or_report(plan_factory(None).await);
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
              let mut plan = plan_or_report(plan_factory(None).await);
              if let Some(ref pattern) = grep_filter {
                crate::discovery::filter_by_grep(&mut plan, pattern, false);
              }
              if self.run_with_tui_drain(plan, tui).await { break; }
              tui.set_status(crate::tui::WatchStatus::Idle);
            }
            WatchCommand::FilterByName(pattern) => {
              if !pattern.is_empty() {
                grep_filter = Some(pattern.clone());
                let mut plan = plan_or_report(plan_factory(None).await);
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
  async fn run_watch_headless(&mut self, watcher: &crate::watch::FileWatcher, plan_factory: &WatchPlanFactory) {
    // Initial run.
    let plan = plan_or_report(plan_factory(None).await);
    let _ = Box::pin(self.run(plan)).await;
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

      let plan = build_plan_for_changes(plan_factory, run_all, &changed_paths).await;
      if plan.total_tests == 0 {
        eprintln!("No tests matched changed files.");
        continue;
      }

      let _ = Box::pin(self.run(plan)).await;
      eprintln!("\n\x1b[2mWatching for changes (non-interactive)...\x1b[0m\n");
    }
  }

  /// Run in UI mode: a localhost web app (`ferridriver bdd --ui`) that
  /// lists scenarios, streams live results over a websocket, and re-runs
  /// on file changes or UI commands.
  ///
  /// Same skeleton as [`Self::run_watch`]: the browser launches once and
  /// is reused across cycles. Traces are forced on when disabled so every
  /// test produces a trace attachment for the viewer link. No tests run
  /// until a file changes or a client sends a run command.
  pub async fn run_ui(
    &mut self,
    plan_factory: WatchPlanFactory,
    watch_root: std::path::PathBuf,
    port: Option<u16>,
  ) -> i32 {
    use crate::ui_server::{UiCommand, UiServer};
    use crate::watch::FileWatcher;

    self.publish_base_url();

    // The UI follows each test's trace while it is being recorded, which
    // only works if events reach the file as they happen.
    self.live_traces = true;

    // Reclaim spool dirs a SIGKILLed previous session left in the temp
    // dir before this long-lived server starts producing its own, and
    // scratch directories a killed run left in the output dir.
    ferridriver::trace::sweep_stale_spools();
    crate::artifacts::sweep(&self.config.output_dir);

    if self.config.trace == crate::tracing::TraceMode::Off {
      Arc::make_mut(&mut self.config).trace = crate::tracing::TraceMode::On;
    }

    let server = match UiServer::start(self.config.output_dir.clone(), port).await {
      Ok(s) => s,
      Err(e) => {
        eprintln!("Failed to start UI server: {e}");
        return 1;
      },
    };
    let UiServer {
      addr,
      state,
      mut commands,
    } = server;
    if let Err(e) = announce_ui_url(&format!("http://{addr}")) {
      eprintln!("Error: could not write the UI address: {e}");
      return 1;
    }

    // Launch browser once — reuse across all UI-triggered runs.
    let Some(browser) = Box::pin(self.launch_session_browser()).await else {
      return 1;
    };

    let watcher = match FileWatcher::new(&watch_root, &self.config.test_match, &self.config.test_ignore) {
      Ok(w) => w,
      Err(e) => {
        eprintln!("Failed to start file watcher: {e}");
        return 1;
      },
    };

    // Initial plan populates the sidebar; nothing runs until requested.
    let plan = plan_or_report(plan_factory(None).await);
    state.publish_test_list(&plan);

    // Commands that arrive mid-run are buffered here and processed in
    // order once the current run finishes (Stop is consumed by the run
    // itself and cancels it).
    let mut queued: std::collections::VecDeque<UiCommand> = std::collections::VecDeque::new();

    loop {
      if let Some(cmd) = queued.pop_front() {
        if let Some(plan) = self.plan_for_ui_command(&plan_factory, cmd, &state).await {
          let outcome = self.run_plan_for_ui(plan, &state, &mut commands).await;
          if outcome.stopped {
            // Stop cancels the current run AND the backlog — a queued
            // pile of runs executing after an explicit Stop is the
            // opposite of what the user asked for.
            queued.clear();
          } else {
            queue_pending(&mut queued, outcome.pending);
          }
        }
        continue;
      }

      tokio::select! {
        _ = tokio::signal::ctrl_c() => break,

        change = watcher.recv() => {
          let Some(change) = change else { break };
          let mut all_changes = vec![change];
          all_changes.extend(watcher.drain_deduped());

          let (run_all, changed_paths) = classify_changes(&all_changes);
          if !run_all && changed_paths.is_empty() { continue; }

          // Full plan refreshes the sidebar (new/renamed scenarios show
          // up); the run itself is narrowed to the changed files.
          let mut plan = plan_or_report(plan_factory(None).await);
          state.publish_test_list(&plan);
          if !run_all {
            retain_tests_in_files(&mut plan, &changed_paths);
          }
          if plan.total_tests == 0 { continue; }
          let outcome = self.run_plan_for_ui(plan, &state, &mut commands).await;
          if outcome.stopped {
            queued.clear();
          } else {
            queue_pending(&mut queued, outcome.pending);
          }
        }

        cmd = commands.recv() => {
          let Some(cmd) = cmd else { break };
          if !queued.contains(&cmd) {
            queued.push_back(cmd);
          }
        }
      }
    }

    self.shared_browser = None;
    close_session_browser(&browser).await
  }

  /// Serve the run through Playwright's UI-mode app.
  ///
  /// The app is embedded in this binary and speaks the test-server
  /// protocol ([`crate::test_server`]); this method owns everything on
  /// our side of it — one shared browser for every run it asks for,
  /// traces written where its viewer can follow them live, and the loop
  /// that answers its calls until the window closes.
  pub async fn run_test_server(
    &mut self,
    plan_factory: WatchPlanFactory,
    root: std::path::PathBuf,
    host: Option<String>,
    port: Option<u16>,
  ) -> i32 {
    use crate::test_server::{TestServerOptions, driver, start};

    self.publish_base_url();

    // The UI shows each test's trace, including while it runs.
    self.live_traces = true;
    if self.config.trace == crate::tracing::TraceMode::Off {
      Arc::make_mut(&mut self.config).trace = crate::tracing::TraceMode::On;
    }
    ferridriver::trace::sweep_stale_spools();
    crate::artifacts::sweep(&self.config.output_dir);

    let root = std::path::absolute(&root).unwrap_or(root);
    let output_dir = std::path::absolute(&self.config.output_dir).unwrap_or_else(|_| self.config.output_dir.clone());
    let host_given = host.is_some();
    let server = match start(TestServerOptions {
      host: host.unwrap_or_else(|| "127.0.0.1".to_string()),
      port,
      file_roots: vec![root.clone(), output_dir],
    })
    .await
    {
      Ok(server) => server,
      Err(e) => {
        eprintln!("Failed to start the test server: {e}");
        return 1;
      },
    };

    // One browser for every run the UI asks for: a launch per click is
    // most of the wait in a UI session.
    let Some(browser) = Box::pin(self.launch_session_browser()).await else {
      return 1;
    };

    if let Err(e) = announce_ui_url(&server.url) {
      eprintln!("Error: could not write the UI address: {e}");
      return 1;
    }
    // Asking for a host or a port means "serve it, I will connect" — the
    // window is for the default, local case (Playwright splits the same
    // way in `runUIMode`).
    let open_window = host_given || port.is_none();
    let url = server.url.clone();
    let window = tokio::spawn(async move {
      if !open_window {
        std::future::pending::<()>().await;
      }
      if let Err(e) = ferridriver_viewer::open_app_window(&url).await {
        eprintln!("could not open a window ({e}); open the URL above yourself");
        // Nothing to close, so hold the session open until interrupted.
        std::future::pending::<()>().await;
      }
    });

    let runner = std::mem::replace(self, TestRunner::new(TestConfig::default(), CliOverrides::default()));
    // The window is the session: closing it ends the run loop, exactly as
    // it does in Playwright.
    *self = Box::pin(driver::serve(runner, plan_factory, root, server, async move {
      let _ = window.await;
    }))
    .await;

    self.shared_browser = None;
    close_session_browser(&browser).await
  }

  /// Build the (filtered) plan a UI command asks for. Publishes the
  /// refreshed full test list as a side effect. Returns `None` when
  /// nothing matches or the command needs no run (idle `Stop`).
  async fn plan_for_ui_command(
    &self,
    plan_factory: &WatchPlanFactory,
    cmd: crate::ui_server::UiCommand,
    state: &Arc<crate::ui_server::UiState>,
  ) -> Option<TestPlan> {
    use crate::ui_server::UiCommand;

    if cmd == UiCommand::Stop {
      return None;
    }
    let mut plan = plan_or_report(plan_factory(None).await);
    state.publish_test_list(&plan);
    match cmd {
      UiCommand::RunAll | UiCommand::Stop => {},
      UiCommand::RunFailed => {
        // No recorded failures — a no-op, NOT "run everything" (an
        // absent or empty @rerun.txt would otherwise leave the full
        // plan in place).
        let rerun_path = self.config.output_dir.join("@rerun.txt");
        let has_failures = std::fs::read_to_string(&rerun_path)
          .is_ok_and(|contents| contents.lines().any(|line| !line.trim().is_empty()));
        if !has_failures {
          return None;
        }
        crate::discovery::filter_by_rerun(&mut plan, &rerun_path);
      },
      UiCommand::RunGrep(pattern) => {
        crate::discovery::filter_by_grep(&mut plan, &pattern, false);
      },
      UiCommand::RunTest(id) => {
        let exact = format!("^{}$", regex::escape(&id));
        crate::discovery::filter_by_grep(&mut plan, &exact, false);
      },
      UiCommand::RunFile(file) => {
        plan.suites.retain(|s| s.file == file);
        plan.total_tests = plan.suites.iter().map(|s| s.tests.len()).sum();
      },
    }
    (plan.total_tests > 0).then_some(plan)
  }

  /// Execute a plan while streaming reporter events to UI clients.
  ///
  /// Same take/restore reporter dance as `run_with_tui_drain`: terminal
  /// reporters keep printing while a second subscriber forwards every
  /// event (mapped to JSON) into the UI broadcast channel.
  ///
  /// Keeps draining the command channel while tests execute: `Stop`
  /// requests a cooperative cancel ([`RunStop`]) — workers drop queued
  /// tests, finish the ones already executing, and `execute` unwinds
  /// normally so no detached test keeps driving the shared browser
  /// behind an idle UI; every other command is returned for the caller
  /// to run afterwards.
  async fn run_plan_for_ui(
    &mut self,
    plan: TestPlan,
    state: &Arc<crate::ui_server::UiState>,
    commands: &mut tokio::sync::mpsc::UnboundedReceiver<crate::ui_server::UiCommand>,
  ) -> UiRunOutcome {
    state.set_watch_status("running");

    let mut builder = EventBusBuilder::new();
    let driver_handle = if self.reporters.is_empty() {
      None
    } else {
      let reporter_sub = builder.subscribe();
      let reporters = std::mem::take(&mut self.reporters);
      Some(tokio::spawn(ReporterDriver::new(reporters, reporter_sub).run()))
    };
    let ui_sub = builder.subscribe();
    let forwarder = tokio::spawn(Arc::clone(state).forward_run_events(ui_sub));
    let bus = builder.build();

    let mut pending = Vec::new();
    let mut stopped = false;
    self.run_stop.reset();
    let stop = self.run_stop.clone();
    {
      let execute = self.execute(plan, bus.clone());
      tokio::pin!(execute);
      loop {
        tokio::select! {
          _ = &mut execute => break,
          cmd = commands.recv() => match cmd {
            Some(crate::ui_server::UiCommand::Stop) => {
              stopped = true;
              stop.request();
            },
            Some(other) => pending.push(other),
            None => {
              // Command channel gone (server shutting down): cancel and
              // let the run unwind before returning.
              stop.request();
              (&mut execute).await;
              break;
            },
          }
        }
      }
    }
    bus.close();

    if let Some(handle) = driver_handle
      && let Ok(reporters) = handle.await
    {
      self.reporters = reporters;
    }
    let _ = forwarder.await;

    if stopped {
      // A cancelled run emits no `runFinished`; tell clients explicitly
      // so they reset their running state instead of leaking it.
      state.publish_run_cancelled();
    }
    state.set_watch_status("idle");
    UiRunOutcome { pending, stopped }
  }
}

/// What a UI-driven run left behind: commands that arrived mid-run and
/// whether the run was cancelled by Stop.
struct UiRunOutcome {
  pending: Vec<crate::ui_server::UiCommand>,
  stopped: bool,
}

/// Append mid-run commands to the backlog, dropping duplicates —
/// spamming "Run All" during a run must not schedule N more full runs.
fn queue_pending(
  queued: &mut std::collections::VecDeque<crate::ui_server::UiCommand>,
  pending: Vec<crate::ui_server::UiCommand>,
) {
  for cmd in pending {
    if !queued.contains(&cmd) {
      queued.push_back(cmd);
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

/// What a cycle's factory produced: the plan, and what went wrong
/// building it.
///
/// Discovery and bundling failures are the only explanation a watcher or
/// a UI can give for an empty tree, so they travel with the plan rather
/// than reaching a terminal and nothing else.
#[derive(Default)]
pub struct PlanBuild {
  pub plan: TestPlan,
  pub errors: Vec<String>,
}

impl PlanBuild {
  /// A build that went fine.
  #[must_use]
  pub fn ok(plan: TestPlan) -> Self {
    Self {
      plan,
      errors: Vec::new(),
    }
  }

  /// A build that produced nothing but the reason it produced nothing.
  #[must_use]
  pub fn failed(plan: TestPlan, error: impl Into<String>) -> Self {
    Self {
      plan,
      errors: vec![error.into()],
    }
  }
}

/// Async closure producing a fresh [`PlanBuild`] for a watch cycle.
/// `None` = build the full plan; `Some(paths)` = only re-process those
/// files (e.g. re-parse only changed `.feature` files). Async so
/// factories can re-bundle JS/TS step graphs per cycle.
pub type WatchPlanFactory =
  Box<dyn Fn(Option<Vec<std::path::PathBuf>>) -> futures::future::BoxFuture<'static, PlanBuild> + Send + Sync>;

/// Take a cycle's plan, printing whatever went wrong building it — a
/// terminal watch run has no other surface for a discovery failure.
fn plan_or_report(build: PlanBuild) -> TestPlan {
  for error in &build.errors {
    eprintln!("{error}");
  }
  build.plan
}

/// Build a test plan, optionally filtered to changed files.
async fn build_plan_for_changes(
  plan_factory: &WatchPlanFactory,
  run_all: bool,
  changed_paths: &[std::path::PathBuf],
) -> TestPlan {
  let changed = if run_all { None } else { Some(changed_paths.to_vec()) };
  let mut plan = plan_or_report(plan_factory(changed).await);

  // Filter plan to changed files if applicable.
  if !run_all {
    retain_tests_in_files(&mut plan, changed_paths);
  }

  plan
}

/// Narrow a plan to tests whose file matches one of the changed paths
/// (by file name). No-op when `changed_paths` is empty.
fn retain_tests_in_files(plan: &mut TestPlan, changed_paths: &[std::path::PathBuf]) {
  if changed_paths.is_empty() {
    return;
  }
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

/// Playwright's `filterProjects` (`runner/projectUtils.ts`): the
/// top-level projects a run selects, as indices into `projects`.
///
/// With no names, every project whose `default` is not `false` (or every
/// project, when `include_non_default`). Names match case-insensitively
/// and `*` is a wildcard; a literal name that matches nothing is an
/// error, as is a filter that selects nothing.
pub(crate) fn select_projects(
  projects: &[ProjectConfig],
  names: &[String],
  include_non_default: bool,
) -> Result<Vec<usize>, String> {
  if names.is_empty() {
    return Ok(
      projects
        .iter()
        .enumerate()
        .filter(|(_, p)| include_non_default || p.default.unwrap_or(true))
        .map(|(i, _)| i)
        .collect(),
    );
  }
  let available = || {
    projects
      .iter()
      .map(|p| format!("\"{}\"", p.name))
      .collect::<Vec<_>>()
      .join(", ")
  };
  let lowered: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();
  let mut unmatched: Vec<&String> = names
    .iter()
    .zip(&lowered)
    .filter(|(_, l)| !l.contains('*'))
    .map(|(n, _)| n)
    .collect();
  let selected: Vec<usize> = projects
    .iter()
    .enumerate()
    .filter(|(_, p)| {
      let name = p.name.to_lowercase();
      let mut hit = false;
      for (original, pattern) in names.iter().zip(&lowered) {
        if pattern.contains('*') {
          hit |= wildcard_matches(pattern, &name);
        } else if *pattern == name {
          unmatched.retain(|n| *n != original);
          hit = true;
        }
      }
      hit
    })
    .map(|(i, _)| i)
    .collect();
  if !unmatched.is_empty() {
    let unknown = unmatched
      .iter()
      .map(|n| format!("\"{n}\""))
      .collect::<Vec<_>>()
      .join(", ");
    return Err(format!(
      "Project(s) {unknown} not found. Available projects: {}",
      available()
    ));
  }
  if selected.is_empty() {
    return Err(format!("No projects matched. Available projects: {}", available()));
  }
  Ok(selected)
}

/// Whole-string match where `*` stands for any run of characters.
fn wildcard_matches(pattern: &str, text: &str) -> bool {
  let parts: Vec<&str> = pattern.split('*').collect();
  let (first, rest) = parts.split_first().map_or(("", &[][..]), |(f, r)| (*f, r));
  let Some(mut remaining) = text.strip_prefix(first) else {
    return false;
  };
  let Some((last, middle)) = rest.split_last() else {
    return remaining.is_empty();
  };
  for part in middle {
    match remaining.split_once(*part) {
      Some((_, after)) => remaining = after,
      None => return false,
    }
  }
  remaining.len() >= last.len() && remaining.ends_with(last)
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
  dispatcher: Arc<Dispatcher>,
  result_tx: mpsc::Sender<WorkerTestResult>,
  custom_fixtures: FxHashMap<String, crate::fixture::FixtureDef>,
  shared_browser: Option<Arc<Browser>>,
  run_shared_handle: Option<Arc<BrowserHandle>>,
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
    let rx = self.dispatcher.receiver();
    let tx = self.result_tx.clone();
    let custom_fixtures = self.custom_fixtures.clone();
    let shared = self.shared_browser.clone();
    let run_shared = self.run_shared_handle.clone();
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

/// The planned projects of a multi-project run.
struct ProjectPlanning {
  merged: FxHashMap<usize, Arc<TestConfig>>,
  total_tests: usize,
  /// Projects others depend on, which run whole: no file filters.
  exempt: rustc_hash::FxHashSet<usize>,
  exempt_overrides: CliOverrides,
}

/// What every project of a multi-project run is launched with.
struct ProjectRunContext<'a> {
  projects: &'a [ProjectConfig],
  planning: &'a ProjectPlanning,
  num_workers: u32,
  hooks: &'a ProjectHooks<'a>,
  bus: &'a EventBus,
}

/// Where the ready-set scheduler stands.
struct ProjectSchedule {
  /// Each project's prerequisites, as `(project, must_pass)`.
  prereqs: FxHashMap<usize, Vec<(usize, bool)>>,
  remaining: Vec<usize>,
  passed: rustc_hash::FxHashSet<usize>,
  terminal: rustc_hash::FxHashSet<usize>,
  exit_code: i32,
  agg: ExecuteSummary,
}

impl ProjectSchedule {
  fn new(prereqs: FxHashMap<usize, Vec<(usize, bool)>>, scheduled: Vec<usize>) -> Self {
    Self {
      prereqs,
      remaining: scheduled,
      passed: rustc_hash::FxHashSet::default(),
      terminal: rustc_hash::FxHashSet::default(),
      exit_code: exit::PASSED,
      agg: ExecuteSummary::default(),
    }
  }

  /// Take a not-yet-started project whose prerequisites are all terminal.
  fn next_ready(&mut self) -> Option<usize> {
    let idx = self.remaining.iter().copied().find(|&idx| {
      self
        .prereqs
        .get(&idx)
        .is_none_or(|rs| rs.iter().all(|(dep, _)| self.terminal.contains(dep)))
    })?;
    self.remaining.retain(|&i| i != idx);
    Some(idx)
  }

  /// A prerequisite that had to pass did not.
  fn is_blocked(&self, idx: usize) -> bool {
    self.prereqs.get(&idx).is_some_and(|rs| {
      rs.iter()
        .any(|&(dep, must_pass)| must_pass && !self.passed.contains(&dep))
    })
  }

  /// Record a project that finished, releasing what it held back.
  fn finished(
    &mut self,
    joined: Result<(usize, Option<ExecuteSummary>), tokio::task::JoinError>,
    rosters: &FxHashMap<usize, Arc<crate::dispatcher::WorkerRoster>>,
    worker_slots: &Arc<crate::dispatcher::RunSlots>,
  ) {
    match joined {
      Ok((idx, Some(summary))) => {
        if let Some(roster) = rosters.get(&idx) {
          worker_slots.started(roster);
        }
        self.terminal.insert(idx);
        if summary.exit_code == exit::PASSED {
          self.passed.insert(idx);
        } else {
          self.exit_code = exit::worst(self.exit_code, summary.exit_code);
        }
        self.agg.passed += summary.passed;
        self.agg.failed += summary.failed;
        self.agg.skipped += summary.skipped;
        self.agg.flaky += summary.flaky;
      },
      Ok((idx, None)) => {
        if let Some(roster) = rosters.get(&idx) {
          worker_slots.started(roster);
        }
        self.terminal.insert(idx);
        self.exit_code = exit::worst(self.exit_code, exit::FAILED);
      },
      Err(e) => {
        tracing::error!(target: "ferridriver::runner", "project task panicked: {e}");
        // Which project it was is gone with it; none may hold slots
        // back for a start that will not come.
        for roster in rosters.values() {
          worker_slots.started(roster);
        }
        self.exit_code = exit::worst(self.exit_code, exit::INFRASTRUCTURE);
      },
    }
  }
}

/// Each scheduled project's prerequisites, as `(project, must_pass)`.
fn project_prerequisites(
  projects: &[ProjectConfig],
  scheduled: &[usize],
  cli_teardown_idx: Option<usize>,
) -> FxHashMap<usize, Vec<(usize, bool)>> {
  // Pre-compute each scheduled project's prerequisites and whether it is a
  // teardown. The ready-set scheduler spawns a project once all its
  // prerequisites have reached a terminal state.
  //
  // - A normal project requires every `dependencies` entry to have PASSED.
  //   If any dependency failed/was skipped, the project is itself skipped.
  // - A teardown project (referenced by another project's `teardown` field)
  //   requires only that its declaring parent reached a terminal state — it
  //   runs even if the parent failed (Playwright teardown semantics).
  // - The CLI-supplied teardown requires every other selected project to be
  //   terminal.
  let teardown_parent: FxHashMap<usize, usize> = projects
    .iter()
    .enumerate()
    .filter_map(|(parent_idx, p)| {
      p.teardown
        .as_deref()
        .and_then(|name| projects.iter().position(|q| q.name == name))
        .map(|td_idx| (td_idx, parent_idx))
    })
    .collect();

  // Prerequisites by index: (prereq_idx, must_pass).
  scheduled
    .iter()
    .map(|&idx| {
      let mut reqs: Vec<(usize, bool)> = Vec::new();
      // Normal dependencies must pass.
      for dep_name in &projects[idx].dependencies {
        if let Some(dep_idx) = projects.iter().position(|p| &p.name == dep_name)
          && scheduled.contains(&dep_idx)
        {
          reqs.push((dep_idx, true));
        }
      }
      // Teardown parent must merely be terminal.
      if let Some(&parent_idx) = teardown_parent.get(&idx)
        && scheduled.contains(&parent_idx)
      {
        reqs.push((parent_idx, false));
      }
      // CLI-supplied teardown waits on every other scheduled project.
      if Some(idx) == cli_teardown_idx {
        for &other in scheduled {
          if other != idx {
            reqs.push((other, false));
          }
        }
      }
      (idx, reqs)
    })
    .collect()
}

/// Topologically sort projects by `dependencies`. Returns indices in execution order.
///
/// Uses Kahn's algorithm. Returns `Err` if there's a cycle or a missing dependency.
fn topo_sort_projects(projects: &[ProjectConfig]) -> Result<Vec<usize>, ferridriver::FerriError> {
  let name_to_idx: FxHashMap<&str, usize> = projects.iter().enumerate().map(|(i, p)| (p.name.as_str(), i)).collect();

  // Build adjacency list + in-degree.
  let n = projects.len();
  let mut in_degree = vec![0usize; n];
  let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];

  for (i, project) in projects.iter().enumerate() {
    for dep_name in &project.dependencies {
      let &dep_idx = name_to_idx.get(dep_name.as_str()).ok_or_else(|| {
        ferridriver::FerriError::invalid_argument(
          "dependencies",
          format!("project '{}' depends on unknown project '{dep_name}'", project.name),
        )
      })?;
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
    return Err(ferridriver::FerriError::invalid_argument(
      "dependencies",
      "circular dependency detected among projects",
    ));
  }

  Ok(order)
}

/// Filter a test plan for a specific project's scope.
///
/// Playwright's `shuffleTestGroups`: the order is the units sorted by a
/// hash of the seed and each unit's first test, so one seed always gives
/// one order.
fn shuffle_work(units: &mut [crate::dispatcher::WorkItem], seed: &str) {
  use sha2::Digest as _;
  units.sort_by_cached_key(|unit| {
    let first = match unit {
      crate::dispatcher::WorkItem::Single(assignment) => Some(&assignment.test.id),
      crate::dispatcher::WorkItem::Serial(batch) => batch.assignments.first().map(|a| &a.test.id),
    };
    let key = first.map(crate::model::TestId::execution_key).unwrap_or_default();
    sha2::Sha256::digest(format!(
      "{seed}\x1e{}\x1e{key}",
      first.map_or("", |id| id.file.as_str())
    ))
    .to_vec()
  });
}

/// The run's `metadata` with Playwright's `shuffleSeed`, which the run
/// header reports.
fn with_shuffle_seed(mut metadata: serde_json::Value, seed: Option<&str>) -> serde_json::Value {
  let Some(seed) = seed else {
    return metadata;
  };
  match &mut metadata {
    serde_json::Value::Object(map) => {
      map.insert("shuffleSeed".into(), seed.into());
    },
    other => *other = serde_json::json!({ "shuffleSeed": seed }),
  }
  metadata
}

/// For each scheduled project, the scheduled projects that depend on it
/// or name it as their teardown: Playwright's `buildProjectsClosure`
/// marks such a project a dependency, which runs every test it has
/// whatever files the command line named, and only when a project that
/// needs it has tests to run. Indices are into `projects`; a run that
/// is no configured project (the implicit one a config without projects
/// runs) has nothing depending on it.
/// For each scheduled project, the longest chain of work that can only
/// start once it has finished: a project depending on it, then one
/// depending on that, each taking `span`.
fn work_waiting_on(
  projects: &[ProjectConfig],
  scheduled: &[usize],
  span: impl Fn(usize) -> u64,
) -> FxHashMap<usize, u64> {
  fn visit(
    idx: usize,
    projects: &[ProjectConfig],
    scheduled: &[usize],
    span: &dyn Fn(usize) -> u64,
    memo: &mut FxHashMap<usize, u64>,
  ) -> u64 {
    if let Some(&known) = memo.get(&idx) {
      return known;
    }
    // Dependencies were checked for cycles; this only keeps a malformed
    // graph from recursing without end.
    memo.insert(idx, 0);
    let name = projects.get(idx).map_or("", |project| project.name.as_str());
    let longest = scheduled
      .iter()
      .copied()
      .filter(|&other| {
        projects
          .get(other)
          .is_some_and(|project| project.dependencies.iter().any(|dep| dep == name))
      })
      .map(|other| span(other).saturating_add(visit(other, projects, scheduled, span, memo)))
      .max()
      .unwrap_or(0);
    memo.insert(idx, longest);
    longest
  }
  let mut memo = FxHashMap::default();
  for &idx in scheduled {
    visit(idx, projects, scheduled, &span, &mut memo);
  }
  memo
}

fn dependents_of(projects: &[ProjectConfig], scheduled: &[usize]) -> FxHashMap<usize, Vec<usize>> {
  scheduled
    .iter()
    .map(|&idx| {
      let Some(name) = projects.get(idx).map(|project| project.name.as_str()) else {
        return (idx, Vec::new());
      };
      let needing = scheduled
        .iter()
        .copied()
        .filter(|&other| {
          other != idx
            && projects.get(other).is_some_and(|project| {
              project.dependencies.iter().any(|dep| dep == name) || project.teardown.as_deref() == Some(name)
            })
        })
        .collect();
      (idx, needing)
    })
    .collect()
}

/// `overrides` without the positional file and `file:line` filters, as a
/// dependency project runs.
fn without_file_filters(overrides: &CliOverrides) -> CliOverrides {
  let mut overrides = overrides.clone();
  overrides.test_files.clear();
  overrides.locations.clear();
  overrides
}

/// Applies project-level `test_dir`, `test_match`, `test_ignore`, grep, `grep_invert`, and tag filters.
fn filter_plan_for_project(plan: &mut TestPlan, config: &TestConfig, project: &ProjectConfig) {
  // Filter by test_dir: only keep suites whose file lives under it.
  // `testDir` is anchored to the config file it was written in, so it
  // arrives absolute while a plan's files are relative to the run's
  // working directory — comparing the two as written keeps nothing.
  if let Some(ref test_dir) = config.test_dir {
    let dir = absolute_path(test_dir);
    plan.suites.retain(|suite| {
      let file = absolute_path(&suite.file);
      let kept = file.starts_with(&dir);
      if !kept {
        tracing::debug!(
          target: "ferridriver::runner",
          project = project.name,
          suite = %file.display(),
          test_dir = %dir.display(),
          "project filter dropped a suite: outside testDir",
        );
      }
      kept
    });
  }

  // The project's own testMatch and testIgnore (the merged config
  // carries them), as Playwright's `collectFilesForProject` applies them.
  // A file scriptMatch names runs as a script, so it is kept as well.
  let test_match = crate::discovery::FileMatcher::new(&config.test_match);
  let script_match = crate::discovery::FileMatcher::new(&config.script_match);
  let test_ignore = crate::discovery::FileMatcher::new(&config.test_ignore);
  if !test_match.is_empty() || !test_ignore.is_empty() {
    crate::discovery::filter_by_file(plan, |file| {
      (test_match.is_empty() || test_match.matches(file) || script_match.matches(file)) && !test_ignore.matches(file)
    });
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

/// `path` in the form a prefix comparison can trust.
///
/// Two things break the naive `std::path::absolute`. It keeps `..`
/// verbatim, so a plan file discovered OUTSIDE the working directory
/// arrives as `<cwd>/../../tmp/specs/a.spec.ts` and is under no
/// `testDir`; and a bundler reports a spec through its REAL path
/// (`/private/var/...`) while `testDir` keeps the symlink the user
/// wrote (`/var/...`). Either one makes every project report "no tests
/// matched" for a config a single-project run happily executes.
///
/// So: canonicalize when the path exists (resolving symlinks the same
/// way for both sides), and fall back to a lexical absolute+normalize
/// when it does not — a `testDir` may name a directory nothing created
/// yet.
fn absolute_path(path: &str) -> std::path::PathBuf {
  let path = std::path::Path::new(path);
  if let Ok(real) = std::fs::canonicalize(path) {
    return real;
  }
  let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
  ferridriver_config::layer::normalize_path(&absolute)
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

async fn close_session_browser(browser: &Browser) -> i32 {
  match browser.close().await {
    Ok(()) => 0,
    Err(error) => {
      eprintln!("browser cleanup failed: {error}");
      1
    },
  }
}

/// Build the launch plan for a run.
///
/// # Errors
///
/// Returns a message when the configured instance cannot be resolved
/// (unusable name, failing args command, proxy credentials).
fn build_launch_plan(browser_config: &crate::config::BrowserConfig) -> Result<LaunchPlan, String> {
  // BrowserConfig is already normalized (browser↔backend consistent)
  // and validated at load, so the mapping cannot silently downgrade an
  // unrecognised backend here.
  let (backend, kind) = browser_config.resolve_kinds().map_err(|error| error.to_string())?;
  let overrides = browser_config.instance_overrides()?;
  let selection = ferridriver::options::BrowserSelection::resolve(
    overrides.kind.or_else(|| overrides.backend.is_none().then_some(kind)),
    overrides
      .backend
      .or_else(|| overrides.kind.is_none().then_some(backend)),
  )
  .map_err(|error| error.to_string())?;
  let (backend, kind) = (selection.backend, selection.browser);

  let mut args = browser_config.args.clone();
  // `use.proxy` is a per-context option, as Playwright's `proxy` fixture
  // is (`build_context_options`), not a launch flag.
  // Ignore HTTPS errors launch arg.
  if browser_config.use_options.ignore_https_errors {
    args.push("--ignore-certificate-errors".to_string());
  }

  // Force headless under CI even if the config left the default
  // (`false`) in place. Headed Chrome / Firefox on a runner with no
  // DISPLAY hangs the launch handshake past the per-command timeout.
  // Matches Playwright's `process.env.CI` handling in
  // `packages/playwright/src/index.ts` (the `headless` option fixture
  // defaults to `!process.env.PWDEBUG`).
  let headless = browser_config.headless || std::env::var("CI").is_ok();

  args.extend(overrides.args);

  Ok(LaunchPlan {
    backend,
    kind,
    headless: overrides.headless.unwrap_or(headless),
    executable_path: overrides
      .executable_path
      .or_else(|| browser_config.executable_path.clone()),
    user_data_dir: overrides.user_data_dir,
    env: (!overrides.env.is_empty()).then_some(overrides.env),
    ignore_default_args: overrides.ignore_default_args,
    slow_mo: browser_config.slow_mo,
    firefox_user_prefs: (!browser_config.firefox_user_prefs.is_empty()).then(|| {
      browser_config
        .firefox_user_prefs
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
    }),
    args,
    default_viewport: browser_config
      .viewport
      .as_ref()
      .map_or_else(
        || Some(crate::config::ViewportConfig::default()),
        ferridriver_config::browser::ViewportOverride::size,
      )
      .map(|v| ferridriver::options::ViewportConfig {
        width: v.width,
        height: v.height,
        ..Default::default()
      }),
    ..Default::default()
  })
}

/// Launch a browser using the runner's internal `LaunchPlan`. Wraps
/// `BrowserState::with_plan` + `Browser::from_state` so callers don't
/// need to repeat the handshake-await dance.
pub(crate) async fn launch_with_plan(plan: LaunchPlan, connection: ConnectMode) -> ferridriver::error::Result<Browser> {
  let mut state = BrowserState::with_plan(connection, plan);
  Box::pin(state.ensure_browser()).await?;
  Ok(Browser::from_state(state))
}

/// Lazy-launch handle for a worker's browser. The browser is launched
/// on first `get()` call and cached. Workers that never access the
/// browser (e.g. config-only tests) skip the launch entirely — under
/// CI conditions where Chromium first-launch can take >30s, this
/// keeps non-browser tests inside the per-test deadline.
pub struct BrowserHandle {
  plan: LaunchPlan,
  connection: ConnectMode,
  cell: Arc<tokio::sync::OnceCell<Arc<Browser>>>,
  launch: Arc<tokio::sync::Mutex<BrowserLaunchState>>,
  shared: bool,
  /// Worker scope id when borrowing a run-shared Chromium process so each
  /// worker's `browser.contexts()` and `browser.on('context')` stay isolated.
  worker_scope: Option<u64>,
  /// Cached worker-scoped `Browser` view so every `get()` / `try_get()` on
  /// this worker shares the exact same event emitter and disconnect latch.
  scoped_cell: std::sync::OnceLock<Arc<Browser>>,
  /// Raised when a launch fails, so the run can tell a browser that never
  /// started from tests that failed on their own.
  launch_failed: Option<Arc<std::sync::atomic::AtomicBool>>,
  /// `contextReuse`: the browser keeps closed contexts for the next test.
  recycle_contexts: bool,
}

type BrowserLaunch = futures::future::Shared<futures::future::BoxFuture<'static, ferridriver::Result<Arc<Browser>>>>;

#[derive(Default)]
struct BrowserLaunchState {
  closed: bool,
  pending: Option<BrowserLaunch>,
}

#[cfg(test)]
mod run_stop_tests {
  use super::RunStop;
  use std::time::Duration;

  /// Every project of a run waits on one stop; a request reaches all of
  /// them, including one that started waiting after it.
  #[tokio::test]
  async fn a_stop_reaches_every_project_waiting_on_it() {
    let stop = RunStop::default();
    let waiting: Vec<_> = (0..3)
      .map(|_| {
        let stop = stop.clone();
        tokio::spawn(async move { stop.wait().await })
      })
      .collect();
    tokio::task::yield_now().await;
    stop.request();
    for project in waiting {
      tokio::time::timeout(Duration::from_millis(100), project)
        .await
        .expect("every waiting project wakes")
        .expect("task");
    }
    tokio::time::timeout(Duration::from_millis(100), stop.wait())
      .await
      .expect("a project that waits after the request returns at once");
  }
}

#[cfg(test)]
mod browser_launch_ownership_tests {
  use super::*;
  use axum::{Json, Router, routing};
  use serde_json::json;
  use std::sync::atomic::{AtomicBool, Ordering};
  use std::time::Duration;
  use tokio::sync::Notify;

  #[tokio::test]
  async fn close_waits_for_cancelled_browser_setup_and_deletes_its_session() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let deleted = Arc::new(AtomicBool::new(false));
    let create = {
      let entered = Arc::clone(&entered);
      let release = Arc::clone(&release);
      move || {
        let entered = Arc::clone(&entered);
        let release = Arc::clone(&release);
        async move {
          entered.notify_one();
          release.notified().await;
          Json(json!({"value":{"sessionId":"owned","capabilities":{
            "browserName":"safari","browserVersion":"26.2","setWindowRect":false
          }}}))
        }
      }
    };
    let delete = {
      let deleted = Arc::clone(&deleted);
      move || {
        let deleted = Arc::clone(&deleted);
        async move {
          deleted.store(true, Ordering::Release);
          Json(json!({"value":null}))
        }
      }
    };
    let app = Router::new()
      .route("/session", routing::post(create))
      .route(
        "/session/owned/window/handles",
        routing::get(async || Json(json!({"value":[]}))),
      )
      .route("/session/owned", routing::delete(delete));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let handle = Arc::new(BrowserHandle::with_connection(
      LaunchPlan {
        backend: ferridriver::backend::BackendKind::WebDriver,
        kind: ferridriver::options::BrowserKind::Safari,
        ..Default::default()
      },
      ConnectMode::WebDriver {
        endpoint,
        browser_name: "safari".into(),
        protocol: ferridriver::backend::webdriver::WebDriverProtocol::Classic,
        capabilities: None,
        headers: None,
        timeout: Some(2000),
      },
    ));
    let setup = tokio::spawn({
      let handle = Arc::clone(&handle);
      async move { handle.get().await }
    });
    entered.notified().await;
    setup.abort();
    assert!(matches!(setup.await, Err(error) if error.is_cancelled()));
    let mut closing = tokio::spawn(async move { handle.close().await });
    assert!(
      tokio::time::timeout(Duration::from_millis(25), &mut closing)
        .await
        .is_err()
    );
    release.notify_one();
    closing.await.unwrap().unwrap();
    assert!(deleted.load(Ordering::Acquire));
    server.abort();
  }
}

impl BrowserHandle {
  #[must_use]
  pub fn new(plan: LaunchPlan) -> Self {
    Self::with_connection(plan, ConnectMode::Launch)
  }

  fn with_connection(plan: LaunchPlan, connection: ConnectMode) -> Self {
    Self {
      plan,
      connection,
      cell: Arc::new(tokio::sync::OnceCell::new()),
      launch: Arc::new(tokio::sync::Mutex::default()),
      shared: false,
      worker_scope: None,
      scoped_cell: std::sync::OnceLock::new(),
      launch_failed: None,
      recycle_contexts: false,
    }
  }

  /// Borrow a lazy handle owned by the runner, scoping each worker's
  /// `Browser` view so `browser.contexts()` / `browser.on('context')` remain
  /// isolated per worker and `close()` leaves shutdown to the owner.
  fn borrow_shared_scoped(&self, scope: u64) -> Self {
    Self {
      plan: self.plan.clone(),
      connection: self.connection.clone(),
      cell: Arc::clone(&self.cell),
      launch: Arc::clone(&self.launch),
      shared: true,
      worker_scope: Some(scope),
      scoped_cell: std::sync::OnceLock::new(),
      launch_failed: self.launch_failed.clone(),
      recycle_contexts: self.recycle_contexts,
    }
  }

  /// Raise `flag` whenever this handle fails to launch its browser.
  #[must_use]
  pub fn reporting_launch_failure(mut self, flag: Arc<std::sync::atomic::AtomicBool>) -> Self {
    self.launch_failed = Some(flag);
    self
  }

  /// Have the browser this handle launches keep closed contexts for the
  /// next test (`[test] contextReuse`).
  #[must_use]
  pub fn recycling_contexts(mut self, enabled: bool) -> Self {
    self.recycle_contexts = enabled;
    self
  }

  /// Wrap a pre-launched browser (watch-mode shared) — `close()` is a
  /// no-op so the shared browser survives across runs.
  #[must_use]
  pub fn from_shared(browser: Arc<Browser>) -> Self {
    let cell = tokio::sync::OnceCell::new();
    let _ = cell.set(browser);
    Self {
      plan: LaunchPlan::default(),
      connection: ConnectMode::Launch,
      cell: Arc::new(cell),
      launch: Arc::new(tokio::sync::Mutex::default()),
      shared: true,
      worker_scope: None,
      scoped_cell: std::sync::OnceLock::new(),
      launch_failed: None,
      recycle_contexts: false,
    }
  }

  #[tracing::instrument(skip_all, name = "browser_launch")]
  /// The browser if one has already been launched, without launching one.
  ///
  /// For callers that only want to act on a browser that is already driving
  /// (the `--debug` pause), where launching one would be beside the point.
  #[must_use]
  pub fn peek(&self) -> Option<Arc<Browser>> {
    self.cell.get().cloned()
  }

  fn scope_browser(&self, browser: Arc<Browser>) -> Arc<Browser> {
    match self.worker_scope {
      Some(scope) => Arc::clone(
        self
          .scoped_cell
          .get_or_init(|| Arc::new(browser.scoped_for_worker(scope))),
      ),
      None => browser,
    }
  }

  /// The browser, launching it on the first call; concurrent callers
  /// share the one launch.
  ///
  /// # Errors
  ///
  /// Returns [`ferridriver::FerriError::TargetClosed`] once the handle is
  /// closed, or the launch or connection failure.
  pub async fn get(&self) -> ferridriver::error::Result<Arc<Browser>> {
    use futures::FutureExt;
    let pending = {
      let mut state = self.launch.lock().await;
      if state.closed {
        return Err(ferridriver::FerriError::TargetClosed { reason: None });
      }
      if let Some(browser) = self.cell.get() {
        return Ok(self.scope_browser(Arc::clone(browser)));
      }
      state
        .pending
        .get_or_insert_with(|| {
          let plan = self.plan.clone();
          let connection = self.connection.clone();
          let cell = Arc::clone(&self.cell);
          let recycle_contexts = self.recycle_contexts;
          // A cancelled fixture must not abandon a session still being created by the driver.
          let task = tokio::spawn(async move {
            let browser = Arc::new(launch_with_plan(plan, connection).await?);
            browser.set_context_recycling(recycle_contexts).await;
            let _ = cell.set(Arc::clone(&browser));
            Ok(browser)
          });
          async move {
            task
              .await
              .map_err(|error| ferridriver::FerriError::backend(format!("browser launch task failed: {error}")))?
          }
          .boxed()
          .shared()
        })
        .clone()
    };
    let result = pending.clone().await;
    if result.is_err() {
      if let Some(flag) = &self.launch_failed {
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
      }
      let mut state = self.launch.lock().await;
      if state.pending.as_ref().is_some_and(|current| current.ptr_eq(&pending)) {
        state.pending = None;
      }
    }
    result.map(|browser| self.scope_browser(browser))
  }

  pub fn try_get(&self) -> Option<Arc<Browser>> {
    self.cell.get().map(|browser| self.scope_browser(Arc::clone(browser)))
  }

  /// Close the browser this handle launched; a shared browser is left to
  /// its owner.
  ///
  /// # Errors
  ///
  /// Returns the launch failure of a launch still in flight, or the
  /// browser's close failure.
  pub async fn close(&self) -> ferridriver::Result<()> {
    if self.shared {
      return Ok(());
    }
    let pending = {
      let mut state = self.launch.lock().await;
      state.closed = true;
      state.pending.clone()
    };
    if let Some(pending) = pending {
      pending.await?.close().await?;
    } else if let Some(browser) = self.cell.get() {
      browser.close().await?;
    }
    Ok(())
  }
}

#[cfg(test)]
mod project_plan_tests {
  use std::sync::Arc;

  use rustc_hash::FxHashMap;

  use super::TestRunner;
  use crate::config::{ProjectConfig, TestConfig};
  use crate::model::{ExpectedStatus, Hooks, SuiteMode, TestCase, TestId, TestPlan, TestSuite};

  fn plan_named(suite: &str) -> TestPlan {
    TestPlan {
      repetitions_expanded: false,
      suites: vec![TestSuite {
        serial_scope: None,
        name: suite.to_string(),
        file: format!("{suite}.feature"),
        tests: vec![TestCase {
          metadata: None,
          id: TestId {
            root_path: None,
            repeat_each_index: 0,
            file: format!("{suite}.feature"),
            suite: Some(suite.to_string()),
            name: "a scenario".to_string(),
            line: None,
            column: None,
          },
          test_fn: Arc::new(|_| Box::pin(async { Ok(()) })),
          fixture_requests: Vec::new(),
          annotations: Vec::new(),
          timeout: None,
          retries: None,
          expected_status: ExpectedStatus::Pass,
          use_options: None,
        }],
        hooks: Hooks::default(),
        annotations: Vec::new(),
        mode: SuiteMode::Parallel,
      }],
      total_tests: 1,
      shard: None,
    }
  }

  fn runner_with(projects: Vec<&str>) -> TestRunner {
    TestRunner::new(
      TestConfig {
        projects: projects
          .into_iter()
          .map(|name| ProjectConfig {
            name: name.to_string(),
            ..Default::default()
          })
          .collect(),
        reporter: Vec::new(),
        ..Default::default()
      },
      crate::config::CliOverrides::default(),
    )
  }

  /// One suite per file, each with one test, as discovery hands them.
  fn plan_of_files(files: &[std::path::PathBuf]) -> TestPlan {
    let suites: Vec<TestSuite> = files
      .iter()
      .map(|file| {
        let mut suite = plan_named("suite").suites.remove(0);
        suite.file = file.to_string_lossy().into_owned();
        for test in &mut suite.tests {
          test.id.file.clone_from(&suite.file);
        }
        suite
      })
      .collect();
    TestPlan {
      repetitions_expanded: false,
      total_tests: suites.len(),
      suites,
      shard: None,
    }
  }

  /// A run that is no configured project, as a config without projects
  /// has, has nothing depending on it; one named among projects keeps
  /// the projects that need it.
  #[test]
  fn dependents_of_a_run_outside_the_configured_projects_are_none() {
    let projects = vec![
      ProjectConfig {
        name: "setup".into(),
        ..Default::default()
      },
      ProjectConfig {
        name: "app".into(),
        dependencies: vec!["setup".into()],
        ..Default::default()
      },
    ];
    let dependents = super::dependents_of(&[], &[usize::MAX]);
    assert_eq!(dependents.get(&usize::MAX), Some(&Vec::new()));
    let dependents = super::dependents_of(&projects, &[0, 1, usize::MAX]);
    assert_eq!(dependents.get(&0), Some(&vec![1]));
    assert_eq!(dependents.get(&1), Some(&Vec::new()));
  }

  /// Each project runs only the files its own testMatch and testIgnore
  /// pick, and a file named on the command line still brings the setup
  /// project its project depends on, as Playwright's
  /// `collectProjectsAndTestFiles` does.
  #[test]
  fn projects_run_their_own_files_and_a_named_file_keeps_its_setup() {
    let dir = tempfile::tempdir().expect("temp dir");
    let e2e = dir.path().join("e2e");
    std::fs::create_dir_all(e2e.join("console")).expect("dirs");
    let files: Vec<std::path::PathBuf> = ["portal.spec.ts", "console/owner.setup.ts", "console/dunning.spec.ts"]
      .iter()
      .map(|name| {
        let file = e2e.join(name);
        std::fs::write(&file, "").expect("write spec");
        crate::identity::comparable_path(&file)
      })
      .collect();
    let plan = plan_of_files(&files);
    let projects = vec![
      ProjectConfig {
        name: "chromium".into(),
        test_ignore: Some(vec!["console/**".into()]),
        ..Default::default()
      },
      ProjectConfig {
        name: "setup".into(),
        test_match: Some(vec!["console/owner.setup.ts".into()]),
        ..Default::default()
      },
      ProjectConfig {
        name: "console".into(),
        test_match: Some(vec!["console/**/*.spec.ts".into()]),
        dependencies: vec!["setup".into()],
        ..Default::default()
      },
    ];
    let listed = |overrides: crate::config::CliOverrides| -> Vec<(String, String)> {
      let runner = TestRunner::new(
        TestConfig {
          projects: projects.clone(),
          test_match: vec!["**/*.spec.ts".into()],
          reporter: Vec::new(),
          ..Default::default()
        },
        overrides,
      );
      runner
        .list(&plan)
        .expect("list")
        .into_iter()
        .map(|test| {
          let name = std::path::Path::new(&test.id.file)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
          (test.record.project, name)
        })
        .collect()
    };
    let pairs = |items: &[(&str, &str)]| -> Vec<(String, String)> {
      items.iter().map(|(a, b)| ((*a).to_owned(), (*b).to_owned())).collect()
    };

    assert_eq!(
      listed(crate::config::CliOverrides::default()),
      pairs(&[
        ("chromium", "portal.spec.ts"),
        ("setup", "owner.setup.ts"),
        ("console", "dunning.spec.ts")
      ])
    );
    let dunning = crate::config::CliOverrides {
      test_files: vec![files[2].to_string_lossy().into_owned()],
      ..Default::default()
    };
    assert_eq!(
      listed(dunning),
      pairs(&[("setup", "owner.setup.ts"), ("console", "dunning.spec.ts")])
    );
    let portal = crate::config::CliOverrides {
      test_files: vec!["portal".into()],
      ..Default::default()
    };
    assert_eq!(listed(portal), pairs(&[("chromium", "portal.spec.ts")]));
  }

  /// The run's worker slots serve whichever project has work, as
  /// Playwright's dispatcher does: once a short project finishes, a long
  /// one started beside it takes its slot and runs on every slot the run
  /// has, rather than on the share it was given when both started.
  #[tokio::test]
  async fn a_project_takes_the_slots_another_project_gives_back() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let dir = tempfile::tempdir().expect("temp dir");
    let file = |name: &str| {
      let path = dir.path().join(name);
      std::fs::write(&path, "").expect("write spec");
      crate::identity::comparable_path(&path)
    };
    let mut files = vec![file("short.spec.ts")];
    files.extend((0..8).map(|n| file(&format!("long-{n}.spec.ts"))));
    let mut plan = plan_of_files(&files);
    let running = Arc::new(AtomicUsize::new(0));
    let most = Arc::new(AtomicUsize::new(0));
    for suite in plan.suites.iter_mut().filter(|suite| suite.file.contains("long-")) {
      for test in &mut suite.tests {
        let running = Arc::clone(&running);
        let most = Arc::clone(&most);
        test.test_fn = Arc::new(move |_| {
          let running = Arc::clone(&running);
          let most = Arc::clone(&most);
          Box::pin(async move {
            let now = running.fetch_add(1, Ordering::SeqCst) + 1;
            most.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            running.fetch_sub(1, Ordering::SeqCst);
            Ok(())
          })
        });
      }
    }
    let project = |name: &str, pattern: &str| ProjectConfig {
      name: name.into(),
      test_match: Some(vec![pattern.into()]),
      ..Default::default()
    };
    let mut runner = TestRunner::new(
      TestConfig {
        projects: vec![project("long", "long-*.spec.ts"), project("short", "short.spec.ts")],
        test_match: vec!["**/*.spec.ts".into()],
        workers: Some(2),
        reporter: Vec::new(),
        ..Default::default()
      },
      crate::config::CliOverrides::default(),
    );
    assert_eq!(runner.run(plan).await, super::exit::PASSED);
    assert_eq!(most.load(Ordering::SeqCst), 2);
  }

  /// A run keeps how long each test took, and the next run starts the
  /// longest first, so no long test is left to run alone at the end.
  #[tokio::test]
  async fn the_next_run_starts_the_tests_that_took_longest() {
    use std::sync::Mutex;

    let dir = tempfile::tempdir().expect("temp dir");
    // Far enough apart that a timer ticking every 15.6ms (Windows) or a
    // loaded scheduler cannot swap two of them.
    let lengths = [20u64, 160, 60, 110];
    let files: Vec<std::path::PathBuf> = lengths
      .iter()
      .map(|ms| {
        let path = dir.path().join(format!("takes-{ms}.spec.ts"));
        std::fs::write(&path, "").expect("write spec");
        crate::identity::comparable_path(&path)
      })
      .collect();
    let started: Arc<Mutex<Vec<u64>>> = Arc::default();
    let plan = || {
      let mut plan = plan_of_files(&files);
      for (suite, &ms) in plan.suites.iter_mut().zip(&lengths) {
        let started = Arc::clone(&started);
        suite.tests[0].test_fn = Arc::new(move |_| {
          let started = Arc::clone(&started);
          Box::pin(async move {
            started.lock().expect("order").push(ms);
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
            Ok(())
          })
        });
      }
      plan
    };
    let run = || {
      TestRunner::new(
        TestConfig {
          config_dir: Some(dir.path().to_path_buf()),
          workers: Some(1),
          reporter: Vec::new(),
          ..Default::default()
        },
        crate::config::CliOverrides::default(),
      )
    };
    assert_eq!(run().run(plan()).await, super::exit::PASSED);
    assert_eq!(
      *started.lock().expect("order"),
      lengths,
      "nothing known yet: plan order"
    );
    started.lock().expect("order").clear();
    assert_eq!(run().run(plan()).await, super::exit::PASSED);
    assert_eq!(*started.lock().expect("order"), [160, 110, 60, 20]);
  }

  /// A project given its own plan runs THAT plan, not the shared one —
  /// the whole point of the store, and a silent fallback to the shared
  /// plan would look like a passing run over the wrong corpus.
  #[test]
  fn a_project_with_its_own_plan_runs_it_instead_of_the_shared_one() {
    let runner = runner_with(vec!["smoke", "regression"]);
    let mut plans = FxHashMap::default();
    plans.insert("smoke".to_string(), plan_named("smoke-only"));
    runner.set_project_plans(plans);

    let shared = plan_named("shared");
    let runs = runner.project_runs();

    let smoke = runs.iter().find(|r| r.name == "smoke").expect("smoke run");
    assert_eq!(smoke.narrow(&shared).suites[0].name, "smoke-only");

    // And a project with no plan of its own still narrows the shared one.
    let regression = runs.iter().find(|r| r.name == "regression").expect("regression run");
    assert_eq!(regression.narrow(&shared).suites[0].name, "shared");
  }
}

#[cfg(test)]
mod project_filter_tests {
  use super::{absolute_path, select_projects, wildcard_matches};
  use crate::config::ProjectConfig;

  fn project(name: &str, default: Option<bool>) -> ProjectConfig {
    ProjectConfig {
      name: name.to_string(),
      default,
      ..Default::default()
    }
  }

  #[test]
  fn wildcards_match_the_whole_name() {
    assert!(wildcard_matches("slow*", "slow"));
    assert!(wildcard_matches("slow*", "slow-nightly"));
    assert!(wildcard_matches("*-nightly", "slow-nightly"));
    assert!(wildcard_matches("s*w*y", "slow-nightly"));
    assert!(!wildcard_matches("slow*", "very-slow"));
    assert!(!wildcard_matches("ab*b", "ab"));
    assert!(!wildcard_matches("*a*a", "a"));
  }

  #[test]
  fn no_filter_selects_default_projects_unless_asked_for_all() {
    let projects = [project("a", None), project("b", Some(false)), project("c", Some(true))];
    assert_eq!(select_projects(&projects, &[], false), Ok(vec![0, 2]));
    assert_eq!(select_projects(&projects, &[], true), Ok(vec![0, 1, 2]));
  }

  #[test]
  fn an_unknown_literal_name_is_an_error_even_beside_a_match() {
    let projects = [project("a", None), project("b", Some(false))];
    let names = ["B".to_string(), "zz".to_string()];
    assert_eq!(
      select_projects(&projects, &names, false),
      Err(r#"Project(s) "zz" not found. Available projects: "a", "b""#.to_string())
    );
  }

  /// A spec discovered outside the working directory reaches
  /// `filter_plan_for_project` as a `../..`-prefixed relative path.
  /// Making it absolute is not enough — the `..` components have to go,
  /// or the `testDir` prefix match fails and every project reports "no
  /// tests matched".
  #[test]
  fn an_out_of_tree_path_normalizes_before_it_is_compared() {
    let cwd = std::env::current_dir().expect("cwd");
    let resolved = absolute_path("../sibling-that-does-not-exist/specs/a.spec.ts");
    assert!(
      !resolved.components().any(|c| c.as_os_str() == ".."),
      "{} still carries `..`",
      resolved.display()
    );
    let expected = cwd
      .parent()
      .expect("cwd has a parent")
      .join("sibling-that-does-not-exist/specs/a.spec.ts");
    assert_eq!(resolved, expected);
    assert!(resolved.starts_with(absolute_path("../sibling-that-does-not-exist/specs")));
  }

  /// The other half: a bundler names a spec through its real path while
  /// `testDir` keeps the symlink the user wrote (`/var` -> `/private/var`
  /// on macOS). Both sides have to resolve the link or the prefix match
  /// fails on a path that exists.
  #[test]
  fn a_symlinked_test_dir_compares_equal_to_the_real_path() {
    let base = std::env::temp_dir().join(format!("ferri-abs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let real = base.join("real/specs");
    std::fs::create_dir_all(&real).expect("create dirs");
    let file = real.join("a.spec.ts");
    std::fs::write(&file, "").expect("write spec");

    let link = base.join("linked");
    #[cfg(unix)]
    std::os::unix::fs::symlink(base.join("real"), &link).expect("symlink");
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(base.join("real"), &link).expect("symlink");

    let via_link = absolute_path(&link.join("specs/a.spec.ts").to_string_lossy());
    let via_real = absolute_path(&file.to_string_lossy());
    assert_eq!(via_link, via_real);
    assert!(via_real.starts_with(absolute_path(&link.join("specs").to_string_lossy())));
    let _ = std::fs::remove_dir_all(&base);
  }
}
