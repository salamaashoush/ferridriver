//! Test runner orchestrator: overlaps browser launch with test dispatch,
//! handles retries with flaky detection.

use std::sync::Arc;
use std::time::Instant;

use rustc_hash::FxHashMap;
use tokio::sync::mpsc;

use crate::config::{CliOverrides, ProjectConfig, TestConfig};
use crate::dispatcher::Dispatcher;
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

/// One project of a run: the config it executes under, and the name its
/// test ids are hashed with. A config without `[[test.projects]]` is
/// itself one project.
#[derive(Clone)]
pub struct ProjectRun {
  pub name: String,
  pub config: Arc<TestConfig>,
  project: Option<ProjectConfig>,
}

impl ProjectRun {
  /// The `[[test.projects]]` entry this run came from, if any.
  #[must_use]
  pub fn project_config(&self) -> Option<&ProjectConfig> {
    self.project.as_ref()
  }

  /// `plan` narrowed to the tests this project runs.
  #[must_use]
  pub fn narrow(&self, plan: &TestPlan) -> TestPlan {
    let mut narrowed = plan.clone();
    if let Some(project) = &self.project {
      filter_plan_for_project(&mut narrowed, &self.config, project);
    }
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
  pub narrow: Option<&'a (dyn Fn(&str, &mut TestPlan) + Send + Sync)>,
}

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
  /// Worker numbers for the run in flight, shared with every project of
  /// it — see the reservation in [`Self::execute_with_summary`].
  worker_ids: Arc<std::sync::atomic::AtomicU32>,
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
    Some(crate::reporter::RunStatus::Passed) => 0,
    Some(_) => 1,
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
    // notify_one stores a permit when no waiter is registered yet, so a
    // request that lands before `wait` is first polled is not lost.
    self.notify.notify_one();
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
    while !self.requested.load(std::sync::atomic::Ordering::SeqCst) {
      self.notify.notified().await;
    }
  }
}

impl TestRunner {
  /// Build a runner with no programmatic suite hooks. For runners that need
  /// `before_all` / `after_all` closures, use [`TestRunner::with_hooks`].
  pub fn new(config: TestConfig, overrides: CliOverrides) -> Self {
    Self::with_hooks(config, TestHooks::default(), overrides)
  }

  /// Build a runner with programmatic suite hooks supplied at construction.
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
      worker_ids: Arc::new(std::sync::atomic::AtomicU32::new(0)),
    }
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
    let runs: Vec<ProjectRun> = self
      .project_runs()
      .into_iter()
      .filter(|run| self.overrides.project_filter.is_empty() || self.overrides.project_filter.contains(&run.name))
      .collect();
    let narrowed: Vec<TestPlan> = runs
      .iter()
      .map(|run| {
        let mut narrowed = run.narrow(plan);
        self.apply_run_filters(&mut narrowed);
        narrowed
      })
      .collect();
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
  pub fn stop_handle(&self) -> RunStop {
    self.run_stop.clone()
  }

  /// Append an additional reporter after construction (e.g., NAPI ResultCollector).
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
      worker_ids: Arc::clone(&self.worker_ids),
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

  /// Export the configured `baseUrl` as `FERRIDRIVER_BASE_URL` so
  /// URL-resolving consumers outside the config path (BDD step
  /// definitions) see it. When no `baseUrl` is configured, the
  /// webServer startup path exports the first server's URL instead.
  fn export_base_url_env(&self) {
    if let Some(url) = &self.config.base_url {
      // SAFETY: called from the single-threaded run entry points before
      // any worker threads spawn.
      #[allow(unsafe_code)]
      unsafe {
        std::env::set_var("FERRIDRIVER_BASE_URL", url);
      }
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
    // Playwright's `config.expect.timeout`: make the configured default
    // visible to every bare `expect()` in this process. A project with
    // its own `expect` block narrows it again per test, in the worker.
    ferridriver_expect::set_default_expect_timeout(std::time::Duration::from_millis(
      self.config.resolved_expect(None).timeout_ms(),
    ));
    self.export_base_url_env();
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
      match self.preprocess_corpus(&plan).await {
        Ok(edits) => {
          let project = self.config.name.clone().unwrap_or_default();
          apply_run_edits(&mut plan, &project, &edits);
        },
        Err(message) => {
          eprintln!("Error: reporter preprocess failed: {message}");
          return 1;
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
        1
      }
    } else {
      inner.await
    }
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
    let summary = self
      .execute_projects_with_summary(
        plan,
        bus.clone(),
        ProjectHooks {
          narrow: Some(&narrow),
          ..ProjectHooks::default()
        },
      )
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

    let sorted = match topo_sort_projects(&projects) {
      Ok(order) => order,
      Err(e) => {
        tracing::error!(target: "ferridriver::runner", "project dependency error: {e}");
        return ExecuteSummary {
          exit_code: 1,
          ..Default::default()
        };
      },
    };

    // Resolve `--project NAME` filter into the index set the runner
    // will execute. When non-empty, also pull in transitive deps
    // (unless `--no-deps`) and any teardown projects referenced by
    // the kept set.
    let allowed_indices: rustc_hash::FxHashSet<usize> = if self.overrides.project_filter.is_empty() {
      (0..projects.len()).collect()
    } else {
      let mut wanted: rustc_hash::FxHashSet<usize> = rustc_hash::FxHashSet::default();
      for name in &self.overrides.project_filter {
        if let Some(idx) = projects.iter().position(|p| &p.name == name) {
          wanted.insert(idx);
        } else {
          tracing::warn!(target: "ferridriver::runner", "--project {name}: no matching project");
        }
      }
      // Walk dependencies until fixpoint (unless --no-deps).
      if !self.overrides.no_deps {
        let mut frontier: Vec<usize> = wanted.iter().copied().collect();
        while let Some(idx) = frontier.pop() {
          for dep_name in &projects[idx].dependencies {
            if let Some(dep_idx) = projects.iter().position(|p| &p.name == dep_name)
              && wanted.insert(dep_idx)
            {
              frontier.push(dep_idx);
            }
          }
        }
      }
      // Always pull in declared teardowns of kept projects.
      let kept: Vec<usize> = wanted.iter().copied().collect();
      for idx in kept {
        if let Some(t) = &projects[idx].teardown
          && let Some(t_idx) = projects.iter().position(|p| &p.name == t)
        {
          wanted.insert(t_idx);
        }
      }
      wanted
    };
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
    let mut scheduled: Vec<usize> = sorted.clone();
    if let Some(td_idx) = cli_teardown_idx
      && !scheduled.contains(&td_idx)
    {
      scheduled.push(td_idx);
    }

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
    let prereqs: FxHashMap<usize, Vec<(usize, bool)>> = scheduled
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
          for &other in &scheduled {
            if other != idx {
              reqs.push((other, false));
            }
          }
        }
        (idx, reqs)
      })
      .collect();

    // ── Hoist web servers out of per-project execute ──
    // `merge_project` copies the top-level `web_server` list onto every
    // project; starting/stopping the same servers per project would bind the
    // same ports concurrently. Start them once here and clear the per-project
    // copies so each project's `execute()` skips its web-server lifecycle.
    let web_server_manager = if self.config.web_server.is_empty() {
      None
    } else {
      match crate::server::WebServerManager::start(&self.config.web_server).await {
        Ok(mgr) => {
          if let Some(url) = mgr.first_url()
            && self.config.base_url.is_none()
          {
            // SAFETY: set once here before any worker threads spawn.
            #[allow(unsafe_code)]
            unsafe {
              std::env::set_var("FERRIDRIVER_BASE_URL", &url)
            };
            tracing::info!(target: "ferridriver::runner", "webServer base_url={url}");
          }
          Some(mgr)
        },
        Err(e) => {
          tracing::error!(target: "ferridriver::runner", "webServer start failed: {e}");
          bus.emit(ReporterEvent::RunError {
            error: Box::new(crate::model::TestFailure::from(format!("webServer start failed: {e}"))),
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
          return ExecuteSummary {
            exit_code: 1,
            ..Default::default()
          };
        },
      }
    };

    // Build each project's merged config + filtered plan up front so we can
    // both report an accurate aggregate total and reuse them when spawning.
    let mut merged: FxHashMap<usize, Arc<TestConfig>> = FxHashMap::default();
    let mut plans: FxHashMap<usize, TestPlan> = FxHashMap::default();
    let mut total_tests = 0usize;
    for &idx in &scheduled {
      let mut mc = self.config.merge_project(&projects[idx]);
      mc.web_server = Vec::new();
      let mut p = plan.clone();
      filter_plan_for_project(&mut p, &mc, &projects[idx]);
      self.apply_run_filters(&mut p);
      if let Some(narrow) = hooks.narrow {
        narrow(&projects[idx].name, &mut p);
      }
      total_tests += p.total_tests;
      merged.insert(idx, Arc::new(mc));
      plans.insert(idx, p);
    }

    // ── Single aggregate run boundary ──
    let reporting_enabled = bus.has_subscribers();

    // `workers` is the global concurrency budget; never launch more workers
    // than tests across all projects in flight.
    let num_workers = (self.config.workers as usize).min(total_tests.max(1)).max(1) as u32;
    if reporting_enabled {
      let project_plans: Vec<crate::reporter::api::ProjectPlan<'_>> = scheduled
        .iter()
        .filter_map(|idx| {
          Some(crate::reporter::api::ProjectPlan {
            name: projects[*idx].name.as_str(),
            config: merged.get(idx)?.as_ref(),
            project: Some(&projects[*idx]),
            plan: plans.get(idx)?,
          })
        })
        .collect();
      bus.emit(ReporterEvent::RunStarted {
        total_tests,
        num_workers,
        metadata: self.config.metadata.clone(),
        start_time: std::time::SystemTime::now(),
        preamble: Arc::new(crate::reporter::api::RunPreamble::build(&self.config, &project_plans)),
      });
    }
    let run_start = Instant::now();

    // ── Ready-set scheduler ──
    // `max_parallel_projects == 0` means unbounded (cap at the number of
    // scheduled projects). Spawn every dependency-ready project up to the cap,
    // drive completions via a JoinSet, and re-evaluate readiness on each
    // completion. Dependency ordering, teardown ordering, and dep-failure
    // skipping are all preserved by the prerequisite model above.
    let cap = if self.config.max_parallel_projects == 0 {
      scheduled.len().max(1)
    } else {
      self.config.max_parallel_projects as usize
    };

    let mut passed_projects: rustc_hash::FxHashSet<usize> = rustc_hash::FxHashSet::default();
    let mut terminal: rustc_hash::FxHashSet<usize> = rustc_hash::FxHashSet::default();
    let mut remaining: Vec<usize> = scheduled.clone();
    let mut join_set: tokio::task::JoinSet<(usize, Option<ExecuteSummary>)> = tokio::task::JoinSet::new();
    let mut in_flight = 0usize;

    let mut exit_code = 0i32;
    let mut agg = ExecuteSummary::default();

    loop {
      // Launch every ready project up to the parallelism cap. Skips (no tests
      // or dependency failed) resolve immediately and may unblock others, so
      // keep scanning until no further progress is possible this round.
      while in_flight < cap {
        // Find a not-yet-started project whose prerequisites are all terminal.
        let next = remaining.iter().copied().find(|&idx| {
          prereqs
            .get(&idx)
            .map(|rs| rs.iter().all(|(dep, _)| terminal.contains(dep)))
            .unwrap_or(true)
        });
        let Some(idx) = next else { break };
        remaining.retain(|&i| i != idx);

        // Skip a normal project whose passing-required prerequisites did not
        // pass (dependency failure). Teardowns are never skipped this way.
        let blocked = prereqs
          .get(&idx)
          .map(|rs| {
            rs.iter()
              .any(|&(dep, must_pass)| must_pass && !passed_projects.contains(&dep))
          })
          .unwrap_or(false);
        if blocked {
          tracing::warn!(
            target: "ferridriver::runner",
            project = projects[idx].name,
            "skipping — dependency failed",
          );
          terminal.insert(idx);
          exit_code = 1;
          continue;
        }

        let Some(project_plan) = plans.remove(&idx) else {
          terminal.insert(idx);
          passed_projects.insert(idx);
          continue;
        };
        if project_plan.total_tests == 0 {
          tracing::debug!(
            target: "ferridriver::runner",
            project = projects[idx].name,
            "no tests matched, skipping",
          );
          terminal.insert(idx);
          passed_projects.insert(idx);
          continue;
        }

        tracing::info!(
          target: "ferridriver::runner",
          project = projects[idx].name,
          tests = project_plan.total_tests,
          "running project",
        );

        let mut sub_runner = self.with_run_options(
          merged.get(&idx).cloned().unwrap_or_else(|| Arc::clone(&self.config)),
          self.overrides.clone(),
        );
        sub_runner.suppress_run_boundary = true;
        let (project_bus, drains) = match hooks.stream {
          Some(stream) => {
            let stream = stream(&projects[idx].name);
            (stream.bus, stream.drains)
          },
          None => (bus.clone(), Vec::new()),
        };
        let owns_bus = hooks.stream.is_some();
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
        match joined {
          Ok((idx, Some(summary))) => {
            terminal.insert(idx);
            if summary.exit_code == 0 {
              passed_projects.insert(idx);
            } else {
              exit_code = 1;
            }
            agg.passed += summary.passed;
            agg.failed += summary.failed;
            agg.skipped += summary.skipped;
            agg.flaky += summary.flaky;
          },
          Ok((idx, None)) => {
            terminal.insert(idx);
            exit_code = 1;
          },
          Err(e) => {
            tracing::error!(target: "ferridriver::runner", "project task panicked: {e}");
            exit_code = 1;
          },
        }
      }
    }

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

    agg.exit_code = exit_code;
    agg.total = total_tests;
    agg
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
  /// (shard, grep, tag). Applied by every `execute`, and by the
  /// multi-project orchestrator when it counts the run — a total that
  /// ignored `--grep` reported more tests than ever ran.
  fn apply_run_filters(&self, plan: &mut TestPlan) {
    if let Some(shard_arg) = &self.overrides.shard {
      shard::filter_by_shard(
        plan,
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
      crate::discovery::filter_by_grep(plan, grep, false);
    }
    if let Some(grep_inv) = grep_inv {
      crate::discovery::filter_by_grep(plan, grep_inv, true);
    }
    if let Some(tag) = &self.overrides.tag {
      crate::discovery::filter_by_tag(plan, tag);
    }
  }

  /// Core execution engine, returning the full per-run tally. `execute()` is
  /// the thin `i32` wrapper; the multi-project orchestrator uses the summary
  /// to aggregate counts across concurrently-run projects.
  #[tracing::instrument(skip_all, fields(workers = self.config.workers, tests = plan.total_tests))]
  pub async fn execute_with_summary(&self, mut plan: TestPlan, event_bus: EventBus) -> ExecuteSummary {
    self.apply_run_filters(&mut plan);

    // ── Forbid-only check ──
    if (self.config.forbid_only || self.overrides.forbid_only)
      && let Err(e) = crate::discovery::check_forbid_only(&plan)
    {
      eprint!("{e}");
      return ExecuteSummary {
        exit_code: 1,
        ..Default::default()
      };
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

    // Worker scratch directories are per-run: whatever is left is from a
    // run that was killed before it could zip its traces. Only the
    // outermost runner may do this — projects run concurrently and share
    // one output directory, so a project sweeping here would delete the
    // trace another project is still writing.
    if !self.suppress_run_boundary {
      crate::artifacts::sweep(&self.config.output_dir);
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

    if self.overrides.list_only {
      for suite in &plan.suites {
        for test in &suite.tests {
          println!("  {}", test.id.full_name());
        }
      }
      println!("\n  {total_tests} test(s) found");
      return ExecuteSummary {
        total: total_tests,
        ..Default::default()
      };
    }

    // Never launch more workers than tests — extra workers launch browsers for nothing.
    let num_workers = (self.config.workers as usize).min(total_tests).max(1) as u32;

    // Custom `#[fixture]` definitions, collected once and seeded into every
    // worker's fixture pool so tests can resolve them via `ctx.get`.
    let custom_fixtures = crate::discovery::collect_rust_fixtures();

    // ── Validate fixture DAG ──
    {
      let mut fixture_defs = builtin_fixtures(&self.config.browser);
      for (name, def) in &custom_fixtures {
        fixture_defs.insert(name.clone(), def.clone());
      }
      if let Err(e) = validate_dag(&fixture_defs) {
        tracing::error!(target: "ferridriver::fixture", "fixture DAG error: {e}");
        return ExecuteSummary {
          exit_code: 1,
          total: total_tests,
          failed: total_tests,
          ..Default::default()
        };
      }
    }

    // ── Web server lifecycle ──
    // Follows Playwright's pattern: start servers, set FERRIDRIVER_BASE_URL env var.
    let web_server_manager = if !self.config.web_server.is_empty() {
      match crate::server::WebServerManager::start(&self.config.web_server).await {
        Ok(mgr) => {
          if let Some(url) = mgr.first_url()
            && self.config.base_url.is_none()
          {
            // SAFETY: set_var is called before worker threads are spawned,
            // so no concurrent reads can race.
            #[allow(unsafe_code)]
            unsafe {
              std::env::set_var("FERRIDRIVER_BASE_URL", &url)
            };
            tracing::info!(target: "ferridriver::runner", "webServer base_url={url}");
          }
          Some(mgr)
        },
        Err(e) => {
          tracing::error!(target: "ferridriver::runner", "webServer start failed: {e}");
          // Before the run boundary: only the error itself is known,
          // and a report with no tests and this error beats a silent one.
          event_bus.emit(ReporterEvent::RunError {
            error: Box::new(crate::model::TestFailure::from(format!("webServer start failed: {e}"))),
          });
          return ExecuteSummary {
            exit_code: 1,
            total: total_tests,
            failed: total_tests,
            ..Default::default()
          };
        },
      }
    } else {
      None
    };

    // Compose `metadata` with optional git info per `captureGitInfo`.
    // Cloned once here so each downstream emit sees the same JSON.
    let mut run_metadata = self.config.metadata.clone();
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
        metadata: run_metadata,
        start_time: std::time::SystemTime::now(),
        preamble: Arc::new(crate::reporter::api::RunPreamble::build(&self.config, &project_plans)),
      });
    }

    let start = Instant::now();

    // ── Global setup ──
    if !self.hooks.global_setup_fns.is_empty() {
      let global_pool = FixturePool::new(FxHashMap::default(), FixtureScope::Global);
      for setup_fn in &self.hooks.global_setup_fns {
        if let Err(e) = setup_fn(global_pool.clone()).await {
          tracing::error!(target: "ferridriver::runner", "global setup failed: {e}");
          if emit_boundary {
            // A setup failure belongs to no test; without this channel
            // every report would show a run of zero failures.
            event_bus.emit(ReporterEvent::RunError {
              error: Box::new(crate::model::TestFailure {
                message: format!("global setup failed: {e}"),
                stack: e.stack.clone(),
                diff: None,
                screenshot: None,
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
          return ExecuteSummary {
            exit_code: 1,
            total: total_tests,
            failed: total_tests,
            ..Default::default()
          };
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
                  expected_status: test.expected_status,
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
                  expected_status: test.expected_status,
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

    // ── Spawn workers with lazy browser launch ──
    // Each worker holds a `BrowserHandle` that launches the browser on first
    // fixture access. Tests that never resolve `browser`/`context`/`page`
    // (config-only tests, request-only tests) skip the launch entirely —
    // critical in CI where Chromium's first-launch can exceed 30s.
    let (result_tx, mut result_rx) = mpsc::channel::<WorkerTestResult>(256);

    let mut worker_handles = Vec::new();
    let launch_plan = match build_launch_plan(&self.config.browser) {
      Ok(plan) => plan,
      Err(message) => {
        eprintln!("Error: {message}");
        return ExecuteSummary {
          exit_code: 1,
          ..ExecuteSummary::default()
        };
      },
    };
    let worker_event_bus = reporting_enabled.then(|| event_bus.clone());

    // A session's browser is only this run's browser when this run would
    // have launched the same one: projects differ in backend, channel and
    // headedness, and borrowing the session's Chromium for a WebKit
    // project would run the tests on the wrong engine.
    let shared_browser = self
      .shared_browser
      .as_ref()
      .filter(|shared| same_launch(&shared.plan, &launch_plan))
      .map(|shared| Arc::clone(&shared.browser));
    if shared_browser.is_none() && self.shared_browser.is_some() {
      tracing::debug!(
        target: "ferridriver::runner",
        project = self.config.name.as_deref().unwrap_or_default(),
        "session browser does not match this run's launch plan; launching its own",
      );
    }

    // Worker numbers are handed out per RUN, not per runner: projects
    // execute concurrently, and two workers numbered 0 would share a
    // scratch directory, a `.playwright-artifacts-0` the UI reads, and —
    // via the per-worker script session — the "test currently running"
    // that `test.step()` and `test.info()` resolve against.
    let worker_base = self
      .worker_ids
      .fetch_add(num_workers, std::sync::atomic::Ordering::SeqCst);

    for slot in 0..num_workers {
      let worker = Worker::new(
        worker_base + slot,
        slot,
        Arc::clone(&self.config),
        worker_event_bus.clone(),
        self.live_traces,
      );
      let rx = dispatcher.receiver();
      let tx = result_tx.clone();
      let custom_pool = FixturePool::new(custom_fixtures.clone(), FixtureScope::Worker);
      let shared = shared_browser.clone();
      let plan = launch_plan.clone();
      let stop_flag = dispatcher.stop_flag();

      let handle = tokio::spawn(async move {
        let browser_handle = if let Some(b) = shared {
          Arc::new(BrowserHandle::from_shared(b))
        } else {
          Arc::new(BrowserHandle::new(plan))
        };
        Box::pin(worker.run(browser_handle, custom_pool, rx, tx, stop_flag)).await;
      });
      worker_handles.push(handle);
    }
    drop(result_tx);

    // ── Collect results with retry re-dispatch ──
    // Statuses plus what the test was declared to end in: a `test.fail()`
    // test that fails is a pass, and only the pair says so.
    let mut attempt_history: FxHashMap<String, (Vec<TestStatus>, crate::model::ExpectedStatus)> = FxHashMap::default();
    let mut final_count = 0usize;
    let mut failure_count = 0usize;
    let max_failures = if self.config.fail_fast {
      1 // fail_fast = stop after first failure
    } else {
      self.config.max_failures as usize // 0 = unlimited
    };

    let mut stop_requested = false;
    loop {
      let result = tokio::select! {
        result = result_rx.recv() => result,
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
      let test_key = result.outcome.test_id.full_name();
      let entry = attempt_history
        .entry(test_key)
        .or_insert_with(|| (Vec::new(), result.outcome.expected_status));
      entry.0.push(result.outcome.status);
      entry.1 = result.outcome.expected_status;

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

      if final_count >= total_executions {
        dispatcher.close();
      }
    }

    for handle in worker_handles {
      let _ = handle.await;
    }

    // ── Global teardown (always runs, even if tests failed) ──
    if !self.hooks.global_teardown_fns.is_empty() {
      let global_pool = FixturePool::new(FxHashMap::default(), FixtureScope::Global);
      for teardown_fn in &self.hooks.global_teardown_fns {
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

    for (attempts, expected) in attempt_history.values() {
      match crate::model::outcome_kind(attempts, *expected) {
        crate::model::TestOutcomeKind::Expected => passed += 1,
        crate::model::TestOutcomeKind::Flaky => {
          flaky += 1;
          passed += 1;
        },
        crate::model::TestOutcomeKind::Skipped => skipped += 1,
        crate::model::TestOutcomeKind::Unexpected => failed += 1,
      }
    }

    // ── preserve_output: "failures-only" — delete output dirs for passing tests ──
    if self.config.preserve_output == "failures-only" {
      for (test_key, (attempts, expected)) in &attempt_history {
        if crate::model::outcome_kind(attempts, *expected) != crate::model::TestOutcomeKind::Unexpected {
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
        passed,
        failed,
        skipped,
        flaky,
        duration,
        status: if failed > 0 {
          crate::reporter::RunStatus::Failed
        } else if self.run_stop.is_requested() {
          crate::reporter::RunStatus::Interrupted
        } else {
          crate::reporter::RunStatus::Passed
        },
      });
    }

    let exit_code = if failed > 0 || (self.config.fail_on_flaky_tests && flaky > 0) {
      1
    } else {
      0
    };
    if exit_code != 0 && failed == 0 && flaky > 0 && self.config.fail_on_flaky_tests {
      tracing::warn!(
        target: "ferridriver::runner",
        flaky,
        "fail_on_flaky_tests: flagging exit 1 for {flaky} flaky test(s)",
      );
    }
    ExecuteSummary {
      exit_code,
      total: total_tests,
      passed,
      failed,
      skipped,
      flaky,
    }
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

    self.export_base_url_env();

    // Launch browser once — reuse across all watch cycles.
    let launch_plan = match build_launch_plan(&self.config.browser) {
      Ok(plan) => plan,
      Err(message) => {
        eprintln!("Error: {message}");
        return 1;
      },
    };
    let browser = match launch_with_plan(launch_plan.clone()).await {
      Ok(b) => Arc::new(b),
      Err(e) => {
        eprintln!("Failed to launch browser: {e}");
        return 1;
      },
    };
    self.shared_browser = Some(SharedBrowser {
      browser: Arc::clone(&browser),
      plan: launch_plan,
    });

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
    let _ = browser.close().await;

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

    self.export_base_url_env();

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
    println!("\n  ferridriver UI mode\n\n  http://{addr}\n");

    // Launch browser once — reuse across all UI-triggered runs.
    let launch_plan = match build_launch_plan(&self.config.browser) {
      Ok(plan) => plan,
      Err(message) => {
        eprintln!("Error: {message}");
        return 1;
      },
    };
    let browser = match launch_with_plan(launch_plan.clone()).await {
      Ok(b) => Arc::new(b),
      Err(e) => {
        eprintln!("Failed to launch browser: {e}");
        return 1;
      },
    };
    self.shared_browser = Some(SharedBrowser {
      browser: Arc::clone(&browser),
      plan: launch_plan,
    });

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
    let _ = browser.close().await;

    0
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

    self.export_base_url_env();

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
    let launch_plan = match build_launch_plan(&self.config.browser) {
      Ok(plan) => plan,
      Err(message) => {
        eprintln!("Error: {message}");
        return 1;
      },
    };
    let browser = match launch_with_plan(launch_plan.clone()).await {
      Ok(browser) => Arc::new(browser),
      Err(e) => {
        eprintln!("Failed to launch browser: {e}");
        return 1;
      },
    };
    self.shared_browser = Some(SharedBrowser {
      browser: Arc::clone(&browser),
      plan: launch_plan,
    });

    println!("\n  ferridriver UI mode\n\n  {}\n", server.url);
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
    let _ = browser.close().await;
    0
  }

  /// Build the (filtered) plan a UI command asks for. Publishes the
  /// refreshed full test list as a side effect. Returns `None` when
  /// nothing matches or the command needs no run (idle `Stop`).
  async fn plan_for_ui_command(
    &mut self,
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
          .map(|contents| contents.lines().any(|line| !line.trim().is_empty()))
          .unwrap_or(false);
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
/// Applies project-level test_match, test_dir, grep, grep_invert, and tag filters.
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
  let (backend, kind) = browser_config.resolve_kinds();

  let mut args = browser_config.args.clone();
  // Proxy launch args.
  if let Some(ref proxy) = browser_config.use_options.proxy {
    args.push(format!("--proxy-server={}", proxy.server));
    if let Some(ref bypass) = proxy.bypass {
      args.push(format!("--proxy-bypass-list={bypass}"));
    }
  }
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

  // Instance overrides, when the config (or the project) selected a
  // named instance. This is what lets a suite run against the same
  // environment an MCP session drives, instead of hard-coding that
  // environment's flags into `args`.
  let overrides = browser_config.instance_overrides()?;
  args.extend(overrides.args);

  Ok(LaunchPlan {
    backend: overrides.backend.unwrap_or(backend),
    kind,
    headless: overrides.headless.unwrap_or(headless),
    executable_path: overrides
      .executable_path
      .or_else(|| browser_config.executable_path.clone()),
    user_data_dir: overrides.user_data_dir,
    env: (!overrides.env.is_empty()).then_some(overrides.env),
    ignore_default_args: overrides.ignore_default_args,
    args,
    default_viewport: browser_config
      .viewport
      .as_ref()
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
pub(crate) async fn launch_with_plan(plan: LaunchPlan) -> ferridriver::error::Result<Browser> {
  let mut state = BrowserState::with_plan(ConnectMode::Launch, plan);
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
  cell: tokio::sync::OnceCell<Arc<Browser>>,
  shared: bool,
}

impl BrowserHandle {
  pub fn new(plan: LaunchPlan) -> Self {
    Self {
      plan,
      cell: tokio::sync::OnceCell::new(),
      shared: false,
    }
  }

  /// Wrap a pre-launched browser (watch-mode shared) — `close()` is a
  /// no-op so the shared browser survives across runs.
  pub fn from_shared(browser: Arc<Browser>) -> Self {
    let cell = tokio::sync::OnceCell::new();
    let _ = cell.set(browser);
    Self {
      plan: LaunchPlan::default(),
      cell,
      shared: true,
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

  pub async fn get(&self) -> ferridriver::error::Result<Arc<Browser>> {
    let plan = self.plan.clone();
    self
      .cell
      .get_or_try_init(|| async move { launch_with_plan(plan).await.map(Arc::new) })
      .await
      .cloned()
  }

  pub fn try_get(&self) -> Option<Arc<Browser>> {
    self.cell.get().cloned()
  }

  pub async fn close(&self) {
    if self.shared {
      return;
    }
    if let Some(b) = self.cell.get() {
      let _ = b.close().await;
    }
  }
}

#[cfg(test)]
mod project_filter_tests {
  use super::absolute_path;

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

    let via_link = absolute_path(&link.join("specs/a.spec.ts").to_string_lossy());
    let via_real = absolute_path(&file.to_string_lossy());
    assert_eq!(via_link, via_real);
    assert!(via_real.starts_with(absolute_path(&link.join("specs").to_string_lossy())));
    let _ = std::fs::remove_dir_all(&base);
  }
}
