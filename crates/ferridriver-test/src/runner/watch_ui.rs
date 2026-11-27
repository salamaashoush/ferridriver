//! Watch mode (TUI / headless), UI server, and test-server orchestration.

use std::sync::Arc;

use ferridriver::Browser;

use super::TestRunner;
use super::browser::close_session_browser;
use crate::config::{CliOverrides, TestConfig};
use crate::model::TestPlan;
use crate::reporter::{EventBusBuilder, ReporterDriver};

fn announce_ui_url(url: &str) -> std::io::Result<()> {
  use std::io::Write as _;
  let mut out = std::io::stdout().lock();
  writeln!(out, "\n  ferridriver UI mode\n\n  {url}\n")?;
  out.flush()
}

impl TestRunner {
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
