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
use ferridriver::backend::BackendKind;
use ferridriver::options::{BrowserKind, LaunchPlan};
use ferridriver::state::{BrowserState, ConnectMode};

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
  /// Shared browser for watch mode (persists across runs).
  shared_browser: Option<Arc<Browser>>,
  /// When set, `execute()` does not emit `RunStarted` / `RunFinished`. The
  /// multi-project orchestrator turns this on for every per-project run so a
  /// single aggregate run boundary is emitted once around all projects,
  /// rather than one pair per project (which would reset terminal counters
  /// and finalize reporters mid-run).
  suppress_run_boundary: bool,
}

impl TestRunner {
  /// Build a runner with no programmatic suite hooks. For runners that need
  /// `before_all` / `after_all` closures, use [`TestRunner::with_hooks`].
  pub fn new(config: TestConfig, overrides: CliOverrides) -> Self {
    Self::with_hooks(config, TestHooks::default(), overrides)
  }

  /// Build a runner with programmatic suite hooks supplied at construction.
  pub fn with_hooks(config: TestConfig, hooks: TestHooks, overrides: CliOverrides) -> Self {
    let reporters = crate::reporter::create_reporters(
      &config.reporter,
      &config.output_dir,
      config.has_bdd,
      config.quiet,
      config.report_slow_tests.clone(),
    );
    Self {
      config: Arc::new(config),
      hooks,
      reporters,
      overrides,
      shared_browser: None,
      suppress_run_boundary: false,
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
    // Playwright's `config.expect.timeout`: make the configured default
    // visible to every bare `expect()` in this process.
    ferridriver_expect::set_default_expect_timeout(std::time::Duration::from_millis(self.config.expect_timeout));
    let global_timeout = self.config.global_timeout;
    let inner = async move {
      // ── Multi-project path ──
      if !self.config.projects.is_empty() {
        return Box::pin(self.run_projects(plan)).await;
      }

      // ── Single-project path ──
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

      if let Some(driver_handle) = driver_handle {
        if let Ok(reporters) = driver_handle.await {
          self.reporters = reporters;
        }
      }

      exit_code
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
            if let Some(dep_idx) = projects.iter().position(|p| &p.name == dep_name) {
              if wanted.insert(dep_idx) {
                frontier.push(dep_idx);
              }
            }
          }
        }
      }
      // Always pull in declared teardowns of kept projects.
      let kept: Vec<usize> = wanted.iter().copied().collect();
      for idx in kept {
        if let Some(t) = &projects[idx].teardown {
          if let Some(t_idx) = projects.iter().position(|p| &p.name == t) {
            wanted.insert(t_idx);
          }
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
    if let Some(td_idx) = cli_teardown_idx {
      if !scheduled.contains(&td_idx) {
        scheduled.push(td_idx);
      }
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
          if let Some(dep_idx) = projects.iter().position(|p| &p.name == dep_name) {
            if scheduled.contains(&dep_idx) {
              reqs.push((dep_idx, true));
            }
          }
        }
        // Teardown parent must merely be terminal.
        if let Some(&parent_idx) = teardown_parent.get(&idx) {
          if scheduled.contains(&parent_idx) {
            reqs.push((parent_idx, false));
          }
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
          if let Some(url) = mgr.first_url() {
            if self.config.base_url.is_none() {
              // SAFETY: set once here before any worker threads spawn.
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
      total_tests += p.total_tests;
      merged.insert(idx, Arc::new(mc));
      plans.insert(idx, p);
    }

    // ── Shared reporter driver + single aggregate run boundary ──
    let mut builder = EventBusBuilder::new();
    let driver_handle = if self.reporters.is_empty() {
      None
    } else {
      let sub = builder.subscribe();
      let reporters = std::mem::take(&mut self.reporters);
      Some(tokio::spawn(ReporterDriver::new(reporters, sub).run()))
    };
    let bus = builder.build();
    let reporting_enabled = bus.has_subscribers();

    // `workers` is the global concurrency budget; never launch more workers
    // than tests across all projects in flight.
    let num_workers = (self.config.workers as usize).min(total_tests.max(1)).max(1) as u32;
    if reporting_enabled {
      bus.emit(ReporterEvent::RunStarted {
        total_tests,
        num_workers,
        metadata: self.config.metadata.clone(),
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

        let sub_runner = TestRunner {
          config: merged.get(&idx).cloned().unwrap_or_else(|| Arc::clone(&self.config)),
          hooks: self.hooks.clone(),
          reporters: ReporterSet::default(),
          overrides: self.overrides.clone(),
          shared_browser: self.shared_browser.clone(),
          suppress_run_boundary: true,
        };
        let project_bus = bus.clone();
        join_set.spawn(async move {
          let summary = sub_runner.execute_with_summary(project_plan, project_bus).await;
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

    // ── Single aggregate RunFinished + reporter teardown ──
    if reporting_enabled {
      bus.emit(ReporterEvent::RunFinished {
        total: total_tests,
        passed: agg.passed,
        failed: agg.failed,
        skipped: agg.skipped,
        flaky: agg.flaky,
        duration: run_start.elapsed(),
      });
    }
    bus.close();
    if let Some(driver_handle) = driver_handle {
      if let Ok(reporters) = driver_handle.await {
        self.reporters = reporters;
      }
    }

    if let Some(mgr) = web_server_manager {
      mgr.stop().await;
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
  pub async fn execute(&self, plan: TestPlan, event_bus: EventBus) -> i32 {
    self.execute_with_summary(plan, event_bus).await.exit_code
  }

  /// Core execution engine, returning the full per-run tally. `execute()` is
  /// the thin `i32` wrapper; the multi-project orchestrator uses the summary
  /// to aggregate counts across concurrently-run projects.
  #[tracing::instrument(skip_all, fields(workers = self.config.workers, tests = plan.total_tests))]
  pub async fn execute_with_summary(&self, mut plan: TestPlan, event_bus: EventBus) -> ExecuteSummary {
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
        return ExecuteSummary {
          exit_code: 1,
          ..Default::default()
        };
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
      event_bus.emit(ReporterEvent::RunStarted {
        total_tests,
        num_workers,
        metadata: run_metadata,
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
            event_bus.emit(ReporterEvent::RunFinished {
              total: total_tests,
              passed: 0,
              failed: total_tests,
              skipped: 0,
              flaky: 0,
              duration: start.elapsed(),
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

    // ── Spawn workers with lazy browser launch ──
    // Each worker holds a `BrowserHandle` that launches the browser on first
    // fixture access. Tests that never resolve `browser`/`context`/`page`
    // (config-only tests, request-only tests) skip the launch entirely —
    // critical in CI where Chromium's first-launch can exceed 30s.
    let (result_tx, mut result_rx) = mpsc::channel::<WorkerTestResult>(256);

    let mut worker_handles = Vec::new();
    let launch_plan = build_launch_plan(&self.config.browser);
    let worker_event_bus = reporting_enabled.then(|| event_bus.clone());

    for worker_id in 0..num_workers {
      let worker = Worker::new(worker_id, Arc::clone(&self.config), worker_event_bus.clone());
      let rx = dispatcher.receiver();
      let tx = result_tx.clone();
      let custom_pool = FixturePool::new(custom_fixtures.clone(), FixtureScope::Worker);
      let shared = self.shared_browser.clone();
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

    if emit_boundary {
      event_bus.emit(ReporterEvent::RunFinished {
        total: total_tests,
        passed,
        failed,
        skipped,
        flaky,
        duration,
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

    // Launch browser once — reuse across all watch cycles.
    let launch_plan = build_launch_plan(&self.config.browser);
    let browser = match launch_with_plan(launch_plan).await {
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
    let plan = plan_factory(None).await;
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
              if self.run_with_tui_drain(plan_factory(None).await, tui).await { break; }
              tui.set_status(crate::tui::WatchStatus::Idle);
            }
            WatchCommand::RunFailed => {
              let mut plan = plan_factory(None).await;
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
              let mut plan = plan_factory(None).await;
              if let Some(ref pattern) = grep_filter {
                crate::discovery::filter_by_grep(&mut plan, pattern, false);
              }
              if self.run_with_tui_drain(plan, tui).await { break; }
              tui.set_status(crate::tui::WatchStatus::Idle);
            }
            WatchCommand::FilterByName(pattern) => {
              if !pattern.is_empty() {
                grep_filter = Some(pattern.clone());
                let mut plan = plan_factory(None).await;
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
    let plan = plan_factory(None).await;
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
    let launch_plan = build_launch_plan(&self.config.browser);
    let browser = match launch_with_plan(launch_plan).await {
      Ok(b) => Arc::new(b),
      Err(e) => {
        eprintln!("Failed to launch browser: {e}");
        return 1;
      },
    };
    self.shared_browser = Some(Arc::clone(&browser));

    let watcher = match FileWatcher::new(&watch_root, &self.config.test_match, &self.config.test_ignore) {
      Ok(w) => w,
      Err(e) => {
        eprintln!("Failed to start file watcher: {e}");
        return 1;
      },
    };

    // Initial plan populates the sidebar; nothing runs until requested.
    let plan = plan_factory(None).await;
    state.publish_test_list(&plan);

    // Commands that arrive mid-run are buffered here and processed in
    // order once the current run finishes (Stop is consumed by the run
    // itself and cancels it).
    let mut queued: std::collections::VecDeque<UiCommand> = std::collections::VecDeque::new();

    loop {
      if let Some(cmd) = queued.pop_front() {
        if let Some(plan) = self.plan_for_ui_command(&plan_factory, cmd, &state).await {
          let pending = self.run_plan_for_ui(plan, &state, &mut commands).await;
          queued.extend(pending);
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
          let mut plan = plan_factory(None).await;
          state.publish_test_list(&plan);
          if !run_all {
            retain_tests_in_files(&mut plan, &changed_paths);
          }
          if plan.total_tests == 0 { continue; }
          let pending = self.run_plan_for_ui(plan, &state, &mut commands).await;
          queued.extend(pending);
        }

        cmd = commands.recv() => {
          let Some(cmd) = cmd else { break };
          queued.push_back(cmd);
        }
      }
    }

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
    let mut plan = plan_factory(None).await;
    state.publish_test_list(&plan);
    match cmd {
      UiCommand::RunAll | UiCommand::Stop => {},
      UiCommand::RunFailed => {
        let rerun_path = self.config.output_dir.join("@rerun.txt");
        if rerun_path.exists() {
          crate::discovery::filter_by_rerun(&mut plan, &rerun_path);
        }
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
  /// cancels the run (the execute future is dropped, mirroring the TUI
  /// cancel path); every other command is returned for the caller to run
  /// afterwards.
  async fn run_plan_for_ui(
    &mut self,
    plan: TestPlan,
    state: &Arc<crate::ui_server::UiState>,
    commands: &mut tokio::sync::mpsc::UnboundedReceiver<crate::ui_server::UiCommand>,
  ) -> Vec<crate::ui_server::UiCommand> {
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
    {
      let execute = self.execute(plan, bus.clone());
      tokio::pin!(execute);
      loop {
        tokio::select! {
          _ = &mut execute => break,
          cmd = commands.recv() => match cmd {
            Some(crate::ui_server::UiCommand::Stop) => break,
            Some(other) => pending.push(other),
            None => break,
          }
        }
      }
    }
    bus.close();

    if let Some(handle) = driver_handle {
      if let Ok(reporters) = handle.await {
        self.reporters = reporters;
      }
    }
    let _ = forwarder.await;

    state.set_watch_status("idle");
    pending
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

/// Async closure producing a fresh [`TestPlan`] for a watch cycle.
/// `None` = build the full plan; `Some(paths)` = only re-process those
/// files (e.g. re-parse only changed `.feature` files). Async so
/// factories can re-bundle JS/TS step graphs per cycle.
pub type WatchPlanFactory =
  Box<dyn Fn(Option<Vec<std::path::PathBuf>>) -> futures::future::BoxFuture<'static, TestPlan> + Send + Sync>;

/// Build a test plan, optionally filtered to changed files.
async fn build_plan_for_changes(
  plan_factory: &WatchPlanFactory,
  run_all: bool,
  changed_paths: &[std::path::PathBuf],
) -> TestPlan {
  let changed = if run_all { None } else { Some(changed_paths.to_vec()) };
  let mut plan = plan_factory(changed).await;

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

fn build_launch_plan(browser_config: &crate::config::BrowserConfig) -> LaunchPlan {
  // BrowserConfig is already normalized (browser↔backend consistent).
  let backend = match browser_config.backend.as_str() {
    "cdp-raw" => BackendKind::CdpRaw,
    "webkit" => BackendKind::WebKit,
    "bidi" => BackendKind::Bidi,
    _ => BackendKind::CdpPipe,
  };

  let kind = match browser_config.browser.as_str() {
    "firefox" => BrowserKind::Firefox,
    "webkit" => BrowserKind::WebKit,
    _ => BrowserKind::Chromium,
  };

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

  LaunchPlan {
    backend,
    kind,
    headless,
    executable_path: browser_config.executable_path.clone(),
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
  }
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
