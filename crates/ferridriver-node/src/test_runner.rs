//! High-performance NAPI test runner.
//!
//! The Rust side drives everything: browser pool, worker dispatch, parallel
//! execution, retry, fixture lifecycle, reporters. The TS side registers
//! test callbacks, calls `run()`, and gets results.
//!
//! Flow:
//! 1. TS creates `TestRunner.create(config)`
//! 2. TS registers tests via `register_test(meta, callback)` — callback is the JS test body
//! 3. TS calls `run()` — Rust launches browsers, dispatches tests in parallel,
//!    calls JS callbacks with Page fixtures, collects results
//! 4. `run()` returns `RunSummary`

use std::sync::Arc;
use std::time::Instant;

use napi::Result;
use napi::Status;
use napi::threadsafe_function::ThreadsafeFunction;
use napi_derive::napi;
use tokio::sync::Mutex;

use crate::bdd_registry::BddRegistry;

/// Test callback TSFN type — async JS function receiving TestFixtures, returning Promise<void>.
/// Matches Playwright's `({ page, browserName, testInfo, ... }) => Promise<void>` signature.
/// callee_handled=false (modern async), weak=true (doesn't block Node exit), unbounded queue.
type TestCallbackFn = ThreadsafeFunction<
  crate::test_fixtures::TestFixtures,
  napi::bindgen_prelude::Promise<()>,
  crate::test_fixtures::TestFixtures,
  Status,
  false,
  true,
  0,
>;

/// Threadsafe function for nullary JS callbacks (no args, returns Promise<void>).
/// Used for `globalSetup` / `globalTeardown` function-form hooks supplied via
/// `defineConfig({ globalSetupFn: ... })`. The pool argument from the core
/// runner is ignored -- these hooks don't have fixture access.
type GlobalHookFn = ThreadsafeFunction<(), napi::bindgen_prelude::Promise<()>, (), Status, false, true, 0>;

/// Static fixture names shared across all test registrations.
/// Avoids per-test String allocations for the standard fixture set.
const STANDARD_FIXTURE_NAMES: &[&str] = &["browser", "context", "page", "test_info", "request"];

/// Build the standard fixture request Vec from static strings.
fn standard_fixture_requests() -> Vec<String> {
  STANDARD_FIXTURE_NAMES.iter().map(|s| (*s).to_string()).collect()
}

/// A single entry in the batch registration array.
/// `object_to_js = false` because it contains a Function (can only be received from JS, not sent).
#[napi(object, object_to_js = false)]
pub struct TestBatchEntry<'a> {
  pub meta: TestMeta,
  pub callback:
    napi::bindgen_prelude::Function<'a, crate::test_fixtures::TestFixtures, napi::bindgen_prelude::Promise<()>>,
}

/// CLI argument overrides applied on top of a `FerridriverConfig` already
/// passed to `TestRunner.create()`. Mirrors `ferridriver_test::config::CliOverrides`
/// field-for-field; conversion is `to_core`.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct NapiCliOverrides {
  pub workers: Option<i32>,
  pub retries: Option<i32>,
  pub timeout: Option<f64>,
  pub reporter: Option<Vec<String>>,
  pub grep: Option<String>,
  pub grep_invert: Option<String>,
  pub tag: Option<String>,
  pub headless: Option<bool>,
  pub shard_current: Option<i32>,
  pub shard_total: Option<i32>,
  pub config_path: Option<String>,
  pub output_dir: Option<String>,
  pub test_files: Option<Vec<String>>,
  pub test_match: Option<Vec<String>>,
  pub list_only: Option<bool>,
  pub update_snapshots: Option<String>,
  pub profile: Option<String>,
  pub forbid_only: Option<bool>,
  pub last_failed: Option<bool>,
  pub video: Option<String>,
  pub trace: Option<String>,
  pub storage_state: Option<String>,
  pub max_failures: Option<i32>,
  pub repeat_each: Option<i32>,
  pub fail_fast: Option<bool>,
  pub global_timeout: Option<f64>,
  pub ignore_snapshots: Option<bool>,
  pub pass_with_no_tests: Option<bool>,
  pub tsconfig: Option<String>,
  pub name: Option<String>,
  pub fully_parallel: Option<bool>,
  pub project_filter: Option<Vec<String>>,
  pub no_deps: Option<bool>,
  pub teardown: Option<String>,
  pub only_changed: Option<String>,
  pub fail_on_flaky_tests: Option<bool>,
  pub browser: Option<String>,
  pub backend: Option<String>,
  pub channel: Option<String>,
  pub executable_path: Option<String>,
  pub browser_args: Option<Vec<String>>,
  pub base_url: Option<String>,
  pub viewport_width: Option<i32>,
  pub viewport_height: Option<i32>,
  pub is_mobile: Option<bool>,
  pub has_touch: Option<bool>,
  pub color_scheme: Option<String>,
  pub locale: Option<String>,
  pub offline: Option<bool>,
  pub bypass_csp: Option<bool>,
  pub bdd_tags: Option<String>,
  pub bdd_dry_run: Option<bool>,
  pub bdd_strict: Option<bool>,
  pub bdd_fail_fast: Option<bool>,
  pub bdd_step_timeout: Option<f64>,
  pub bdd_order: Option<String>,
  pub bdd_language: Option<String>,
}

impl NapiCliOverrides {
  fn to_core(&self) -> ferridriver_test::config::CliOverrides {
    let update_snapshots = self.update_snapshots.as_deref().and_then(|m| match m {
      "all" => Some(ferridriver_test::config::UpdateSnapshotsMode::All),
      "changed" => Some(ferridriver_test::config::UpdateSnapshotsMode::Changed),
      "missing" => Some(ferridriver_test::config::UpdateSnapshotsMode::Missing),
      "none" => Some(ferridriver_test::config::UpdateSnapshotsMode::None),
      _ => None,
    });
    let shard = match (self.shard_current, self.shard_total) {
      (Some(c), Some(t)) if c > 0 && t > 0 => Some(ferridriver_test::config::ShardArg {
        current: c as u32,
        total: t as u32,
      }),
      _ => None,
    };
    ferridriver_test::config::CliOverrides {
      workers: self.workers.map(|w| w as u32),
      retries: self.retries.map(|r| r as u32),
      timeout: self.timeout.map(|t| t as u64),
      reporter: self.reporter.clone().unwrap_or_default(),
      grep: self.grep.clone(),
      grep_invert: self.grep_invert.clone(),
      tag: self.tag.clone(),
      headless: self.headless.unwrap_or(false),
      shard,
      config_path: self.config_path.clone(),
      output_dir: self.output_dir.clone(),
      test_files: self.test_files.clone().unwrap_or_default(),
      test_match: self.test_match.clone(),
      list_only: self.list_only.unwrap_or(false),
      update_snapshots,
      profile: self.profile.clone(),
      forbid_only: self.forbid_only.unwrap_or(false),
      last_failed: self.last_failed.unwrap_or(false),
      video: self.video.clone(),
      trace: self.trace.clone(),
      storage_state: self.storage_state.clone(),
      max_failures: self.max_failures.map(|n| n as u32),
      repeat_each: self.repeat_each.map(|n| n as u32),
      fail_fast: self.fail_fast.unwrap_or(false),
      global_timeout: self.global_timeout.map(|t| t as u64),
      ignore_snapshots: self.ignore_snapshots.unwrap_or(false),
      pass_with_no_tests: self.pass_with_no_tests.unwrap_or(false),
      tsconfig: self.tsconfig.clone(),
      name: self.name.clone(),
      fully_parallel: self.fully_parallel,
      project_filter: self.project_filter.clone().unwrap_or_default(),
      no_deps: self.no_deps.unwrap_or(false),
      teardown: self.teardown.clone(),
      only_changed: self.only_changed.clone(),
      fail_on_flaky_tests: self.fail_on_flaky_tests.unwrap_or(false),
      browser: self.browser.clone(),
      backend: self.backend.clone(),
      channel: self.channel.clone(),
      executable_path: self.executable_path.clone(),
      browser_args: self.browser_args.clone().unwrap_or_default(),
      base_url: self.base_url.clone(),
      viewport_width: self.viewport_width.map(|v| v as i64),
      viewport_height: self.viewport_height.map(|v| v as i64),
      is_mobile: self.is_mobile,
      has_touch: self.has_touch,
      color_scheme: self.color_scheme.clone(),
      locale: self.locale.clone(),
      offline: self.offline,
      bypass_csp: self.bypass_csp,
      bdd_tags: self.bdd_tags.clone(),
      bdd_dry_run: self.bdd_dry_run.unwrap_or(false),
      bdd_strict: self.bdd_strict.unwrap_or(false),
      bdd_fail_fast: self.bdd_fail_fast.unwrap_or(false),
      bdd_step_timeout: self.bdd_step_timeout.map(|t| t as u64),
      bdd_order: self.bdd_order.clone(),
      bdd_language: self.bdd_language.clone(),
    }
  }
}

/// Flattened annotation passed from TS — avoids serde_json round-trip.
/// Exactly one of the fields should be set.
#[napi(object, use_nullable = true)]
#[derive(Debug, Clone)]
pub struct NapiAnnotation {
  /// "skip", "slow", "fixme", "fail", "only", "tag", "info"
  pub kind: String,
  pub reason: Option<String>,
  pub condition: Option<String>,
  /// For "tag" annotations: the tag string. For "info": the type_name.
  pub value: Option<String>,
  /// For "info" annotations: the description.
  pub description: Option<String>,
}

impl NapiAnnotation {
  /// Convert to core TestAnnotation.
  fn to_core(&self) -> Option<ferridriver_test::model::TestAnnotation> {
    match self.kind.as_str() {
      "skip" => Some(ferridriver_test::model::TestAnnotation::Skip {
        reason: self.reason.clone(),
        condition: self.condition.clone(),
      }),
      "slow" => Some(ferridriver_test::model::TestAnnotation::Slow {
        reason: self.reason.clone(),
        condition: self.condition.clone(),
      }),
      "fixme" => Some(ferridriver_test::model::TestAnnotation::Fixme {
        reason: self.reason.clone(),
        condition: self.condition.clone(),
      }),
      "fail" => Some(ferridriver_test::model::TestAnnotation::Fail {
        reason: self.reason.clone(),
        condition: self.condition.clone(),
      }),
      "only" => Some(ferridriver_test::model::TestAnnotation::Only),
      "tag" => self
        .value
        .as_ref()
        .map(|v| ferridriver_test::model::TestAnnotation::Tag(v.clone())),
      "info" => {
        let type_name = self.value.clone().unwrap_or_default();
        let description = self.description.clone().unwrap_or_default();
        Some(ferridriver_test::model::TestAnnotation::Info { type_name, description })
      },
      _ => None,
    }
  }
}

/// Metadata for a registered test.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct TestMeta {
  pub id: String,
  pub title: String,
  pub file: String,
  pub timeout: Option<f64>,
  pub retries: Option<i32>,
  /// Suite ID this test belongs to (from register_suite).
  pub suite_id: Option<String>,
  /// Annotations — flattened for direct NAPI transfer (no JSON round-trip).
  pub annotations: Vec<NapiAnnotation>,
  /// Fixture overrides from test.use() — merged with global config by the worker.
  pub use_options: Option<serde_json::Value>,
  /// Optional list of fixture names this test actually uses (e.g. `["page"]`).
  /// When set, only these fixtures (plus test_info) are requested from the pool,
  /// saving browser/context/request creation for tests that don't need them.
  /// When None, all standard fixtures are requested (backwards-compatible).
  pub requested_fixtures: Option<Vec<String>>,
}

/// Metadata for a test suite (describe block).
#[napi(object)]
#[derive(Debug, Clone)]
pub struct SuiteMeta {
  /// Unique suite name.
  pub name: String,
  /// Source file.
  pub file: String,
  /// "parallel" (default) or "serial".
  pub mode: Option<String>,
}

/// Hook registration metadata.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct HookMeta {
  /// Suite ID this hook belongs to.
  pub suite_id: String,
  /// "beforeAll", "afterAll", "beforeEach", "afterEach".
  pub kind: String,
  /// Optional list of fixtures the hook body actually reads.
  pub requested_fixtures: Option<Vec<String>>,
}

/// Result of a single test.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct TestResultItem {
  pub id: String,
  pub title: String,
  pub status: String,
  pub duration_ms: f64,
  pub attempt: i32,
  pub error_message: Option<String>,
}

/// Summary of the full run.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct RunSummary {
  pub total: i32,
  pub passed: i32,
  pub failed: i32,
  pub skipped: i32,
  pub flaky: i32,
  pub duration_ms: f64,
  pub exit_code: i32,
  pub results: Vec<TestResultItem>,
}

/// A registered test: metadata + JS callback.
struct RegisteredTest {
  meta: TestMeta,
  /// The JS test body: async (page: Page) => void.
  /// Called from Rust workers with a Page fixture.
  callback: Arc<TestCallbackFn>,
}

/// A registered suite.
#[allow(dead_code)]
struct RegisteredSuite {
  meta: SuiteMeta,
  id: String,
}

/// A registered hook.
#[allow(dead_code)]
#[derive(Clone)]
struct RegisteredHook {
  meta: ferridriver_test::HookRegistration,
  callback: Arc<TestCallbackFn>,
}

/// Replay every blob (`*.zip`) under `dir` through the given list of
/// reporters and write a unified summary into `output_dir`.
///
/// Mirrors Playwright's `npx playwright merge-reports <dir>`. Returns
/// the run summary (`total`/`passed`/`failed`/...) so the CLI can
/// surface it. Reporter names map to the same factory the test
/// runner uses (`terminal`, `dot`, `json`, `junit`, `html`, `github`,
/// `null`/`empty`, `blob`, ...).
/// `#[allow(dead_code)]` because the `#[napi]` macro generates the
/// FFI export but cargo's lib-test build doesn't see the JS-side
/// caller, so the function reads as unused without a JS consumer.
#[napi]
#[allow(dead_code)]
pub async fn merge_reports(
  dir: String,
  reporters: Option<Vec<String>>,
  output_dir: Option<String>,
) -> Result<RunSummary> {
  use ferridriver_test::reporter::{ReporterEvent, blob, create_reporters_pub};
  use std::path::PathBuf;

  let path = PathBuf::from(dir);
  if !path.is_dir() {
    return Err(napi::Error::from_reason(format!(
      "merge-reports: {} is not a directory",
      path.display()
    )));
  }
  let events = blob::read_blob_dir(&path).map_err(napi::Error::from_reason)?;

  let output = output_dir.map_or_else(|| PathBuf::from("merged-report"), PathBuf::from);
  let reporter_names: Vec<ferridriver_test::config::ReporterConfig> = reporters
    .unwrap_or_else(|| vec!["terminal".into()])
    .into_iter()
    .map(|name| ferridriver_test::config::ReporterConfig {
      name,
      options: std::collections::BTreeMap::new(),
    })
    .collect();
  let mut set = create_reporters_pub(&reporter_names, &output, false, false, None);

  let mut summary = RunSummary {
    total: 0,
    passed: 0,
    failed: 0,
    skipped: 0,
    flaky: 0,
    duration_ms: 0.0,
    exit_code: 0,
    results: Vec::new(),
  };
  for event in &events {
    if let ReporterEvent::TestFinished { test_id, outcome } = event {
      let status = match outcome.status {
        ferridriver_test::model::TestStatus::Passed => "passed",
        ferridriver_test::model::TestStatus::Failed => "failed",
        ferridriver_test::model::TestStatus::TimedOut => "timed out",
        ferridriver_test::model::TestStatus::Skipped => "skipped",
        ferridriver_test::model::TestStatus::Flaky => "flaky",
        ferridriver_test::model::TestStatus::Interrupted => "interrupted",
      };
      summary.total += 1;
      match status {
        "passed" => summary.passed += 1,
        "skipped" => summary.skipped += 1,
        "flaky" => {
          summary.flaky += 1;
          summary.passed += 1;
        },
        _ => summary.failed += 1,
      }
      summary.results.push(TestResultItem {
        id: test_id.full_name(),
        title: test_id.name.clone(),
        status: status.into(),
        duration_ms: outcome.duration.as_secs_f64() * 1000.0,
        attempt: outcome.attempt as i32,
        error_message: outcome.error.as_ref().map(|e| e.message.clone()),
      });
    }
    set.emit(event).await;
  }
  set.finalize().await;

  if summary.failed > 0 {
    summary.exit_code = 1;
  }
  Ok(summary)
}

/// The test runner. Manages browser pool, dispatches tests to workers,
/// calls JS callbacks with fixtures, collects results.
#[napi]
pub struct TestRunner {
  config: ferridriver_test::TestConfig,
  last_failed: bool,
  watch: bool,
  grep: Option<String>,
  // E2E state
  tests: Mutex<Vec<RegisteredTest>>,
  suites: Mutex<Vec<RegisteredSuite>>,
  hooks: Mutex<Vec<RegisteredHook>>,
  // TS-authored Reporter dispatchers registered via
  // `register_js_reporter`. Drained into the core runner's
  // `add_reporter` slot at the start of `run`.
  js_reporters: Mutex<Vec<crate::js_reporter::JsReporter>>,
  // Function-form global setup/teardown hooks supplied by the TS config.
  // Drained into the core runner's `TestHooks` when `run()` builds the
  // underlying runner. The string-form `globalSetup` / `globalTeardown`
  // file paths in `TestConfig` still flow through the data schema.
  global_setup_fns: Mutex<Vec<Arc<GlobalHookFn>>>,
  global_teardown_fns: Mutex<Vec<Arc<GlobalHookFn>>>,
  // BDD state
  bdd: BddRegistry,
}

#[napi]
impl TestRunner {
  /// Create a new test runner from a serialized `FerridriverConfig`.
  ///
  /// `config_json` is the JSON representation of `ferridriver_config::FerridriverConfig`
  /// (typically produced by `JSON.stringify({ test: userConfig })` in TS). The
  /// payload owns the schema -- no per-field flat NAPI struct, no manual mapper.
  /// CLI argument overrides are layered later via `apply_overrides()`; runtime-
  /// only state (grep/last_failed/watch/verbose/debug) uses dedicated setters.
  #[napi(factory)]
  pub fn create(config_json: String) -> Result<Self> {
    let unified: ferridriver_config::FerridriverConfig = serde_json::from_str(&config_json)
      .map_err(|e| napi::Error::new(Status::GenericFailure, format!("invalid config json: {e}")))?;

    // Initialise tracing from environment by default; explicit verbose is set
    // post-create via `set_verbose()` when the CLI parses --verbose.
    ferridriver_test::logging::init_from_env();

    // Apply env vars + auto-detect workers + normalize, with no CLI overrides.
    let tc =
      ferridriver_test::config::resolve_config_from(unified.test, &ferridriver_test::config::CliOverrides::default())
        .map_err(|e| napi::Error::new(Status::GenericFailure, e))?;

    Ok(Self {
      config: tc,
      last_failed: false,
      watch: false,
      grep: None,
      tests: Mutex::new(Vec::new()),
      suites: Mutex::new(Vec::new()),
      hooks: Mutex::new(Vec::new()),
      js_reporters: Mutex::new(Vec::new()),
      global_setup_fns: Mutex::new(Vec::new()),
      global_teardown_fns: Mutex::new(Vec::new()),
      bdd: BddRegistry::new(),
    })
  }

  /// Layer CLI argument overrides on top of the loaded config.
  ///
  /// Runs the standard `resolve_config_from` pipeline (env vars + overrides +
  /// auto-detect + normalize). Designed to be called once after `create()`,
  /// before `run()`.
  #[napi]
  pub fn apply_overrides(&mut self, overrides: NapiCliOverrides) -> Result<()> {
    let core = overrides.to_core();
    let new_config = ferridriver_test::config::resolve_config_from(self.config.clone(), &core)
      .map_err(|e| napi::Error::new(Status::GenericFailure, e))?;
    self.config = new_config;
    Ok(())
  }

  /// Grep pattern used to filter tests by name. Runtime-only -- not part of the
  /// serialized config schema.
  #[napi]
  pub fn set_grep(&mut self, pattern: String) {
    self.grep = if pattern.is_empty() { None } else { Some(pattern) };
  }

  /// Re-run only tests that failed on the previous run.
  #[napi]
  pub fn set_last_failed(&mut self, enabled: bool) {
    self.last_failed = enabled;
  }

  /// Watch mode: re-run tests on file changes.
  #[napi]
  pub fn set_watch(&mut self, enabled: bool) {
    self.watch = enabled;
  }

  /// Verbose tracing level: 0 = off (default), 1 = debug, 2 = trace.
  #[napi]
  pub fn set_verbose(&mut self, level: u32) {
    if level > 0 {
      ferridriver_test::logging::init(level as u8);
    }
  }

  /// Debug category filter -- same syntax as the `FERRIDRIVER_DEBUG` env var.
  /// Stored on the process env so the centralised tracing subscriber picks it up.
  #[napi]
  pub fn set_debug(&mut self, categories: String) {
    #[allow(unused_unsafe)]
    unsafe {
      std::env::set_var("FERRIDRIVER_DEBUG", categories);
    }
  }

  /// Register a function-form global setup hook. Runs once before any test
  /// starts, ahead of the file-path `globalSetup` entries in `TestConfig`.
  /// The callback takes no arguments and must return a `Promise<void>`.
  #[napi(ts_args_type = "callback: () => Promise<void>")]
  pub fn register_global_setup(
    &self,
    callback: napi::bindgen_prelude::Function<'_, (), napi::bindgen_prelude::Promise<()>>,
  ) -> Result<()> {
    let tsfn = callback
      .build_threadsafe_function()
      .callee_handled::<false>()
      .weak::<true>()
      .max_queue_size::<0>()
      .build()?;
    let mut slot = self
      .global_setup_fns
      .try_lock()
      .map_err(|_| napi::Error::from_reason("global_setup_fns lock contended"))?;
    slot.push(Arc::new(tsfn));
    Ok(())
  }

  /// Register a function-form global teardown hook. Runs once after every
  /// test finishes, ahead of the file-path `globalTeardown` entries.
  /// The callback takes no arguments and must return a `Promise<void>`.
  #[napi(ts_args_type = "callback: () => Promise<void>")]
  pub fn register_global_teardown(
    &self,
    callback: napi::bindgen_prelude::Function<'_, (), napi::bindgen_prelude::Promise<()>>,
  ) -> Result<()> {
    let tsfn = callback
      .build_threadsafe_function()
      .callee_handled::<false>()
      .weak::<true>()
      .max_queue_size::<0>()
      .build()?;
    let mut slot = self
      .global_teardown_fns
      .try_lock()
      .map_err(|_| napi::Error::from_reason("global_teardown_fns lock contended"))?;
    slot.push(Arc::new(tsfn));
    Ok(())
  }

  /// Register a test. The callback receives fixtures (page, testInfo, browserName, etc.).
  /// Called from TS after loading each test file.
  #[napi(ts_args_type = "meta: TestMeta, callback: (fixtures: TestFixtures) => Promise<void>")]
  pub fn register_test(
    &self,
    meta: TestMeta,
    callback: napi::bindgen_prelude::Function<
      '_,
      crate::test_fixtures::TestFixtures,
      napi::bindgen_prelude::Promise<()>,
    >,
  ) -> Result<()> {
    // callee_handled::<false>() — modern async pattern (rspack/rolldown standard).
    // max_queue_size::<0>() — unbounded queue, prevents backpressure blocking Rust.
    // weak::<true>() — doesn't prevent Node.js process from exiting.
    let tsfn = callback
      .build_threadsafe_function()
      .callee_handled::<false>()
      .weak::<true>()
      .max_queue_size::<0>()
      .build()?;

    // Can't use .await in a sync method, so use try_lock.
    let mut tests = self
      .tests
      .try_lock()
      .map_err(|_| napi::Error::from_reason("tests lock contended during registration"))?;
    tests.push(RegisteredTest {
      meta,
      callback: Arc::new(tsfn),
    });
    Ok(())
  }

  /// Batch-register multiple tests in a single NAPI call.
  /// Takes the lock once, reserves capacity, and builds all TSFNs in one go.
  /// Reduces N NAPI boundary crossings + N lock acquisitions to 1 each.
  #[napi(ts_args_type = "entries: Array<{ meta: TestMeta, callback: (fixtures: TestFixtures) => Promise<void> }>")]
  pub fn register_tests_batch(&self, entries: Vec<TestBatchEntry<'_>>) -> Result<()> {
    let mut tests = self
      .tests
      .try_lock()
      .map_err(|_| napi::Error::from_reason("tests lock contended during batch registration"))?;
    tests.reserve(entries.len());

    for entry in entries {
      let tsfn = entry
        .callback
        .build_threadsafe_function()
        .callee_handled::<false>()
        .weak::<true>()
        .max_queue_size::<0>()
        .build()?;

      tests.push(RegisteredTest {
        meta: entry.meta,
        callback: Arc::new(tsfn),
      });
    }

    Ok(())
  }

  /// Register a test suite (describe block). Returns a suite ID for test/hook registration.
  #[napi]
  pub fn register_suite(&self, meta: SuiteMeta) -> Result<String> {
    let id = format!("{}::{}", meta.file, meta.name);
    let mut suites = self
      .suites
      .try_lock()
      .map_err(|_| napi::Error::from_reason("suites lock contended"))?;
    suites.push(RegisteredSuite { meta, id: id.clone() });
    Ok(id)
  }

  /// Register a lifecycle hook for a suite.
  #[napi(ts_args_type = "meta: HookMeta, callback: (fixtures: TestFixtures) => Promise<void>")]
  pub fn register_hook(
    &self,
    meta: HookMeta,
    callback: napi::bindgen_prelude::Function<
      '_,
      crate::test_fixtures::TestFixtures,
      napi::bindgen_prelude::Promise<()>,
    >,
  ) -> Result<()> {
    let tsfn = callback
      .build_threadsafe_function()
      .callee_handled::<false>()
      .weak::<true>()
      .max_queue_size::<0>()
      .build()?;

    let mut hooks = self
      .hooks
      .try_lock()
      .map_err(|_| napi::Error::from_reason("hooks lock contended"))?;
    let (phase, scope) = match meta.kind.as_str() {
      "beforeAll" => (ferridriver_test::HookPhase::Before, ferridriver_test::HookScope::Suite),
      "afterAll" => (ferridriver_test::HookPhase::After, ferridriver_test::HookScope::Suite),
      "beforeEach" => (
        ferridriver_test::HookPhase::Before,
        ferridriver_test::HookScope::Scenario,
      ),
      "afterEach" => (
        ferridriver_test::HookPhase::After,
        ferridriver_test::HookScope::Scenario,
      ),
      _ => {
        return Err(napi::Error::from_reason(format!(
          "unknown test hook kind: {}",
          meta.kind
        )));
      },
    };
    hooks.push(RegisteredHook {
      meta: ferridriver_test::HookRegistration {
        phase,
        scope,
        owner: ferridriver_test::HookOwner::Suite(meta.suite_id),
        tags: None,
        requested_fixtures: meta.requested_fixtures.unwrap_or_default(),
      },
      callback: Arc::new(tsfn),
    });
    Ok(())
  }

  // ── BDD registration methods (delegate to BddRegistry) ──

  /// Register a BDD step definition from TypeScript.
  /// Callback receives unified TestFixtures (with BDD args/dataTable/docString populated).
  #[napi(
    ts_args_type = "kind: 'given' | 'when' | 'then' | 'step', pattern: string, callback: (fixtures: TestFixtures) => Promise<void>, isRegex?: boolean, timeout?: number"
  )]
  pub fn register_step(
    &self,
    kind: String,
    pattern: String,
    callback: napi::bindgen_prelude::Function<
      '_,
      crate::test_fixtures::TestFixtures,
      napi::bindgen_prelude::Promise<()>,
    >,
    is_regex: Option<bool>,
    timeout: Option<f64>,
  ) -> Result<()> {
    self.bdd.register_step(kind, pattern, callback, is_regex, timeout)
  }

  /// Register a BDD lifecycle hook from TypeScript.
  /// Callback receives unified TestFixtures (BDD fields are null for hooks).
  #[napi(
    ts_args_type = "point: 'before' | 'after', scope: 'scenario' | 'step' | 'all', callback: (fixtures: TestFixtures) => Promise<void>, tags?: string, name?: string, timeout?: number"
  )]
  pub fn register_bdd_hook(
    &self,
    point: String,
    scope: String,
    callback: napi::bindgen_prelude::Function<
      '_,
      crate::test_fixtures::TestFixtures,
      napi::bindgen_prelude::Promise<()>,
    >,
    tags: Option<String>,
    name: Option<String>,
    timeout: Option<f64>,
  ) -> Result<()> {
    let _ = name;
    let _ = timeout;

    let tsfn = callback
      .build_threadsafe_function()
      .callee_handled::<false>()
      .weak::<true>()
      .max_queue_size::<0>()
      .build()?;

    let (phase, scope) = match (point.as_str(), scope.as_str()) {
      ("before", "all") => (ferridriver_test::HookPhase::Before, ferridriver_test::HookScope::Suite),
      ("after", "all") => (ferridriver_test::HookPhase::After, ferridriver_test::HookScope::Suite),
      ("before", "scenario") => (
        ferridriver_test::HookPhase::Before,
        ferridriver_test::HookScope::Scenario,
      ),
      ("after", "scenario") => (
        ferridriver_test::HookPhase::After,
        ferridriver_test::HookScope::Scenario,
      ),
      ("before", "step") => (ferridriver_test::HookPhase::Before, ferridriver_test::HookScope::Step),
      ("after", "step") => (ferridriver_test::HookPhase::After, ferridriver_test::HookScope::Step),
      _ => {
        return Err(napi::Error::from_reason(format!(
          "unknown BDD hook point/scope: {point}/{scope}"
        )));
      },
    };

    let mut hooks = self
      .hooks
      .try_lock()
      .map_err(|_| napi::Error::from_reason("hooks lock contended"))?;
    hooks.push(RegisteredHook {
      meta: ferridriver_test::HookRegistration {
        phase,
        scope,
        owner: ferridriver_test::HookOwner::Root,
        tags,
        requested_fixtures: Vec::new(),
      },
      callback: Arc::new(tsfn),
    });
    Ok(())
  }

  /// Register a custom parameter type for Cucumber expressions.
  #[napi(ts_args_type = "name: string, regex: string")]
  pub fn define_parameter_type(&self, name: String, regex: String) -> Result<()> {
    self.bdd.define_parameter_type(name, regex)
  }

  /// Register a TS-authored Reporter via the dispatcher pattern.
  /// `dispatcher` is built by `defineReporter(impl)` in
  /// `packages/ferridriver-test/src/reporter.ts`; ferridriver calls it
  /// once per `ReporterEvent` with `(eventName, args)` so the helper
  /// can fan out to the right method on the user's Reporter object.
  #[napi(ts_args_type = "dispatcher: (payload: { event: string, args: unknown[] }) => unknown")]
  pub fn register_js_reporter(
    &self,
    dispatcher: napi::bindgen_prelude::Function<'_, serde_json::Value, napi::bindgen_prelude::Unknown<'static>>,
  ) -> Result<()> {
    let reporter = crate::js_reporter::JsReporter::build(dispatcher)?;
    let mut slot = self
      .js_reporters
      .try_lock()
      .map_err(|_| napi::Error::from_reason("js_reporters lock contended during registration"))?;
    slot.push(reporter);
    Ok(())
  }

  /// Run all registered tests (E2E + BDD) through the core TestRunner pipeline.
  ///
  /// Converts registered JS tests into a TestPlan, optionally adds BDD feature
  /// tests, delegates to the core runner for browser launch, parallel dispatch,
  /// retries, filtering, and reporting.
  #[napi]
  #[allow(clippy::too_many_lines)]
  pub async fn run(&self, feature_files: Option<Vec<String>>) -> Result<RunSummary> {
    use ferridriver_test::model::{ExpectedStatus, SuiteMode, TestAnnotation, TestCase, TestFailure, TestId};

    let tests = self.tests.lock().await;

    // Convert registered JS tests into core TestCase objects.
    let browser_config = self.config.browser.clone();
    let test_cases: Vec<TestCase> = tests
      .iter()
      .map(|t| {
        let cb = Arc::clone(&t.callback);
        let meta = t.meta.clone();
        let bcfg = browser_config.clone();
        let requested_fixtures = meta
          .requested_fixtures
          .clone()
          .unwrap_or_else(standard_fixture_requests);

        // Convert flattened NAPI annotations directly — no JSON round-trip.
        let annotations: Vec<TestAnnotation> = meta.annotations.iter().filter_map(NapiAnnotation::to_core).collect();

        TestCase {
          id: TestId {
            file: meta.file.clone(),
            suite: meta.suite_id.clone(),
            name: meta.title.clone(),
            line: None,
          },
          test_fn: Arc::new({
            let requested_fixtures_for_test = requested_fixtures.clone();
            move |pool| {
              let cb = Arc::clone(&cb);
              let bcfg = bcfg.clone();
              let requested_fixtures = requested_fixtures_for_test.clone();
              Box::pin(async move {
                let test_info: Arc<ferridriver_test::model::TestInfo> = pool
                  .get("test_info")
                  .await
                  .map_err(|e| TestFailure::wrap("fixture 'test_info'", e))?;

                let modifiers = Arc::new(ferridriver_test::model::TestModifiers::default());
                pool.inject("__test_modifiers", Arc::clone(&modifiers));
                resolve_requested_fixtures(&pool, &requested_fixtures)
                  .await
                  .map_err(TestFailure::from)?;

                let fixtures = crate::test_fixtures::TestFixtures::from_pool(
                  pool.clone(),
                  Arc::clone(&test_info),
                  Arc::clone(&modifiers),
                  bcfg.clone(),
                );

                call_js_test(&cb, fixtures).await.map_err(TestFailure::from)
              })
            }
          }),
          fixture_requests: requested_fixtures,
          expected_status: ExpectedStatus::Pass,
          annotations,
          timeout: meta.timeout.map(|t| std::time::Duration::from_millis(t as u64)),
          retries: meta.retries.map(|r| r as u32),
          use_options: meta.use_options.clone(),
        }
      })
      .collect();

    let has_e2e = !test_cases.is_empty();
    drop(tests); // Release lock before building plan.

    // ── Build TestPlan via core builder (grouping + hook wiring in Rust core) ──
    let mut builder = ferridriver_test::TestPlanBuilder::new();

    for tc in test_cases {
      builder.add_test(tc);
    }

    // Register suites.
    let suites_reg = self.suites.lock().await;
    for s in suites_reg.iter() {
      builder.add_suite(ferridriver_test::SuiteDef {
        id: s.id.clone(),
        name: s.meta.name.clone(),
        file: s.meta.file.clone(),
        mode: match s.meta.mode.as_deref() {
          Some("serial") => SuiteMode::Serial,
          _ => SuiteMode::Parallel,
        },
      });
    }
    drop(suites_reg);

    // Register hooks — NAPI wraps JS callbacks into Rust hook fns, core handles association.
    let hooks_reg = self.hooks.lock().await;
    let registered_hooks = hooks_reg.clone();
    for h in &registered_hooks {
      let cb = Arc::clone(&h.callback);
      let bcfg = browser_config.clone();
      let requested_fixtures = h.meta.requested_fixtures.clone();
      let kind = match (h.meta.phase, h.meta.scope, &h.meta.owner) {
        (
          ferridriver_test::HookPhase::Before,
          ferridriver_test::HookScope::Suite,
          ferridriver_test::HookOwner::Suite(_),
        ) => ferridriver_test::HookKind::BeforeAll(make_suite_hook(cb, requested_fixtures.clone(), bcfg.clone())),
        (
          ferridriver_test::HookPhase::After,
          ferridriver_test::HookScope::Suite,
          ferridriver_test::HookOwner::Suite(_),
        ) => ferridriver_test::HookKind::AfterAll(make_suite_hook(cb, requested_fixtures, bcfg)),
        (
          ferridriver_test::HookPhase::Before,
          ferridriver_test::HookScope::Scenario,
          ferridriver_test::HookOwner::Suite(_),
        ) => ferridriver_test::HookKind::BeforeEach(make_each_hook(cb, requested_fixtures.clone(), bcfg)),
        (
          ferridriver_test::HookPhase::After,
          ferridriver_test::HookScope::Scenario,
          ferridriver_test::HookOwner::Suite(_),
        ) => ferridriver_test::HookKind::AfterEach(make_each_hook(cb, requested_fixtures, bcfg)),
        _ => continue,
      };
      builder.add_hook(ferridriver_test::HookDef {
        suite_id: match &h.meta.owner {
          ferridriver_test::HookOwner::Suite(suite_id) => suite_id.clone(),
          ferridriver_test::HookOwner::Root => String::new(),
        },
        kind,
      });
    }
    drop(hooks_reg);

    // ── BDD features ──
    let features = feature_files.or_else(|| {
      if self.config.features.is_empty() {
        None
      } else {
        Some(self.config.features.clone())
      }
    });

    let mut has_bdd = false;
    if let Some(patterns) = features {
      let mut registry = self.bdd.build_step_registry()?;
      register_bdd_hooks(&mut registry, &registered_hooks)?;

      let files = ferridriver_bdd::feature::FeatureSet::discover(&patterns, &self.config.test_ignore)
        .map_err(|e| napi::Error::from_reason(format!("feature discovery: {e}")))?;
      let feature_set =
        ferridriver_bdd::feature::FeatureSet::parse_with_language(files, self.config.language.as_deref())
          .map_err(|e| napi::Error::from_reason(format!("feature parse: {e}")))?;

      if !feature_set.features.is_empty() {
        let bdd_plan = ferridriver_bdd::translate::translate_features(&feature_set, Arc::new(registry), &self.config);
        for suite in bdd_plan.suites {
          for test in suite.tests {
            builder.add_test(test);
          }
        }
        has_bdd = true;
      }
    }

    if !has_e2e && !has_bdd {
      return Ok(RunSummary {
        total: 0,
        passed: 0,
        failed: 0,
        skipped: 0,
        flaky: 0,
        duration_ms: 0.0,
        exit_code: 0,
        results: Vec::new(),
      });
    }

    let plan = builder.build();

    // Build CLI overrides.
    let overrides = ferridriver_test::config::CliOverrides {
      last_failed: self.last_failed,
      grep: self.grep.clone(),
      ..Default::default()
    };

    // Set has_bdd for reporter selection.
    let mut config = self.config.clone();
    if has_bdd {
      config.has_bdd = true;
    }

    // Run on the NAPI tokio runtime directly. The #[napi] async method yields
    // via .await, freeing the JS main thread to process TSFN callbacks from workers.
    // No separate runtime needed — workers use tokio::spawn which runs on this runtime.
    let collector = Arc::new(tokio::sync::Mutex::new(ResultCollector::new()));
    let start = Instant::now();
    let watch = self.watch;
    let collector_clone = Arc::clone(&collector);

    // Drain function-form global setup/teardown hooks and wrap each into a
    // SuiteHookFn that ignores its FixturePool argument (the JS function
    // signature is `() => Promise<void>`).
    let mut hooks = ferridriver_test::model::TestHooks::default();
    {
      let mut setups = self.global_setup_fns.lock().await;
      for tsfn in std::mem::take(&mut *setups) {
        hooks.global_setup_fns.push(make_global_hook(tsfn));
      }
    }
    {
      let mut teardowns = self.global_teardown_fns.lock().await;
      for tsfn in std::mem::take(&mut *teardowns) {
        hooks.global_teardown_fns.push(make_global_hook(tsfn));
      }
    }

    let exit_code = {
      let mut runner = ferridriver_test::runner::TestRunner::with_hooks(config, hooks, overrides);
      runner.add_reporter(Box::new(ResultCollectorReporter(collector_clone)));

      // Drain any TS-authored reporters registered via
      // `register_js_reporter` and append them to the runner. Drained
      // (not cloned) so a TestRunner instance reused across runs gets
      // a clean slate — register_js_reporter must be called per run.
      {
        let mut js_reporters = self.js_reporters.lock().await;
        for jr in std::mem::take(&mut *js_reporters) {
          runner.add_reporter(Box::new(jr));
        }
      }

      if watch {
        let cwd = std::env::current_dir().unwrap_or_default();
        Box::pin(runner.run_watch(move |_changed| plan.clone(), cwd)).await
      } else {
        runner.run(plan).await
      }
    };

    let duration = start.elapsed();

    // Collect results from the reporter.
    let collected = collector.lock().await;
    let results: Vec<TestResultItem> = collected.results.clone();

    // Prefer the runner's RunFinished totals when present — they
    // already account for retry-flake collapsing. Fall back to per-
    // attempt sums for paths that don't emit RunFinished (e.g. early
    // exits before the run started).
    let (total, passed, failed, skipped, flaky, duration_ms) = if let Some(t) = &collected.run_totals {
      (
        t.total as i32,
        t.passed as i32,
        t.failed as i32,
        t.skipped as i32,
        t.flaky as i32,
        t.duration.as_secs_f64() * 1000.0,
      )
    } else {
      let mut passed = 0i32;
      let mut failed = 0i32;
      let mut skipped = 0i32;
      let flaky = 0i32;
      for r in &results {
        match r.status.as_str() {
          "passed" => passed += 1,
          "skipped" => skipped += 1,
          _ => failed += 1,
        }
      }
      (
        results.len() as i32,
        passed,
        failed,
        skipped,
        flaky,
        duration.as_secs_f64() * 1000.0,
      )
    };

    Ok(RunSummary {
      total,
      passed,
      failed,
      skipped,
      flaky,
      duration_ms,
      exit_code,
      results,
    })
  }

  /// Get config accessors.
  #[napi]
  pub fn get_timeout(&self) -> f64 {
    self.config.timeout as f64
  }
  #[napi]
  pub fn get_expect_timeout(&self) -> f64 {
    self.config.expect_timeout as f64
  }
  #[napi]
  pub fn get_retries(&self) -> i32 {
    self.config.retries as i32
  }
  #[napi]
  pub fn worker_count(&self) -> i32 {
    self.config.workers as i32
  }
  #[napi]
  pub fn get_base_url(&self) -> Option<String> {
    self.config.base_url.clone()
  }

  // ── Browser config accessors (Playwright fixture equivalents) ──

  /// Browser name: "chromium", "firefox", "webkit".
  #[napi]
  pub fn get_browser_name(&self) -> String {
    self.config.browser.browser.clone()
  }

  /// Whether running headless.
  #[napi]
  pub fn get_headless(&self) -> bool {
    self.config.browser.headless
  }

  /// Browser channel (e.g. "chrome", "msedge").
  #[napi]
  pub fn get_channel(&self) -> Option<String> {
    self.config.browser.channel.clone()
  }

  /// Whether isMobile is set.
  #[napi]
  pub fn get_is_mobile(&self) -> bool {
    self.config.browser.use_options.is_mobile
  }

  /// Whether hasTouch is set.
  #[napi]
  pub fn get_has_touch(&self) -> bool {
    self.config.browser.use_options.has_touch
  }

  /// Color scheme: "light", "dark", or null.
  #[napi]
  pub fn get_color_scheme(&self) -> Option<String> {
    self.config.browser.use_options.color_scheme.clone()
  }

  /// Locale (e.g. "en-US").
  #[napi]
  pub fn get_locale(&self) -> Option<String> {
    self.config.browser.use_options.locale.clone()
  }

  /// Display name from config (Playwright top-level `name`).
  #[napi]
  pub fn get_name(&self) -> Option<String> {
    self.config.name.clone()
  }

  /// Path to a single tsconfig (Playwright top-level `tsconfig`).
  #[napi]
  pub fn get_tsconfig(&self) -> Option<String> {
    self.config.tsconfig.clone()
  }

  /// Whether `--ignore-snapshots` is in effect.
  #[napi]
  pub fn get_ignore_snapshots(&self) -> bool {
    self.config.ignore_snapshots
  }

  /// Whether `--pass-with-no-tests` is in effect.
  #[napi]
  pub fn get_pass_with_no_tests(&self) -> bool {
    self.config.pass_with_no_tests
  }

  /// Whole-suite timeout in ms (0 = unlimited).
  #[napi]
  pub fn get_global_timeout(&self) -> f64 {
    self.config.global_timeout as f64
  }

  /// `maxFailures` (0 = unlimited).
  #[napi]
  pub fn get_max_failures(&self) -> i32 {
    self.config.max_failures as i32
  }

  /// `repeatEach`.
  #[napi]
  pub fn get_repeat_each(&self) -> i32 {
    self.config.repeat_each as i32
  }

  /// Whether `-x` / fail-fast is in effect.
  #[napi]
  pub fn get_fail_fast(&self) -> bool {
    self.config.fail_fast
  }

  /// Discover test files.
  #[napi]
  pub fn discover_files(&self, root_dir: String) -> Result<Vec<String>> {
    ferridriver_test::discovery::find_test_files(&root_dir, &self.config.test_match, &self.config.test_ignore)
      .map_err(napi::Error::from_reason)
  }
}

/// Build a beforeEach/afterEach hook fn that wraps a JS callback.
fn make_each_hook(
  cb: Arc<TestCallbackFn>,
  requested_fixtures: Vec<String>,
  browser_config: ferridriver_test::config::BrowserConfig,
) -> ferridriver_test::model::HookFn {
  Arc::new(move |pool, test_info| {
    let cb = Arc::clone(&cb);
    let requested_fixtures = requested_fixtures.clone();
    let bcfg = browser_config.clone();
    Box::pin(async move {
      resolve_requested_fixtures(&pool, &requested_fixtures)
        .await
        .map_err(ferridriver_test::TestFailure::from)?;
      let modifiers = Arc::new(ferridriver_test::model::TestModifiers::default());
      let fixtures = crate::test_fixtures::TestFixtures::from_pool(pool.clone(), test_info, modifiers, bcfg.clone());
      call_js_test(&cb, fixtures)
        .await
        .map_err(ferridriver_test::TestFailure::from)
    })
  })
}

/// Wrap a nullary JS function (registered via `registerGlobalSetup` /
/// `registerGlobalTeardown`) into a SuiteHookFn the core runner can drive.
/// The pool argument is dropped -- global hooks don't have fixture access.
fn make_global_hook(tsfn: Arc<GlobalHookFn>) -> ferridriver_test::model::SuiteHookFn {
  Arc::new(move |_pool| {
    let tsfn = Arc::clone(&tsfn);
    Box::pin(async move {
      let result = match tsfn.call_async(()).await {
        Ok(promise) => promise.await.map_err(|e| format!("{e}")),
        Err(e) => Err(format!("{e}")),
      };
      result.map_err(|message| ferridriver_test::TestFailure {
        message,
        stack: None,
        diff: None,
        screenshot: None,
      })
    })
  })
}

fn make_suite_hook(
  cb: Arc<TestCallbackFn>,
  requested_fixtures: Vec<String>,
  browser_config: ferridriver_test::config::BrowserConfig,
) -> ferridriver_test::model::SuiteHookFn {
  Arc::new(move |pool| {
    let cb = Arc::clone(&cb);
    let requested_fixtures = requested_fixtures.clone();
    let bcfg = browser_config.clone();
    Box::pin(async move {
      resolve_requested_fixtures(&pool, &requested_fixtures)
        .await
        .map_err(ferridriver_test::TestFailure::from)?;

      let test_info = pool
        .get::<ferridriver_test::model::TestInfo>("test_info")
        .await
        .map_err(ferridriver_test::TestFailure::from)?;
      let modifiers = Arc::new(ferridriver_test::model::TestModifiers::default());
      let fixtures = crate::test_fixtures::TestFixtures::from_pool(pool.clone(), test_info, modifiers, bcfg.clone());
      call_js_test(&cb, fixtures)
        .await
        .map_err(ferridriver_test::TestFailure::from)
    })
  })
}

fn register_bdd_hooks(registry: &mut ferridriver_bdd::registry::StepRegistry, hooks: &[RegisteredHook]) -> Result<()> {
  for hook in hooks {
    if hook.meta.owner != ferridriver_test::HookOwner::Root {
      continue;
    }

    let Some(point) = ferridriver_bdd::hook::runtime_hook_point(&hook.meta) else {
      continue;
    };

    let (handler, tags) = match hook.meta.scope {
      ferridriver_test::HookScope::Suite | ferridriver_test::HookScope::Scenario => (
        ferridriver_bdd::hook::HookHandler::Scenario(make_bdd_scenario_hook(Arc::clone(&hook.callback))),
        hook.meta.tags.as_deref(),
      ),
      ferridriver_test::HookScope::Step => (
        ferridriver_bdd::hook::HookHandler::Step(make_bdd_step_hook(Arc::clone(&hook.callback))),
        hook.meta.tags.as_deref(),
      ),
    };

    let tag_filter = tags.and_then(|t| ferridriver_bdd::filter::TagExpression::parse(t).ok());

    registry.hooks_mut().register(ferridriver_bdd::hook::Hook {
      point,
      tag_filter,
      order: 0,
      handler,
      location: ferridriver_bdd::step::StepLocation {
        file: "<typescript>",
        line: 0,
      },
    });
  }

  Ok(())
}

/// Async callback that receives a mutable `BrowserWorld` reference — used for BDD scenario hooks.
type BddScenarioHookFn = dyn for<'a> Fn(
    &'a mut ferridriver_bdd::world::BrowserWorld,
  ) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = std::result::Result<(), ferridriver::FerriError>> + Send + 'a>,
  > + Send
  + Sync;

/// Async callback that receives a mutable `BrowserWorld` reference and step text — used for BDD step hooks.
type BddStepHookFn = dyn for<'a> Fn(
    &'a mut ferridriver_bdd::world::BrowserWorld,
    &'a str,
  ) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = std::result::Result<(), ferridriver::FerriError>> + Send + 'a>,
  > + Send
  + Sync;

fn make_bdd_scenario_hook(cb: Arc<TestCallbackFn>) -> Arc<BddScenarioHookFn> {
  Arc::new(move |world| {
    let cb = Arc::clone(&cb);
    let fixtures = fixtures_with_bdd_params(world, None, None, None);
    Box::pin(async move {
      let napi_fixtures = crate::test_fixtures::TestFixtures::from_resolved(fixtures);
      cb.call_async(napi_fixtures)
        .await
        .map_err(|e| ferridriver::FerriError::backend(e.to_string()))?
        .await
        .map_err(|e| ferridriver::FerriError::backend(e.to_string()))
    })
  })
}

fn make_bdd_step_hook(cb: Arc<TestCallbackFn>) -> Arc<BddStepHookFn> {
  Arc::new(move |world, _step_text| {
    let cb = Arc::clone(&cb);
    let fixtures = fixtures_with_bdd_params(world, None, None, None);
    Box::pin(async move {
      let napi_fixtures = crate::test_fixtures::TestFixtures::from_resolved(fixtures);
      cb.call_async(napi_fixtures)
        .await
        .map_err(|e| ferridriver::FerriError::backend(e.to_string()))?
        .await
        .map_err(|e| ferridriver::FerriError::backend(e.to_string()))
    })
  })
}

pub(crate) fn fixtures_with_bdd_params(
  world: &ferridriver_bdd::world::BrowserWorld,
  params: Option<&[ferridriver_bdd::step::StepParam]>,
  table: Option<&ferridriver_bdd::step::DataTable>,
  docstring: Option<&str>,
) -> ferridriver_test::model::TestFixtures {
  use ferridriver_bdd::step::StepParam;

  let mut fixtures = world.fixtures().clone();

  fixtures.bdd_args = params.map(|p| {
    p.iter()
      .map(|param| match param {
        StepParam::Int(i) => serde_json::Value::Number((*i).into()),
        StepParam::Float(f) => serde_json::json!(f),
        StepParam::String(s) | StepParam::Word(s) => serde_json::Value::String(s.clone()),
        StepParam::Custom { value, .. } => serde_json::Value::String(value.clone()),
      })
      .collect()
  });
  fixtures.bdd_data_table = table.map(|t| t.iter().cloned().collect());
  fixtures.bdd_doc_string = docstring.map(|s| s.to_string());

  fixtures
}

async fn resolve_requested_fixtures(
  pool: &ferridriver_test::fixture::FixturePool,
  requested_fixtures: &[String],
) -> ferridriver::error::Result<()> {
  for name in requested_fixtures {
    pool.resolve(name).await?;
  }
  Ok(())
}

/// Call a JS test callback with fixtures and await the returned Promise.
async fn call_js_test(
  tsfn: &TestCallbackFn,
  fixtures: crate::test_fixtures::TestFixtures,
) -> ferridriver::error::Result<()> {
  let t = std::time::Instant::now();
  let result = match tsfn.call_async(fixtures).await {
    Ok(promise) => promise
      .await
      .map_err(|e| ferridriver::FerriError::backend(e.to_string())),
    Err(e) => Err(ferridriver::FerriError::backend(e.to_string())),
  };
  tracing::debug!(target: "ferridriver::napi", elapsed_us = t.elapsed().as_micros() as u64, "call_js_test");
  result
}

/// In-memory result collector for returning test outcomes to TS.
struct ResultCollector {
  results: Vec<TestResultItem>,
  /// Aggregate counts captured from `RunFinished` so the NAPI summary
  /// surfaces the runner's flaky-detection result rather than a
  /// per-attempt sum.
  run_totals: Option<RunTotals>,
}

#[derive(Debug, Clone)]
struct RunTotals {
  total: usize,
  passed: usize,
  failed: usize,
  skipped: usize,
  flaky: usize,
  duration: std::time::Duration,
}

impl ResultCollector {
  fn new() -> Self {
    Self {
      results: Vec::new(),
      run_totals: None,
    }
  }
}

/// Reporter that collects results into a shared ResultCollector.
struct ResultCollectorReporter(Arc<tokio::sync::Mutex<ResultCollector>>);

#[async_trait::async_trait]
impl ferridriver_test::reporter::Reporter for ResultCollectorReporter {
  async fn on_event(&mut self, event: &ferridriver_test::reporter::ReporterEvent) {
    match event {
      ferridriver_test::reporter::ReporterEvent::TestFinished { test_id, outcome } => {
        let status = match outcome.status {
          ferridriver_test::model::TestStatus::Passed => "passed",
          ferridriver_test::model::TestStatus::Failed => "failed",
          ferridriver_test::model::TestStatus::TimedOut => "timed out",
          ferridriver_test::model::TestStatus::Skipped => "skipped",
          ferridriver_test::model::TestStatus::Flaky => "flaky",
          ferridriver_test::model::TestStatus::Interrupted => "interrupted",
        };
        let mut collector = self.0.lock().await;
        collector.results.push(TestResultItem {
          id: test_id.full_name(),
          title: test_id.name.clone(),
          status: status.into(),
          duration_ms: outcome.duration.as_secs_f64() * 1000.0,
          attempt: outcome.attempt as i32,
          error_message: outcome.error.as_ref().map(|e| e.message.clone()),
        });
      },
      ferridriver_test::reporter::ReporterEvent::RunFinished {
        total,
        passed,
        failed,
        skipped,
        flaky,
        duration,
      } => {
        let mut collector = self.0.lock().await;
        collector.run_totals = Some(RunTotals {
          total: *total,
          passed: *passed,
          failed: *failed,
          skipped: *skipped,
          flaky: *flaky,
          duration: *duration,
        });
      },
      _ => {},
    }
  }

  async fn finalize(&mut self) -> ferridriver::error::Result<()> {
    Ok(())
  }
}
