//! Answering the UI's calls.
//!
//! One loop, one runner, requests handled in arrival order: discovery
//! (`listTests`), execution (`runTests`), cancellation (`stopTests`), and
//! the small housekeeping methods around them
//! (`testServerInterface.ts`). While a run is in flight the loop keeps
//! reading requests so `stopTests` still lands — everything else waits
//! its turn, because two runs sharing one browser would interleave.
//!
//! What the UI sees of a run is [`tele`]: the same reporter events
//! Playwright's own reporters emit, forwarded as they happen.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustc_hash::FxHashMap;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::tele;
use super::{Events, Request, TestServer};
use crate::config::{CliOverrides, ReporterConfig, TestConfig, TraceMode, UpdateSnapshotsMode, VideoMode};
use crate::model::{TestPlan, TestStatus};
use crate::reporter::{EventBus, EventBusBuilder, ReporterEvent, ReporterSet, Subscription};
use crate::runner::{ProjectHooks, ProjectRun, ProjectStream, TestRunner, WatchPlanFactory};

/// State the loop keeps between calls.
pub struct Driver {
  runner: TestRunner,
  plan_factory: WatchPlanFactory,
  root_dir: PathBuf,
  /// Files the UI asked to be told about (`watch`). Empty means it is
  /// not watching anything yet.
  watched: Vec<PathBuf>,
}

/// One project of a call, with the tests that call covers in it.
struct ProjectPlan {
  project: ProjectRun,
  plan: TestPlan,
}

/// What a call is about to work on: the projects it covers, and whatever
/// went wrong discovering them.
struct Listing {
  /// The discovered plan, before any project narrowed it — what the run
  /// itself is handed, since each project narrows it its own way.
  plan: TestPlan,
  projects: Vec<ProjectPlan>,
  errors: Vec<String>,
}

impl Listing {
  fn total_tests(&self) -> usize {
    self.projects.iter().map(|entry| entry.plan.total_tests).sum()
  }
}

impl Driver {
  pub fn new(runner: TestRunner, plan_factory: WatchPlanFactory, root_dir: PathBuf) -> Self {
    Self {
      runner,
      plan_factory,
      root_dir,
      watched: Vec::new(),
    }
  }

  /// Give the runner back once the loop is done with it.
  pub fn into_runner(self) -> TestRunner {
    self.runner
  }

  /// Files the UI is watching — a change outside them is not reported.
  #[must_use]
  pub fn watches(&self, path: &Path) -> bool {
    self.watched.is_empty() || self.watched.iter().any(|watched| watched == path)
  }

  /// Handle one request. Requests that arrive while a run is executing
  /// are handled by [`Self::handle_during_run`] instead.
  pub async fn handle(&mut self, request: Request, server: &TestServer) {
    match request.method.as_str() {
      "initialize" | "ping" | "clearCache" | "resizeTerminal" | "runGlobalTeardown" | "closeGracefully" => {
        // Nothing to set up, tear down, or clear: discovery is done per
        // call and the runner holds no cache between them.
        let result = if request.method == "runGlobalTeardown" {
          json!({ "report": [], "status": "passed" })
        } else {
          json!({})
        };
        request.respond(result);
      },
      "runGlobalSetup" => {
        // The UI needs a config before it will show anything; there is
        // no user-defined global setup to run.
        match self.runner_for(&request) {
          Ok((runner, _)) => {
            let projects = runner.project_runs();
            let infos: Vec<tele::ProjectInfo<'_>> = projects
              .iter()
              .map(|project| tele::ProjectInfo {
                name: &project.name,
                config: &project.config,
              })
              .collect();
            request.respond(json!({
              "report": [tele::configure(runner.config(), &self.root_dir, &infos)],
              "env": [],
              "status": "passed",
            }));
          },
          Err(message) => {
            server.events.report(tele::error(&message));
            request.respond(json!({ "report": [tele::error(&message)], "env": [], "status": "failed" }));
          },
        }
      },
      "checkBrowsers" => {
        let installer = ferridriver::install::BrowserInstaller::new();
        let has_browsers =
          installer.find_installed_chromium().is_some() || installer.find_installed_headless_shell().is_some();
        request.respond(json!({ "hasBrowsers": has_browsers }));
      },
      "installBrowsers" => {
        let installed = install_browsers(&server.events).await;
        match installed {
          Ok(()) => request.respond(json!({})),
          Err(message) => request.fail(message),
        }
      },
      "listFiles" | "listTests" => {
        let (runner, _) = match self.runner_for(&request) {
          Ok(setup) => setup,
          Err(message) => {
            request.respond(json!({ "report": [tele::error(&message)], "status": "failed" }));
            return;
          },
        };
        let listing = self.listing_for(&request, &runner).await;
        let status = if listing.errors.is_empty() { "passed" } else { "failed" };
        request.respond(json!({ "report": self.list_report(&runner, &listing), "status": status }));
      },
      "findRelatedTestFiles" => {
        // Without an import graph the honest answer is the test files
        // among the ones we were asked about.
        let build = (self.plan_factory)(None).await;
        let known: Vec<String> = build.plan.suites.iter().map(|suite| suite.file.clone()).collect();
        let files: Vec<String> = request
          .string_list("files")
          .into_iter()
          .filter(|file| {
            known
              .iter()
              .any(|known| known.ends_with(file.as_str()) || file.ends_with(known))
          })
          .collect();
        request.respond(json!({ "testFiles": files }));
      },
      "watch" => {
        self.watched = request
          .string_list("fileNames")
          .into_iter()
          .map(PathBuf::from)
          .collect();
        request.respond(json!({}));
      },
      "open" => {
        open_in_editor(request.params.get("location"));
        request.respond(json!({}));
      },
      "stopTests" => {
        // Nothing is running; a Stop that arrives between runs is a no-op.
        request.respond(json!({}));
      },
      other => {
        let message = format!("unknown method {other}");
        request.fail(message);
      },
    }
  }

  /// Requests that arrive while a run is executing: only cancellation and
  /// liveness are answered inline, the rest are returned to be handled
  /// after the run.
  pub fn handle_during_run(&self, request: Request) -> Option<Request> {
    match request.method.as_str() {
      "stopTests" => {
        self.runner.request_stop();
        request.respond(json!({}));
        None
      },
      "ping" | "resizeTerminal" => {
        request.respond(json!({}));
        None
      },
      _ => Some(request),
    }
  }

  /// `runTests`: filter the plan the way the UI asked, then execute it
  /// while streaming the run to every connected client.
  pub async fn run_tests(
    &mut self,
    request: &Request,
    server: &Events,
    requests: &mut mpsc::UnboundedReceiver<Request>,
    deferred: &mut std::collections::VecDeque<Request>,
  ) -> Value {
    // Pausing is the debugger's job, and the debugger has to be armed
    // before the run starts (`--debug` installs it). Saying so beats
    // running to completion while the caller believes it will stop.
    if (request.flag("pauseOnError") || request.flag("pauseAtEnd")) && crate::debug::debug_hook().is_none() {
      return refuse(
        server,
        "pausing needs the debugger: start the session with `--debug` (pause on the first call) or `--debug=fail` (pause at a failure)",
      );
    }

    let (runner, extra_reporters) = match self.runner_for(request) {
      Ok(setup) => setup,
      Err(message) => return refuse(server, &message),
    };

    let listing = self.listing_for(request, &runner).await;
    for event in self.list_report(&runner, &listing) {
      server.report(event);
    }
    if listing.total_tests() == 0 {
      let status = if listing.errors.is_empty() { "passed" } else { "failed" };
      return json!({ "status": status });
    }

    let summary = Box::pin(self.execute_streaming(
      runner,
      extra_reporters,
      request,
      listing.plan,
      server,
      requests,
      deferred,
    ))
    .await;
    let status = if summary.exit_code == 0 && listing.errors.is_empty() {
      "passed"
    } else {
      "failed"
    };
    json!({ "status": status })
  }

  /// The listing every `listTests` / `runTests` starts from: config, one
  /// project tree per project, the `onBegin` that tells the receiver to
  /// build them, and whatever discovery could not do.
  fn list_report(&self, runner: &TestRunner, listing: &Listing) -> Vec<Value> {
    let infos: Vec<tele::ProjectInfo<'_>> = listing
      .projects
      .iter()
      .map(|entry| tele::ProjectInfo {
        name: &entry.project.name,
        config: &entry.project.config,
      })
      .collect();
    let mut report = vec![tele::configure(runner.config(), &self.root_dir, &infos)];
    for (info, entry) in infos.iter().zip(&listing.projects) {
      report.push(tele::project(&self.root_dir, info, &entry.plan));
    }
    report.push(tele::begin());
    // A discovery or bundling failure is the only explanation the UI can
    // give for a tree that came back short; without it the run just looks
    // empty.
    for error in &listing.errors {
      report.push(tele::error(error));
    }
    let status = if listing.errors.is_empty() { "passed" } else { "failed" };
    report.push(tele::end(status, wall_ms(), Duration::ZERO));
    report
  }

  /// Discover, then narrow per project: a test's id is hashed with the
  /// name of the project running it, so `testIds` only mean anything
  /// once a project is fixed.
  async fn listing_for(&self, request: &Request, runner: &TestRunner) -> Listing {
    let build = (self.plan_factory)(None).await;
    let filter = RequestFilter::new(request, &self.root_dir);
    let wanted = request.string_list("projects");
    let projects = runner
      .project_runs()
      .into_iter()
      .filter(|project| wanted.is_empty() || wanted.iter().any(|name| name == &project.name))
      .map(|project| {
        let mut plan = project.narrow(&build.plan);
        filter.apply(&project.name, &mut plan);
        ProjectPlan { project, plan }
      })
      .collect();
    Listing {
      plan: build.plan,
      projects,
      errors: build.errors,
    }
  }

  /// The runner one call runs with: the session's, under the config that
  /// call asked for. Options the runner cannot honour are refused here,
  /// so a toggle the UI shows never silently does nothing.
  fn runner_for(&self, request: &Request) -> Result<(TestRunner, ReporterSet), String> {
    let mut config = self.runner.config().clone();
    let mut overrides = self.runner.overrides().clone();

    // A UI run is one attempt: Playwright forces both
    // (`testRunner.ts::_innerRunTests`) because re-running is a click.
    config.retries = 0;
    config.repeat_each = 1;

    apply_run_options(&mut config, &mut overrides, request)?;

    let names = request.string_list("reporters");
    let extra = if names.is_empty() {
      ReporterSet::default()
    } else {
      let configs: Vec<ReporterConfig> = names
        .into_iter()
        .map(|name| ReporterConfig {
          name,
          options: std::collections::BTreeMap::new(),
        })
        .collect();
      crate::reporter::create_reporters_pub(&configs, &config)
    };

    Ok((self.runner.with_run_options(Arc::new(config), overrides), extra))
  }

  /// Execute `plan` across the run's projects, forwarding every reporter
  /// event as a teleReporter event, and keep answering cancellation
  /// while it runs.
  #[allow(clippy::too_many_arguments)]
  async fn execute_streaming(
    &mut self,
    runner: TestRunner,
    extra_reporters: ReporterSet,
    request: &Request,
    plan: TestPlan,
    server: &Events,
    requests: &mut mpsc::UnboundedReceiver<Request>,
    deferred: &mut std::collections::VecDeque<Request>,
  ) -> crate::runner::ExecuteSummary {
    let started = Instant::now();
    let start_wall = wall_ms();

    let run_bus = self.runner.start_run_bus(extra_reporters);
    let bus = run_bus.bus.clone();

    // Each project reports on a stream of its own: two projects run the
    // same test file, and only the project name tells their results
    // apart (it is half of every test id).
    let timeouts: FxHashMap<String, Duration> = runner
      .project_runs()
      .into_iter()
      .map(|project| (project.name, Duration::from_millis(project.config.timeout)))
      .collect();
    let default_timeout = Duration::from_millis(runner.config().timeout);
    let events = server.clone();
    let shared = bus.clone();
    let stream = move |project: &str| -> ProjectStream {
      let mut builder = EventBusBuilder::new();
      let timeout = timeouts.get(project).copied().unwrap_or(default_timeout);
      let forwarder = tokio::spawn(forward_run(
        builder.subscribe(),
        events.clone(),
        project.to_string(),
        timeout,
      ));
      // The configured reporters still see one run, so a project's
      // events are re-emitted onto the run's shared bus.
      let pump = tokio::spawn(pump_into(builder.subscribe(), shared.clone()));
      ProjectStream {
        bus: builder.build(),
        drains: vec![forwarder, pump],
      }
    };
    let filter = RequestFilter::new(request, &self.root_dir);
    let narrow = move |project: &str, plan: &mut TestPlan| filter.apply(project, plan);

    self.runner.reset_stop();
    let summary = {
      let hooks = ProjectHooks {
        stream: Some(&stream),
        narrow: Some(&narrow),
      };
      let execute = runner.execute_projects_with_summary(plan, bus.clone(), hooks);
      tokio::pin!(execute);
      loop {
        tokio::select! {
          summary = &mut execute => break summary,
          request = requests.recv() => {
            let Some(request) = request else {
              // The UI is gone: cancel and let the run unwind rather than
              // leaving tests driving a browser nobody is watching.
              self.runner.request_stop();
              break (&mut execute).await;
            };
            if let Some(deferred_request) = self.handle_during_run(request) {
              deferred.push_back(deferred_request);
            }
          },
        }
      }
    };
    self.runner.finish_run_bus(run_bus).await;

    let status = if summary.exit_code == 0 { "passed" } else { "failed" };
    server.report(tele::end(status, start_wall, started.elapsed()));
    summary
  }
}

/// Answer a call the runner cannot honour: the UI's Errors tab gets the
/// reason, and the call fails rather than looking like it ran.
fn refuse(server: &Events, message: &str) -> Value {
  server.report(tele::error(message));
  json!({ "status": "failed", "error": message })
}

/// Re-emit one project's events onto the run's shared bus.
async fn pump_into(mut subscription: Subscription, bus: EventBus) {
  while let Some(event) = subscription.rx.recv().await {
    bus.emit(event);
  }
}

/// What a request narrowed its run to, applied per project because a
/// test's id is hashed with the name of the project running it.
struct RequestFilter {
  test_ids: std::collections::HashSet<String>,
  locations: Vec<String>,
  grep: Option<String>,
  grep_invert: Option<String>,
  root: PathBuf,
}

impl RequestFilter {
  fn new(request: &Request, root: &Path) -> Self {
    Self {
      test_ids: request.string_list("testIds").into_iter().collect(),
      locations: request.string_list("locations"),
      grep: request.string("grep").filter(|grep| !grep.is_empty()),
      grep_invert: request.string("grepInvert").filter(|grep| !grep.is_empty()),
      root: root.to_path_buf(),
    }
  }

  fn apply(&self, project: &str, plan: &mut TestPlan) {
    if !self.test_ids.is_empty() {
      retain_tests(plan, |test| self.test_ids.contains(&test.id.stable_id(project)));
    }
    if !self.locations.is_empty() {
      retain_tests(plan, |test| {
        self
          .locations
          .iter()
          .any(|location| matches_location(&self.root, &test.id.file, location))
      });
    }
    if let Some(grep) = &self.grep {
      crate::discovery::filter_by_grep(plan, grep, false);
    }
    if let Some(grep_invert) = &self.grep_invert {
      crate::discovery::filter_by_grep(plan, grep_invert, true);
    }
  }
}

/// Map one call's options onto the config and overrides it runs with
/// (`testServerInterface.ts::runTests` / `listTests`).
///
/// # Errors
///
/// Returns the reason when an option cannot be honoured — refusing beats
/// running to completion while the caller believes the option took.
fn apply_run_options(config: &mut TestConfig, overrides: &mut CliOverrides, request: &Request) -> Result<(), String> {
  if request.flag("onlyChanged") {
    return Err("onlyChanged is not supported: discovery does not consult git".to_string());
  }
  if request.flag("reuseContext") {
    return Err("reuseContext is not supported: every test gets a context of its own".to_string());
  }
  if let Some(endpoint) = request.string("connectWsEndpoint").filter(|value| !value.is_empty()) {
    return Err(format!(
      "connectWsEndpoint ({endpoint}) is not supported: a run uses the browser this session launched"
    ));
  }
  if let Some(method) = request
    .string("updateSourceMethod")
    .filter(|value| value != "overwrite")
  {
    return Err(format!(
      "updateSourceMethod {method} is not supported: snapshots are rewritten in place"
    ));
  }

  if let Some(headed) = request.bool("headed") {
    config.browser.headless = !headed;
  }
  if let Some(workers) = workers_from(request)? {
    config.workers = workers;
  }
  if let Some(max_failures) = request.number("maxFailures") {
    config.max_failures = u32::try_from(max_failures).unwrap_or(u32::MAX);
  }
  if let Some(timeout) = request.number("timeout") {
    config.timeout = timeout;
  }
  if let Some(mode) = request.string("updateSnapshots") {
    config.update_snapshots = match mode.as_str() {
      "all" => UpdateSnapshotsMode::All,
      "changed" => UpdateSnapshotsMode::Changed,
      "missing" => UpdateSnapshotsMode::Missing,
      "none" => UpdateSnapshotsMode::None,
      other => return Err(format!("updateSnapshots {other} is not a mode")),
    };
  }
  if let Some(trace) = request.string("trace") {
    config.trace = match trace.as_str() {
      "on" => TraceMode::On,
      "off" => TraceMode::Off,
      other => return Err(format!("trace {other} is not a mode a run can be asked for")),
    };
  }
  if let Some(video) = request.string("video") {
    config.video.mode = match video.as_str() {
      "on" => VideoMode::On,
      "off" => VideoMode::Off,
      other => return Err(format!("video {other} is not a mode a run can be asked for")),
    };
  }
  overrides.project_filter = request.string_list("projects");
  Ok(())
}

/// `workers` arrives as a number or a string, and the string may be a
/// share of the machine's cores (Playwright's `"50%"`).
fn workers_from(request: &Request) -> Result<Option<u32>, String> {
  let Some(value) = request.params.get("workers") else {
    return Ok(None);
  };
  let workers = match value {
    Value::Null => return Ok(None),
    Value::Number(number) => number.as_u64().and_then(|n| u32::try_from(n).ok()),
    Value::String(text) => match text.strip_suffix('%') {
      Some(percent) => percent.trim().parse::<u32>().ok().map(|percent| {
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get() as u32);
        (cores * percent / 100).max(1)
      }),
      None => text.trim().parse::<u32>().ok(),
    },
    other => return Err(format!("workers {other} is neither a count nor a percentage")),
  };
  match workers {
    Some(0) | None => Err(format!("workers {value} is not a usable worker count")),
    Some(workers) => Ok(Some(workers)),
  }
}

/// Turn the run's reporter events into the protocol's.
async fn forward_run(
  mut subscription: crate::reporter::Subscription,
  events: Events,
  project: String,
  timeout: Duration,
) {
  // Attempts in flight, so a step or a finish can name the result it
  // belongs to.
  let mut attempts: rustc_hash::FxHashMap<String, u32> = rustc_hash::FxHashMap::default();

  while let Some(event) = subscription.rx.recv().await {
    match event {
      ReporterEvent::TestStarted {
        test_id,
        attempt,
        worker_id,
        ..
      } => {
        let id = test_id.stable_id(&project);
        attempts.insert(id.clone(), attempt);
        events.report(tele::test_begin(&id, attempt, worker_id, wall_ms()));
      },
      ReporterEvent::StepStarted(step) => {
        let id = step.test_id.stable_id(&project);
        let attempt = attempts.get(&id).copied().unwrap_or(1);
        events.report(tele::step_begin(
          &id,
          attempt,
          &step.step_id,
          step.parent_step_id.as_deref(),
          &step.title,
          &step.category.to_string(),
          wall_ms(),
          step.location.as_ref(),
        ));
      },
      ReporterEvent::StepFinished(step) => {
        let id = step.test_id.stable_id(&project);
        let attempt = attempts.get(&id).copied().unwrap_or(1);
        events.report(tele::step_end(
          &id,
          attempt,
          &step.step_id,
          step.duration,
          step.error.as_deref(),
          &step.annotations,
        ));
      },
      ReporterEvent::TestFinished { outcome } => {
        let id = outcome.test_id.stable_id(&project);
        if !outcome.stdout.is_empty() {
          events.report(tele::stdio("stdout", Some(&id), outcome.attempt, &outcome.stdout));
        }
        if !outcome.stderr.is_empty() {
          events.report(tele::stdio("stderr", Some(&id), outcome.attempt, &outcome.stderr));
        }
        if !outcome.attachments.is_empty() {
          events.report(tele::attach(&id, &outcome));
        }
        events.report(tele::test_end(&id, &outcome, timeout));
        if outcome.status != TestStatus::Failed {
          attempts.remove(&id);
        }
      },
      ReporterEvent::RunError { error } => {
        events.report(tele::error(&error.message));
      },
      ReporterEvent::RunStarted { .. }
      | ReporterEvent::RunFinished { .. }
      | ReporterEvent::TestOutput { .. }
      | ReporterEvent::WorkerStarted { .. }
      | ReporterEvent::WorkerFinished { .. } => {},
    }
  }
}

/// Keep only the tests `keep` accepts, dropping suites left empty.
fn retain_tests(plan: &mut TestPlan, keep: impl Fn(&crate::model::TestCase) -> bool) {
  for suite in &mut plan.suites {
    suite.tests.retain(&keep);
  }
  plan.suites.retain(|suite| !suite.tests.is_empty());
  plan.total_tests = plan.suites.iter().map(|suite| suite.tests.len()).sum();
}

/// Whether `file` is the test file the UI named in `location`.
///
/// Locations arrive as escaped regexes over ABSOLUTE paths with the
/// leading slash removed (`uiModeView.tsx::escapeRegex` does that so
/// Playwright does not read them as regexes), while a plan's files may
/// be relative to the root. Comparing the tails of both, resolved
/// against the root, is what makes the two meet.
fn matches_location(root: &Path, file: &str, location: &str) -> bool {
  let cleaned = location.replace('\\', "");
  // Trailing `:line` / `:line:column`, when the UI sends a position.
  let path = cleaned
    .rsplit_once(':')
    .filter(|(_, tail)| tail.chars().all(|c| c.is_ascii_digit()) && !tail.is_empty())
    .map_or(cleaned.as_str(), |(head, _)| head)
    .trim_start_matches('/')
    .to_string();
  if path.is_empty() {
    return false;
  }

  let absolute = if Path::new(file).is_absolute() {
    file.to_string()
  } else {
    root.join(file).display().to_string()
  };
  let absolute = absolute.trim_start_matches('/');
  absolute.ends_with(&path) || path.ends_with(absolute) || file.ends_with(&path)
}

/// Open a source location in the user's editor, the way Playwright does
/// — through the OS handler for a `vscode://` URL.
fn open_in_editor(location: Option<&Value>) {
  let Some(location) = location else { return };
  let Some(file) = location.get("file").and_then(Value::as_str) else {
    return;
  };
  let line = location.get("line").and_then(Value::as_u64).unwrap_or(1);
  let url = format!("vscode://file/{file}:{line}");
  let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
  let _ = std::process::Command::new(opener)
    .arg(url)
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .spawn();
}

/// `installBrowsers`: the UI offers this when none are installed.
async fn install_browsers(events: &Events) -> Result<(), String> {
  use ferridriver::install::{BrowserInstaller, InstallProgress};

  let events = events.clone();
  let progress = move |progress: InstallProgress| {
    let line = match progress {
      InstallProgress::Resolving => "resolving browser version\n".to_string(),
      InstallProgress::Downloading {
        bytes_downloaded,
        total_bytes: Some(total),
      } => format!("downloading browser: {}%\n", bytes_downloaded * 100 / total.max(1)),
      InstallProgress::Downloading { bytes_downloaded, .. } => {
        format!("downloading browser: {} bytes\n", bytes_downloaded)
      },
      InstallProgress::Extracting => "extracting browser\n".to_string(),
      InstallProgress::Complete { version, .. } => format!("installed {version}\n"),
      InstallProgress::AlreadyInstalled { version, .. } => format!("{version} already installed\n"),
      InstallProgress::InstallingDeps { distro } => format!("installing dependencies for {distro}\n"),
      InstallProgress::DepsInstalled => "dependencies installed\n".to_string(),
    };
    events.send("stdio", json!({ "type": "stdout", "text": line }));
  };
  BrowserInstaller::new()
    .install_chromium(progress)
    .await
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// Epoch milliseconds — the clock the protocol's timestamps are in.
fn wall_ms() -> f64 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or_default()
    .as_secs_f64()
    * 1000.0
}

/// Serve the UI until the browser window closes or the process is
/// interrupted.
pub async fn serve(
  runner: TestRunner,
  plan_factory: WatchPlanFactory,
  watch_root: PathBuf,
  mut server: TestServer,
  shutdown: impl std::future::Future<Output = ()> + Send,
) -> TestRunner {
  let root_dir = std::path::absolute(&watch_root).unwrap_or(watch_root.clone());
  let mut driver = Driver::new(runner, plan_factory, root_dir);

  let watcher = match crate::watch::FileWatcher::new(
    &watch_root,
    &driver.runner.config().test_match,
    &driver.runner.config().test_ignore,
  ) {
    Ok(watcher) => Some(watcher),
    Err(e) => {
      tracing::warn!(target: "ferridriver::test_server", "file watching unavailable: {e}");
      None
    },
  };

  tokio::pin!(shutdown);
  // Requests that arrived during a run, in arrival order: a client that
  // queued a listing and then a run must get them back that way round.
  let mut deferred: std::collections::VecDeque<Request> = std::collections::VecDeque::new();
  loop {
    if let Some(request) = deferred.pop_front() {
      Box::pin(dispatch(&mut driver, request, &mut server, &mut deferred)).await;
      continue;
    }

    tokio::select! {
      // The UI window closing is how a session ends; ctrl-C is the same
      // thing from the terminal side.
      () = &mut shutdown => break,
      _ = tokio::signal::ctrl_c() => break,
      request = server.requests.recv() => match request {
        Some(request) => Box::pin(dispatch(&mut driver, request, &mut server, &mut deferred)).await,
        None => break,
      },
      change = watch_next(watcher.as_ref()) => {
        let Some(paths) = change else { continue };
        let watched: Vec<String> = paths
          .iter()
          .filter(|path| driver.watches(path))
          .map(|path| path.display().to_string())
          .collect();
        // Playwright's UI re-lists on any change and re-runs only what it
        // is watching, so an empty list is still worth sending.
        server.events.send("testFilesChanged", json!({ "testFiles": watched }));
      },
    }
  }

  driver.into_runner()
}

async fn dispatch(
  driver: &mut Driver,
  request: Request,
  server: &mut TestServer,
  deferred: &mut std::collections::VecDeque<Request>,
) {
  if request.method == "runTests" {
    let events = server.events.clone();
    let result = Box::pin(driver.run_tests(&request, &events, &mut server.requests, deferred)).await;
    request.respond(result);
    return;
  }
  driver.handle(request, server).await;
}

/// Next batch of changed files, or never when there is no watcher.
async fn watch_next(watcher: Option<&crate::watch::FileWatcher>) -> Option<Vec<PathBuf>> {
  let watcher = match watcher {
    Some(watcher) => watcher,
    None => return std::future::pending().await,
  };
  let change = watcher.recv().await?;
  let mut changes = vec![change];
  changes.extend(watcher.drain_deduped());
  Some(changes.into_iter().filter_map(change_path).collect())
}

fn change_path(change: crate::watch::ChangeKind) -> Option<PathBuf> {
  use crate::watch::ChangeKind;
  match change {
    ChangeKind::TestFile(path)
    | ChangeKind::FeatureFile(path)
    | ChangeKind::SourceFile(path)
    | ChangeKind::StepFile(path) => Some(path),
    ChangeKind::Config => None,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn locations_match_absolute_ui_paths_against_relative_plan_paths() {
    let root = Path::new("/repo");
    // What the UI actually sends: an absolute path, escaped, without its
    // leading slash.
    assert!(matches_location(
      root,
      "tests/a.spec.ts",
      "repo\\/tests\\/a\\.spec\\.ts"
    ));
    assert!(matches_location(root, "/repo/tests/a.spec.ts", "repo/tests/a.spec.ts"));
    assert!(matches_location(root, "tests/a.spec.ts", "tests/a.spec.ts:12"));
    assert!(!matches_location(root, "tests/a.spec.ts", "repo/tests/b.spec.ts"));
    assert!(!matches_location(root, "tests/a.spec.ts", ""));
  }

  #[test]
  fn retain_drops_emptied_suites_and_recounts() {
    let mut plan = TestPlan {
      suites: vec![crate::model::TestSuite {
        name: "s".into(),
        file: "a.spec.ts".into(),
        tests: vec![case("keep"), case("drop")],
        hooks: crate::model::Hooks::default(),
        annotations: Vec::new(),
        mode: crate::model::SuiteMode::Parallel,
      }],
      total_tests: 2,
      shard: None,
    };
    retain_tests(&mut plan, |test| test.id.name == "keep");
    assert_eq!(plan.total_tests, 1);
    assert_eq!(plan.suites.len(), 1);

    retain_tests(&mut plan, |_| false);
    assert_eq!(plan.total_tests, 0);
    assert!(plan.suites.is_empty(), "emptied suites are dropped");
  }

  #[test]
  fn run_options_reach_the_config_the_run_uses() {
    let mut config = TestConfig::default();
    config.browser.headless = true;
    let mut overrides = CliOverrides::default();
    let request = request(json!({
      "headed": true,
      "workers": 1,
      "maxFailures": 1,
      "timeout": 12_000,
      "updateSnapshots": "all",
      "trace": "off",
      "video": "on",
      "projects": ["webkit", "bidi"],
    }));

    apply_run_options(&mut config, &mut overrides, &request).expect("options are all supported");

    assert!(!config.browser.headless, "headed:true launches a headed browser");
    assert_eq!(config.workers, 1, "the UI's Single worker toggle");
    assert_eq!(config.max_failures, 1, "the UI's Stop on first failure toggle");
    assert_eq!(config.timeout, 12_000);
    assert_eq!(config.update_snapshots, UpdateSnapshotsMode::All);
    assert_eq!(config.trace, TraceMode::Off);
    assert_eq!(config.video.mode, VideoMode::On);
    assert_eq!(overrides.project_filter, vec!["webkit".to_string(), "bidi".to_string()]);
  }

  #[test]
  fn an_absent_option_leaves_the_config_alone() {
    let mut config = TestConfig::default();
    config.browser.headless = true;
    config.workers = 4;
    config.trace = TraceMode::On;
    let before = (config.browser.headless, config.workers, config.trace);

    apply_run_options(&mut config, &mut CliOverrides::default(), &request(json!({}))).expect("nothing to apply");

    assert_eq!((config.browser.headless, config.workers, config.trace), before);
  }

  #[test]
  fn options_the_runner_cannot_honour_are_refused() {
    for params in [
      json!({ "reuseContext": true }),
      json!({ "connectWsEndpoint": "ws://elsewhere" }),
      json!({ "updateSourceMethod": "3way" }),
      json!({ "onlyChanged": true }),
      json!({ "trace": "retain-on-failure" }),
      json!({ "video": "sometimes" }),
      json!({ "updateSnapshots": "occasionally" }),
    ] {
      let refusal = apply_run_options(
        &mut TestConfig::default(),
        &mut CliOverrides::default(),
        &request(params.clone()),
      );
      assert!(refusal.is_err(), "{params} was accepted and then ignored");
    }
  }

  #[test]
  fn workers_arrive_as_a_count_or_a_share_of_the_machine() {
    assert_eq!(workers_from(&request(json!({}))), Ok(None));
    assert_eq!(workers_from(&request(json!({ "workers": 3 }))), Ok(Some(3)));
    assert_eq!(workers_from(&request(json!({ "workers": "2" }))), Ok(Some(2)));
    let cores = std::thread::available_parallelism().map_or(4, |n| n.get() as u32);
    assert_eq!(
      workers_from(&request(json!({ "workers": "50%" }))),
      Ok(Some((cores / 2).max(1)))
    );
    assert!(workers_from(&request(json!({ "workers": 0 }))).is_err());
    assert!(workers_from(&request(json!({ "workers": "many" }))).is_err());
  }

  #[test]
  fn test_ids_are_resolved_against_the_project_asked_about() {
    let plan = || TestPlan {
      suites: vec![crate::model::TestSuite {
        name: "s".into(),
        file: "a.spec.ts".into(),
        tests: vec![case("keep"), case("drop")],
        hooks: crate::model::Hooks::default(),
        annotations: Vec::new(),
        mode: crate::model::SuiteMode::Parallel,
      }],
      total_tests: 2,
      shard: None,
    };
    let wanted = case("keep").id.stable_id("cdp-pipe");
    let filter = RequestFilter::new(&request(json!({ "testIds": [wanted] })), Path::new("/repo"));

    let mut for_cdp = plan();
    filter.apply("cdp-pipe", &mut for_cdp);
    assert_eq!(for_cdp.total_tests, 1, "the id names a test of this project");

    let mut for_webkit = plan();
    filter.apply("webkit", &mut for_webkit);
    assert_eq!(
      for_webkit.total_tests, 0,
      "the same title in another project is another id, and was not asked for"
    );
  }

  fn request(params: Value) -> Request {
    Request {
      method: "runTests".to_string(),
      params,
      reply: None,
    }
  }

  fn case(name: &str) -> crate::model::TestCase {
    crate::model::TestCase {
      id: crate::model::TestId {
        file: "a.spec.ts".into(),
        suite: None,
        name: name.into(),
        line: Some(1),
        column: None,
      },
      test_fn: std::sync::Arc::new(|_| Box::pin(async { Ok(()) })),
      fixture_requests: Vec::new(),
      annotations: Vec::new(),
      timeout: None,
      retries: None,
      expected_status: crate::model::ExpectedStatus::Pass,
      use_options: None,
    }
  }
}
