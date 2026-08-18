//! Reporter system: event-driven, multiplexed, trait-based.

pub mod allure;
pub mod api;
pub mod base;
pub mod bdd;
pub mod blob;
pub mod ctrf;
pub mod dot;
pub mod empty;
pub mod github;
pub mod html;
pub mod json;
pub mod junit;
pub mod line;
pub mod markdown;
pub mod progress;
pub mod rerun;
pub mod tap;
pub mod teamcity;
pub mod terminal;

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::model::{StepCategory, StepLocation, TestAnnotation, TestId, TestOutcome};

// ── Events ──

#[derive(Debug, Clone)]
pub struct StepStartedEvent {
  pub test_id: TestId,
  /// Project the attempt belongs to. Two projects run the same
  /// `test_id` concurrently off one bus, so a consumer that keys live
  /// state by test needs it to tell them apart.
  pub project: String,
  pub step_id: String,
  pub parent_step_id: Option<String>,
  pub title: String,
  pub category: StepCategory,
  /// Where the step happened — its own file, which need not be the
  /// test's: an explicit `test.step(…, { location })` and every BDD
  /// step name one the spec does not.
  pub location: Option<StepLocation>,
}

#[derive(Debug, Clone)]
pub struct StepFinishedEvent {
  pub test_id: TestId,
  /// See [`StepStartedEvent::project`].
  pub project: String,
  pub step_id: String,
  pub title: String,
  pub category: StepCategory,
  pub duration: Duration,
  pub error: Option<String>,
  /// Arbitrary metadata attached to this step (e.g. BDD keyword/text).
  pub metadata: Option<serde_json::Value>,
  /// Annotations the step recorded while it ran (`step.skip()`).
  pub annotations: Vec<TestAnnotation>,
}

/// One chunk a test wrote while it ran, delivered as it happens.
#[derive(Debug, Clone)]
pub struct TestOutputEvent {
  pub test_id: TestId,
  /// See [`StepStartedEvent::project`].
  pub project: String,
  /// `true` for stderr, `false` for stdout.
  pub stderr: bool,
  pub text: String,
}

/// How a whole run ended. Mirrors Playwright's `FullResult.status`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RunStatus {
  #[default]
  Passed,
  Failed,
  TimedOut,
  Interrupted,
}

impl RunStatus {
  #[must_use]
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Passed => "passed",
      Self::Failed => "failed",
      Self::TimedOut => "timedout",
      Self::Interrupted => "interrupted",
    }
  }

  /// Parse back from [`Self::as_str`]. Anything unrecognised reads as
  /// `passed` — the only statuses a producer emits are these four, and
  /// inventing a failure from a typo would fail a green run.
  #[must_use]
  pub fn parse(s: &str) -> Self {
    match s {
      "failed" => Self::Failed,
      "timedout" | "timedOut" => Self::TimedOut,
      "interrupted" => Self::Interrupted,
      _ => Self::Passed,
    }
  }
}

/// Events emitted during a test run.
#[derive(Debug, Clone)]
pub enum ReporterEvent {
  /// The entire run is starting.
  RunStarted {
    total_tests: usize,
    num_workers: u32,
    /// Arbitrary metadata from config (Playwright's `metadata` field).
    metadata: serde_json::Value,
    /// Wall-clock start of the run — the `stats.startTime` every
    /// serialized report carries.
    start_time: std::time::SystemTime,
    /// The run's `FullConfig` and its whole `Suite` tree — Playwright's
    /// `onConfigure(config)` and `onBegin(suite)` arguments, resolved
    /// once by the runner so every reporter reads the same tree.
    preamble: Arc<api::RunPreamble>,
  },
  /// A worker has been spawned.
  WorkerStarted { worker_id: u32 },
  /// A test is about to execute.
  TestStarted {
    test_id: TestId,
    /// See [`StepStartedEvent::project`].
    project: String,
    attempt: u32,
    /// Worker running it. A UI that follows a live trace needs it: the
    /// in-progress trace files live in that worker's artifacts directory.
    worker_id: u32,
  },
  /// A step within a test has started (real-time, emitted during execution).
  StepStarted(Arc<StepStartedEvent>),
  /// A step within a test has finished (real-time, emitted during execution).
  StepFinished(Arc<StepFinishedEvent>),
  /// A test wrote to stdout/stderr. Emitted as the write happens so a
  /// live reporter or UI can stream it; the same text is also replayed
  /// in bulk on [`Self::TestFinished`].
  TestOutput(Arc<TestOutputEvent>),
  /// A test finished (pass, fail, skip, etc.).
  ///
  /// Shared rather than cloned: an outcome carries the attempt's
  /// screenshots and step tree, and the bus hands it to every
  /// subscriber and every reporter within one.
  TestFinished { outcome: Arc<TestOutcome> },
  /// An error that belongs to no single test — a config failure, a
  /// worker that died, a global-setup throw. Mirrors Playwright's
  /// `Reporter.onError`.
  RunError { error: Box<crate::model::TestFailure> },
  /// A worker has shut down.
  WorkerFinished { worker_id: u32 },
  /// The entire run completed.
  RunFinished {
    total: usize,
    passed: usize,
    failed: usize,
    skipped: usize,
    flaky: usize,
    duration: Duration,
    status: RunStatus,
  },
}

/// A reporter's edits to the corpus, from Playwright's
/// `Reporter.preprocess({ config, suite, testRun })`. The reporter is
/// handed the whole tree before the run and may drop cases from it or
/// annotate them; `TestRun.skipSharding()` says the reporter has taken
/// sharding over itself.
#[derive(Debug, Default, Clone)]
pub struct TestRunEdits {
  /// Stable case ids to drop from the run (`TestRun.exclude`).
  pub excluded: Vec<String>,
  /// Annotations to apply before a case runs — `TestRun.skip`,
  /// `fixme` and `fail`, keyed by stable case id.
  pub annotations: Vec<(String, TestAnnotation)>,
  /// `TestRun.skipSharding()`: the run's `--shard` is not applied.
  pub skip_sharding: bool,
}

// ── Reporter Trait ──

/// Trait that all reporters implement.
#[async_trait::async_trait]
pub trait Reporter: Send + Sync {
  /// Called for every event.
  async fn on_event(&mut self, event: &ReporterEvent);

  /// Called after the run to finalize output (write files, close streams).
  async fn finalize(&mut self) -> ferridriver::error::Result<()> {
    Ok(())
  }

  /// Playwright's `Reporter.preprocess`: the reporter sees the corpus
  /// before the run and may edit it. Unlike every other callback, an
  /// error here is NOT swallowed — a half-applied edit must not reach
  /// the workers.
  ///
  /// # Errors
  ///
  /// Whatever the reporter raised, which aborts the run.
  async fn preprocess(&mut self, _preamble: &api::RunPreamble, _edits: &mut TestRunEdits) -> Result<(), String> {
    Ok(())
  }

  /// How this reporter says the run ended, when it says so at all.
  /// Playwright's `onEnd` may return `{ status }`, and its multiplexer
  /// lets that overwrite the run's own verdict — the last reporter to
  /// answer wins. Read once, after [`Self::finalize`].
  fn status_override(&self) -> Option<RunStatus> {
    None
  }

  /// Whether this reporter writes to the terminal. Playwright's
  /// `printsToStdio()`: when nothing in a set does, a line (or dot,
  /// under CI) reporter goes in FRONT of the set so the run is not
  /// silent — first, so a reporter that stalls in its finalize cannot
  /// swallow the output.
  fn prints_to_stdio(&self) -> bool {
    false
  }
}

// ── Reporter Set (multiplexer) ──

/// Multiplexes events to multiple reporters.
pub struct ReporterSet {
  reporters: Vec<Box<dyn Reporter>>,
}

impl Default for ReporterSet {
  fn default() -> Self {
    Self { reporters: Vec::new() }
  }
}

impl ReporterSet {
  pub fn new(reporters: Vec<Box<dyn Reporter>>) -> Self {
    Self { reporters }
  }

  pub fn is_empty(&self) -> bool {
    self.reporters.is_empty()
  }

  /// Append an additional reporter (e.g., NAPI ResultCollector).
  pub fn add(&mut self, reporter: Box<dyn Reporter>) {
    self.reporters.push(reporter);
  }

  /// Replace all reporters with a new set.
  pub fn replace(&mut self, reporters: Vec<Box<dyn Reporter>>) {
    self.reporters = reporters;
  }

  pub async fn emit(&mut self, event: &ReporterEvent) {
    for reporter in &mut self.reporters {
      reporter.on_event(event).await;
    }
  }

  pub async fn finalize(&mut self) {
    for reporter in &mut self.reporters {
      if let Err(e) = reporter.finalize().await {
        tracing::error!("reporter finalize error: {e}");
      }
    }
  }

  /// Whether anything in the set writes to the terminal. Playwright's
  /// multiplexer answers the same way — `some`, not `all`.
  #[must_use]
  pub fn prints_to_stdio(&self) -> bool {
    self.reporters.iter().any(|r| r.prints_to_stdio())
  }

  /// Let every reporter edit the corpus before the run. Errors
  /// propagate: Playwright's multiplexer deliberately does not swallow
  /// a `preprocess` throw.
  ///
  /// # Errors
  ///
  /// The first reporter error, verbatim.
  pub async fn preprocess(&mut self, preamble: &api::RunPreamble, edits: &mut TestRunEdits) -> Result<(), String> {
    for reporter in &mut self.reporters {
      reporter.preprocess(preamble, edits).await?;
    }
    Ok(())
  }

  /// The run status the reporters between them decided on, if any.
  /// Playwright's multiplexer overwrites `result.status` with each
  /// reporter's returned one in turn, so the last answer wins.
  #[must_use]
  pub fn status_override(&self) -> Option<RunStatus> {
    self.reporters.iter().filter_map(|r| r.status_override()).next_back()
  }
}

// ── Event Bus ──

/// Builder for constructing an `EventBus` with registered subscribers.
///
/// Register all subscribers before calling `build()`. Once built, the bus
/// is immutable — no new subscribers can be added. This ensures workers
/// (which clone the bus) fan out to a fixed set of consumers.
pub struct EventBusBuilder {
  subscribers: Vec<mpsc::UnboundedSender<ReporterEvent>>,
}

impl Default for EventBusBuilder {
  fn default() -> Self {
    Self::new()
  }
}

impl EventBusBuilder {
  pub fn new() -> Self {
    Self {
      subscribers: Vec::new(),
    }
  }

  /// Register a subscriber. Returns a `Subscription` (the receiving end).
  /// Must be called before `build()`.
  pub fn subscribe(&mut self) -> Subscription {
    let (tx, rx) = mpsc::unbounded_channel();
    self.subscribers.push(tx);
    Subscription { rx }
  }

  /// Finalize the bus. No more subscribers can be added after this.
  pub fn build(self) -> EventBus {
    let has_subscribers = !self.subscribers.is_empty();
    EventBus {
      inner: Arc::new(EventBusInner {
        has_subscribers,
        subscribers: std::sync::RwLock::new(self.subscribers),
      }),
    }
  }
}

/// The receiving end of a subscriber channel.
pub struct Subscription {
  pub rx: mpsc::UnboundedReceiver<ReporterEvent>,
}

/// Fan-out event bus. Workers clone this and call `emit()` — events are
/// delivered to all subscribers registered at build time.
///
/// Clone is cheap (Arc internals). All clones share the same subscriber list.
#[derive(Clone)]
pub struct EventBus {
  inner: Arc<EventBusInner>,
}

struct EventBusInner {
  has_subscribers: bool,
  /// Subscriber channels — frozen after build. Read-only during emit (no lock needed).
  /// `close()` swaps to empty Vec via `std::sync::RwLock` (write only on shutdown).
  subscribers: std::sync::RwLock<Vec<mpsc::UnboundedSender<ReporterEvent>>>,
}

impl EventBus {
  pub fn has_subscribers(&self) -> bool {
    self.inner.has_subscribers
  }

  /// Emit an event to all subscribers. Lock-free read path — `RwLock::read()` never
  /// blocks other readers. Only `close()` takes a write lock (once, at shutdown).
  pub fn emit(&self, event: ReporterEvent) {
    if !self.inner.has_subscribers {
      return;
    }
    let subs = self.inner.subscribers.read().expect("EventBus RwLock poisoned");
    if subs.is_empty() {
      return;
    }
    let last = subs.len() - 1;
    for sub in &subs[..last] {
      let _ = sub.send(event.clone());
    }
    let _ = subs[last].send(event);
  }

  /// Explicitly close all sender channels.
  pub fn close(&self) {
    self
      .inner
      .subscribers
      .write()
      .expect("EventBus RwLock poisoned")
      .clear();
  }
}

// ── Reporter Driver ──

/// Standalone consumer that drains a `Subscription` and drives a `ReporterSet`.
/// Decoupled from test execution — can run as an independent tokio task.
///
/// Spawn this with `tokio::spawn(driver.run())`. When the event bus is dropped
/// (all senders gone), the subscription channel closes, the driver finalizes
/// all reporters, and returns the `ReporterSet` for potential reuse.
pub struct ReporterDriver {
  reporters: ReporterSet,
  subscription: Subscription,
}

impl ReporterDriver {
  pub fn new(reporters: ReporterSet, subscription: Subscription) -> Self {
    Self {
      reporters,
      subscription,
    }
  }

  /// Consume events until the channel closes, finalize reporters, return them.
  pub async fn run(mut self) -> ReporterSet {
    while let Some(event) = self.subscription.rx.recv().await {
      self.reporters.emit(&event).await;
    }
    self.reporters.finalize().await;
    self.reporters
  }
}

// ── Host-provided reporters ──

/// Builds a reporter for a name the built-in factory does not answer
/// to. Playwright's `loadReporter`: a reporter description that is not
/// a built-in name is a module path, and the module's default export is
/// the reporter class.
///
/// Registered before the run, so loading — resolving, bundling,
/// compiling, instantiating — happens where it can fail loudly, and
/// [`ReporterFactory::create`] itself is the synchronous hand-off
/// [`create_reporters`] needs.
pub trait ReporterFactory: Send + Sync {
  /// The reporter `entry` names, or `None` when this factory does not
  /// own the name.
  fn create(
    &self,
    entry: &crate::config::ReporterConfig,
    config: &crate::config::TestConfig,
  ) -> Option<Box<dyn Reporter>>;
}

static REPORTER_FACTORY: std::sync::RwLock<Option<Arc<dyn ReporterFactory>>> = std::sync::RwLock::new(None);

/// Install the factory consulted for every reporter name outside
/// [`REPORTER_NAMES`]. Replaces any previous one.
pub fn set_reporter_factory(factory: Arc<dyn ReporterFactory>) {
  if let Ok(mut slot) = REPORTER_FACTORY.write() {
    *slot = Some(factory);
  }
}

/// The installed factory, if a host registered one.
#[must_use]
pub fn reporter_factory() -> Option<Arc<dyn ReporterFactory>> {
  REPORTER_FACTORY.read().ok().and_then(|slot| slot.clone())
}

/// What a set of reporters is being built for. Playwright's `mode`:
/// only a real run gets a fallback terminal reporter put in front —
/// merging shards prints its own summary and must not gain one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReporterMode {
  #[default]
  Test,
  Merge,
}

// ── Factory ──

/// Unified reporter factory. Creates reporters from `names`, resolving
/// each one's output path and options against `config`.
///
/// `names` is passed separately from `config.reporter` because a caller
/// may run one set of reporters over a config that declares another
/// (the test server drives per-request reporters).
pub fn create_reporters_pub(
  names: &[crate::config::ReporterConfig],
  config: &crate::config::TestConfig,
) -> ReporterSet {
  create_reporters(names, config)
}

/// [`create_reporters_pub`] for a caller that is not running tests.
pub fn create_reporters_mode(
  names: &[crate::config::ReporterConfig],
  config: &crate::config::TestConfig,
  mode: ReporterMode,
) -> ReporterSet {
  build_reporters(names, config, mode)
}

/// Every reporter name the factory answers to, with the aliases that
/// map onto it. Used by the factory and by the CLI's `--reporter`
/// validation, so an unknown name is caught with the list in hand
/// rather than silently dropped.
pub const REPORTER_NAMES: &[&str] = &[
  "list",
  "line",
  "dot",
  "json",
  "junit",
  "html",
  "blob",
  "github",
  "null",
  "tap",
  "tap-flat",
  "teamcity",
  "ctrf",
  "markdown",
  "allure",
  "progress",
  "rerun",
  "cucumber-json",
  "messages",
  "usage",
];

pub(crate) fn create_reporters(
  names: &[crate::config::ReporterConfig],
  config: &crate::config::TestConfig,
) -> ReporterSet {
  build_reporters(names, config, ReporterMode::Test)
}

fn build_reporters(
  names: &[crate::config::ReporterConfig],
  config: &crate::config::TestConfig,
  mode: ReporterMode,
) -> ReporterSet {
  let output_dir = config.output_dir.as_path();
  let quiet = config.quiet;
  let report_slow_tests = config.report_slow_tests.clone();
  if names.len() == 1 && matches!(names[0].name.as_str(), "none" | "null" | "empty") {
    return ReporterSet::default();
  }

  let factory = reporter_factory();
  let mut reporters: Vec<Box<dyn Reporter>> = Vec::new();
  let mut has_terminal = false;

  for entry in names {
    let opts = &entry.options;
    let out = |default_name: &str| base::resolve_output_file(&entry.name, opts, output_dir, default_name);
    match entry.name.as_str() {
      // Terminal reporter handles both E2E and BDD — detects BDD by step metadata.
      "terminal" | "list" | "bdd" | "default" | "" => {
        if !has_terminal && !quiet {
          reporters.push(Box::new(
            terminal::TerminalReporter::new().with_slow_tests_config(report_slow_tests.clone()),
          ));
          has_terminal = true;
        }
      },
      "line" => {
        if !quiet {
          reporters.push(Box::new(
            line::LineReporter::new().with_slow_tests_config(report_slow_tests.clone()),
          ));
        }
      },
      "json" => {
        reporters.push(Box::new(
          json::JsonReporter::new(out("results.json")).with_config(config),
        ));
      },
      "junit" => {
        reporters.push(Box::new(
          junit::JUnitReporter::new(out("junit.xml"))
            .with_include_project_in_test_name(base::bool_option(opts, "junit", "includeProjectInTestName"))
            .with_include_retries(base::bool_option(opts, "junit", "includeRetries"))
            .with_strip_ansi(base::bool_option(opts, "junit", "stripANSIControlSequences"))
            .with_omit_tags(base::bool_option(opts, "junit", "omitTags"))
            .with_suite_id(base::str_option(opts, "suiteId").unwrap_or_default())
            .with_suite_name(base::str_option(opts, "suiteName").unwrap_or_default()),
        ));
      },
      "dot" => {
        reporters.push(Box::new(
          dot::DotReporter::new().with_slow_tests_config(report_slow_tests.clone()),
        ));
      },
      "null" | "empty" => {
        reporters.push(Box::new(empty::EmptyReporter));
      },
      "blob" => {
        // `path` is the historical option name; `outputFile` is the one
        // every other file reporter takes.
        let path = base::str_option(opts, "path")
          .map(std::path::PathBuf::from)
          .unwrap_or_else(|| out("report.zip"));
        let mut reporter = blob::BlobReporter::new(path);
        if let (Some(current), Some(total)) = (
          opts
            .get("shard_index")
            .or_else(|| opts.get("shardIndex"))
            .and_then(serde_json::Value::as_u64)
            .and_then(|v| u32::try_from(v).ok()),
          opts
            .get("shard_total")
            .or_else(|| opts.get("shardTotal"))
            .and_then(serde_json::Value::as_u64)
            .and_then(|v| u32::try_from(v).ok()),
        ) {
          reporter = reporter.with_shard(current, total);
        }
        reporters.push(Box::new(reporter));
      },
      "github" => {
        // Wraps the terminal reporter so users see human-readable
        // output AND the CI annotations from a single flag. The
        // wrapped reporter respects `quiet`.
        let inner: Box<dyn Reporter> = if quiet {
          Box::new(empty::EmptyReporter)
        } else {
          Box::new(terminal::TerminalReporter::new().with_slow_tests_config(report_slow_tests.clone()))
        };
        let mut reporter = github::GithubReporter::new(inner);
        if let Some(force) = opts.get("enabled").and_then(serde_json::Value::as_bool) {
          reporter = reporter.with_enabled(force);
        }
        reporters.push(Box::new(reporter));
      },

      // ── Shared reporters (same for both modes) ──
      "html" => {
        let mut reporter = html::HtmlReporter::new(out("report.html"));
        if let Some(open) = base::str_option(opts, "open") {
          reporter = reporter.with_open_mode(html::OpenMode::parse(&open));
        }
        reporters.push(Box::new(reporter));
      },
      "tap" => {
        reporters.push(Box::new(tap::TapReporter::new(tap::TapStyle::Nested)));
      },
      "tap-flat" => {
        reporters.push(Box::new(tap::TapReporter::new(tap::TapStyle::Flat)));
      },
      "teamcity" => {
        reporters.push(Box::new(teamcity::TeamCityReporter::new()));
      },
      "ctrf" => {
        reporters.push(Box::new(ctrf::CtrfReporter::new(out("ctrf-report.json"))));
      },
      "markdown" => {
        reporters.push(Box::new(markdown::MarkdownReporter::new(out("report.md"))));
      },
      "allure" => {
        let dir = base::str_option(opts, "output_dir")
          .or_else(|| base::str_option(opts, "outputDir"))
          .map(std::path::PathBuf::from)
          .unwrap_or_else(|| output_dir.join("allure-results"));
        let mut reporter = allure::AllureReporter::new(dir);
        if let Some(title) = base::str_option(opts, "suite_title").or_else(|| base::str_option(opts, "suiteTitle")) {
          reporter = reporter.with_suite_title(title);
        }
        reporters.push(Box::new(reporter));
      },
      "progress" => {
        reporters.push(Box::new(
          progress::ProgressReporter::new().with_slow_tests_config(report_slow_tests.clone()),
        ));
      },
      "rerun" => {
        reporters.push(Box::new(rerun::RerunReporter::new(out("@rerun.txt"))));
      },

      // ── BDD-specific reporters (usable in any mode) ──
      "cucumber-json" | "cucumber" => {
        reporters.push(Box::new(bdd::cucumber_json::CucumberJsonReporter::new(out(
          "cucumber.json",
        ))));
      },
      "messages" | "ndjson" => {
        reporters.push(Box::new(bdd::messages::CucumberMessagesReporter::new(out(
          "cucumber-messages.ndjson",
        ))));
      },
      "usage" => {
        reporters.push(Box::new(bdd::usage::UsageReporter::new()));
      },

      other => {
        if let Some(reporter) = factory.as_ref().and_then(|f| f.create(entry, config)) {
          reporters.push(reporter);
        } else {
          tracing::warn!(
            "unknown reporter '{other}' — known reporters: {}",
            REPORTER_NAMES.join(", ")
          );
        }
      },
    }
  }

  if reporters.is_empty() {
    reporters.push(Box::new(terminal::TerminalReporter::new()));
  } else if mode == ReporterMode::Test && !quiet && !reporters.iter().any(|r| r.prints_to_stdio()) {
    // Playwright `runner/reporters.ts::createReporters`: nothing in the
    // set writes to the terminal, so a run would be silent. Line off
    // CI, dot on it, and FIRST — a reporter that stalls in `onEnd`
    // must not be able to swallow the output.
    let fallback: Box<dyn Reporter> = if std::env::var_os("CI").is_some() {
      Box::new(dot::DotReporter::new().with_slow_tests_config(report_slow_tests.clone()))
    } else {
      Box::new(line::LineReporter::new().with_slow_tests_config(report_slow_tests.clone()))
    };
    reporters.insert(0, fallback);
  }

  // Two reporters drawing on the same terminal produce unreadable output,
  // and `line` in particular rewrites the last line it printed — which
  // another reporter may have replaced. Playwright refuses the same
  // combination; warn rather than drop one, since the user asked for both.
  let stdio: Vec<&str> = names
    .iter()
    .map(|entry| entry.name.as_str())
    .filter(|name| {
      matches!(
        *name,
        "terminal" | "list" | "bdd" | "default" | "" | "line" | "dot" | "progress" | "github" | "tap" | "tap-flat"
      )
    })
    .collect();
  if stdio.len() > 1 && !quiet {
    tracing::warn!(
      "several reporters write to the terminal ({}) — their output will interleave;        keep one and give the others a file",
      stdio.join(", ")
    );
  }

  // Always add the rerun reporter so @rerun.txt is available for --last-failed.
  let has_rerun = names.iter().any(|c| c.name == "rerun");
  if !has_rerun {
    reporters.push(Box::new(rerun::RerunReporter::new(output_dir.join("@rerun.txt"))));
  }

  ReporterSet::new(reporters)
}
