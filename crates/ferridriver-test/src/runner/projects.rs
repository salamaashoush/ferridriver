//! Multi-project scheduling, dependency graph ordering, and project-scoped filtering.

use std::sync::Arc;
use std::time::Instant;

use rustc_hash::FxHashMap;

use super::{TestRunner, apply_run_edits, apply_status_override};
use crate::config::{CliOverrides, ProjectConfig, TestConfig};
use crate::exit;
use crate::model::TestPlan;
use crate::reporter::{EventBus, EventBusBuilder, ReporterDriver, ReporterEvent};

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
  pub(super) project: Option<ProjectConfig>,
  /// This project's own plan, when discovery itself differs per project
  /// (see [`TestRunner::set_project_plans`]). `None` narrows the shared
  /// plan instead.
  pub(super) plan: Option<Arc<TestPlan>>,
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

impl TestRunner {
  /// Run multiple projects in dependency order, reporting to the
  /// configured reporters.
  pub(super) async fn run_projects(&mut self, plan: TestPlan) -> i32 {
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
  pub(super) fn print_list(&self, plan: &TestPlan) -> i32 {
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
  pub(super) async fn execute_single_project(
    &self,
    plan: TestPlan,
    bus: EventBus,
    hooks: &ProjectHooks<'_>,
  ) -> ExecuteSummary {
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
pub(super) fn wildcard_matches(pattern: &str, text: &str) -> bool {
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
pub(super) fn project_prerequisites(
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
pub(super) fn topo_sort_projects(projects: &[ProjectConfig]) -> Result<Vec<usize>, ferridriver::FerriError> {
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
pub(super) fn shuffle_work(units: &mut [crate::dispatcher::WorkItem], seed: &str) {
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
pub(super) fn with_shuffle_seed(mut metadata: serde_json::Value, seed: Option<&str>) -> serde_json::Value {
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

pub(super) fn dependents_of(projects: &[ProjectConfig], scheduled: &[usize]) -> FxHashMap<usize, Vec<usize>> {
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
pub(super) fn without_file_filters(overrides: &CliOverrides) -> CliOverrides {
  let mut overrides = overrides.clone();
  overrides.test_files.clear();
  overrides.locations.clear();
  overrides
}

/// Applies project-level `test_dir`, `test_match`, `test_ignore`, grep, `grep_invert`, and tag filters.
pub(super) fn filter_plan_for_project(plan: &mut TestPlan, config: &TestConfig, project: &ProjectConfig) {
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
pub(super) fn absolute_path(path: &str) -> std::path::PathBuf {
  let path = std::path::Path::new(path);
  if let Ok(real) = std::fs::canonicalize(path) {
    return real;
  }
  let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
  ferridriver_config::layer::normalize_path(&absolute)
}
