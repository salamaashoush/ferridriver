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

/// Test runner configuration from TypeScript.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct TestRunnerConfig {
  pub workers: Option<i32>,
  pub timeout: Option<f64>,
  pub retries: Option<i32>,
  pub headed: Option<bool>,
  /// Browser name: "chromium" (default), "firefox", "webkit".
  pub browser: Option<String>,
  /// Backend protocol: "cdp-pipe", "cdp-raw", "bidi", "webkit".
  pub backend: Option<String>,
  /// Browser channel: "chrome", "chrome-beta", "msedge", etc.
  pub channel: Option<String>,
  pub executable_path: Option<String>,
  pub browser_args: Option<Vec<String>>,
  pub base_url: Option<String>,
  pub reporter: Option<Vec<String>>,
  pub output_dir: Option<String>,
  pub test_match: Option<Vec<String>>,
  pub viewport_width: Option<i32>,
  pub viewport_height: Option<i32>,
  pub forbid_only: Option<bool>,
  pub last_failed: Option<bool>,
  /// Verbose logging level: 0=off, 1=debug, 2=trace
  pub verbose: Option<i32>,
  /// Debug categories (e.g. "cdp", "steps", "cdp,action"). Same as FERRIDRIVER_DEBUG env var.
  pub debug: Option<String>,
  /// Grep pattern to filter tests by name.
  pub grep: Option<String>,
  /// Video recording mode: "off", "on", "retain-on-failure".
  pub video: Option<String>,
  /// Trace recording mode: "off", "on", "retain-on-failure", "on-first-retry".
  pub trace: Option<String>,
  /// Path to storage state JSON file (pre-authenticated session).
  pub storage_state: Option<String>,
  /// Watch mode: re-run tests on file changes.
  pub watch: Option<bool>,
  // ── Context options (Playwright `use` block) ──
  /// Simulate mobile device (isMobile). Condition: "mobile".
  pub is_mobile: Option<bool>,
  /// Enable touch events (hasTouch). Condition: "touch".
  pub has_touch: Option<bool>,
  /// Color scheme: "light", "dark", "no-preference". Condition: "dark", "light".
  pub color_scheme: Option<String>,
  /// Browser locale (e.g. "en-US", "de-DE").
  pub locale: Option<String>,
  /// Simulate offline mode. Condition: "offline".
  pub offline: Option<bool>,
  /// Bypass CSP. Condition: "bypass-csp".
  pub bypass_csp: Option<bool>,
  /// Project configurations — matches Playwright's `projects` array.
  pub projects: Option<Vec<NapiProjectConfig>>,

  // ── BDD config ──
  /// Tag filter expression (BDD).
  pub tags: Option<String>,
  /// Strict mode: undefined/pending steps cause failure (BDD).
  pub strict: Option<bool>,
  /// Execution order: "defined" | "random" | "random:SEED" (BDD).
  pub order: Option<String>,
  /// Gherkin keyword language (BDD).
  pub language: Option<String>,
  /// Feature file glob patterns (BDD).
  pub features: Option<Vec<String>>,
  /// Screenshot on failure.
  pub screenshot_on_failure: Option<bool>,

  // ── Web server ──
  pub web_server: Option<Vec<NapiWebServerConfig>>,
}

/// Web server config passed from TS — maps to Rust `WebServerConfig`.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct NapiWebServerConfig {
  pub command: Option<String>,
  pub static_dir: Option<String>,
  pub url: Option<String>,
  pub port: Option<i32>,
  pub timeout: Option<f64>,
  pub cwd: Option<String>,
}

/// Project config passed from TS — maps to Rust `ProjectConfig`.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct NapiProjectConfig {
  pub name: String,
  pub test_match: Option<Vec<String>>,
  pub test_ignore: Option<Vec<String>>,
  pub test_dir: Option<String>,
  pub grep: Option<String>,
  pub grep_invert: Option<String>,
  pub timeout: Option<f64>,
  pub retries: Option<i32>,
  pub repeat_each: Option<i32>,
  pub fully_parallel: Option<bool>,
  pub output_dir: Option<String>,
  pub snapshot_dir: Option<String>,
  pub dependencies: Option<Vec<String>>,
  pub teardown: Option<String>,
  pub tag: Option<Vec<String>>,
  /// Browser/context config (Playwright's `use` block).
  pub browser: Option<String>,
  pub backend: Option<String>,
  pub channel: Option<String>,
  pub headed: Option<bool>,
  pub viewport_width: Option<i32>,
  pub viewport_height: Option<i32>,
  pub is_mobile: Option<bool>,
  pub has_touch: Option<bool>,
  pub color_scheme: Option<String>,
  pub locale: Option<String>,
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
  /// Optional list of fixture names this test actually uses (e.g. ["page"]).
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
  // BDD state
  bdd: BddRegistry,
}

#[napi]
impl TestRunner {
  /// Create a new test runner.
  /// Sync factory -- no async work needed. Avoids NAPI async overhead (~60ms thread hop).
  #[napi(factory)]
  pub fn create(config: Option<TestRunnerConfig>) -> Result<Self> {
    let cfg = config.unwrap_or_default();
    // Inject debug config as env var so the centralized logging picks it up.
    // This works around Bun not propagating process.env to std::env::var.
    if let Some(ref debug) = cfg.debug {
      #[allow(unused_unsafe)]
      unsafe {
        std::env::set_var("FERRIDRIVER_DEBUG", debug);
      }
    }
    let verbose = cfg.verbose.unwrap_or(0) as u8;
    if verbose > 0 {
      ferridriver_test::logging::init(verbose);
    } else {
      ferridriver_test::logging::init_from_env();
    }
    // Map NAPI config → CliOverrides and use the single resolve_config() path.
    // This ensures env vars, normalization, and worker auto-detection all work.
    let overrides = ferridriver_test::config::CliOverrides {
      workers: cfg.workers.map(|w| w as u32),
      timeout: cfg.timeout.map(|t| t as u64),
      retries: cfg.retries.map(|r| r as u32),
      headed: cfg.headed.unwrap_or(false),
      browser: cfg.browser.clone(),
      backend: cfg.backend.clone(),
      channel: cfg.channel.clone(),
      executable_path: cfg.executable_path.clone(),
      browser_args: cfg.browser_args.clone().unwrap_or_default(),
      base_url: cfg.base_url.clone(),
      reporter: cfg.reporter.clone().unwrap_or_default(),
      output_dir: cfg.output_dir.clone(),
      test_match: cfg.test_match.clone(),
      viewport_width: cfg.viewport_width.map(|v| v as i64),
      viewport_height: cfg.viewport_height.map(|v| v as i64),
      forbid_only: cfg.forbid_only.unwrap_or(false),
      video: cfg.video.clone(),
      trace: cfg.trace.clone(),
      storage_state: cfg.storage_state.clone(),
      is_mobile: cfg.is_mobile,
      has_touch: cfg.has_touch,
      color_scheme: cfg.color_scheme.clone(),
      locale: cfg.locale.clone(),
      offline: cfg.offline,
      bypass_csp: cfg.bypass_csp,
      ..Default::default()
    };
    let mut tc =
      ferridriver_test::config::resolve_config(&overrides).map_err(|e| napi::Error::new(Status::GenericFailure, e))?;

    // Wire NAPI projects into Rust config.
    if let Some(ref napi_projects) = cfg.projects {
      tc.projects = napi_projects.iter().map(napi_project_to_rust).collect();
    }

    // BDD overrides.
    if let Some(ref t) = cfg.tags {
      tc.tags = Some(t.clone());
    }
    if let Some(s) = cfg.strict {
      tc.strict = s;
    }
    if let Some(ref o) = cfg.order {
      tc.order.clone_from(o);
    }
    if let Some(ref l) = cfg.language {
      tc.language = Some(l.clone());
    }
    if let Some(ref f) = cfg.features {
      tc.features.clone_from(f);
    }
    if let Some(ss) = cfg.screenshot_on_failure {
      tc.screenshot_on_failure = ss;
    }

    // Web server config.
    if let Some(ref servers) = cfg.web_server {
      tc.web_server = servers.iter().map(napi_web_server_to_rust).collect();
    }

    Ok(Self {
      config: tc,
      last_failed: cfg.last_failed.unwrap_or(false),
      watch: cfg.watch.unwrap_or(false),
      grep: cfg.grep.clone(),
      tests: Mutex::new(Vec::new()),
      suites: Mutex::new(Vec::new()),
      hooks: Mutex::new(Vec::new()),
      bdd: BddRegistry::new(),
    })
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

  /// Run all registered tests (E2E + BDD) through the core TestRunner pipeline.
  ///
  /// Converts registered JS tests into a TestPlan, optionally adds BDD feature
  /// tests, delegates to the core runner for browser launch, parallel dispatch,
  /// retries, filtering, and reporting.
  #[napi]
  #[allow(clippy::too_many_lines)]
  pub async fn run(&self, feature_files: Option<Vec<String>>) -> Result<RunSummary> {
    use ferridriver_test::model::*;

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
                  .map_err(|e| TestFailure::from(format!("fixture 'test_info': {e}")))?;

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

                call_js_test(&cb, fixtures).await.map_err(|e| TestFailure {
                  message: e,
                  stack: None,
                  diff: None,
                  screenshot: None,
                })
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

    let exit_code = {
      let mut runner = ferridriver_test::runner::TestRunner::new(config, overrides);
      runner.add_reporter(Box::new(ResultCollectorReporter(collector_clone)));

      if watch {
        let cwd = std::env::current_dir().unwrap_or_default();
        runner.run_watch(move |_changed| plan.clone(), cwd).await
      } else {
        runner.run(plan).await
      }
    };

    let duration = start.elapsed();

    // Collect results from the reporter.
    let collected = collector.lock().await;
    let mut passed = 0i32;
    let mut failed = 0i32;
    let mut skipped = 0i32;
    let mut flaky = 0i32;
    let mut results = Vec::new();

    for r in &collected.results {
      match r.status.as_str() {
        "passed" => passed += 1,
        "skipped" => skipped += 1,
        "flaky" => {
          flaky += 1;
          passed += 1;
        },
        _ => failed += 1,
      }
      results.push(r.clone());
    }

    Ok(RunSummary {
      total: results.len() as i32,
      passed,
      failed,
      skipped,
      flaky,
      duration_ms: duration.as_secs_f64() * 1000.0,
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
    self.config.browser.context.is_mobile
  }

  /// Whether hasTouch is set.
  #[napi]
  pub fn get_has_touch(&self) -> bool {
    self.config.browser.context.has_touch
  }

  /// Color scheme: "light", "dark", or null.
  #[napi]
  pub fn get_color_scheme(&self) -> Option<String> {
    self.config.browser.context.color_scheme.clone()
  }

  /// Locale (e.g. "en-US").
  #[napi]
  pub fn get_locale(&self) -> Option<String> {
    self.config.browser.context.locale.clone()
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
        .map_err(|e| ferridriver_test::TestFailure {
          message: e,
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
        .map_err(|e| ferridriver_test::TestFailure {
          message: e,
          stack: None,
          diff: None,
          screenshot: None,
        })
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

fn make_bdd_scenario_hook(
  cb: Arc<TestCallbackFn>,
) -> Arc<
  dyn for<'a> Fn(
      &'a mut ferridriver_bdd::world::BrowserWorld,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = std::result::Result<(), String>> + Send + 'a>>
    + Send
    + Sync,
> {
  Arc::new(move |world| {
    let cb = Arc::clone(&cb);
    let fixtures = fixtures_with_bdd_params(world, None, None, None);
    Box::pin(async move {
      let napi_fixtures = crate::test_fixtures::TestFixtures::from_resolved(fixtures);
      cb.call_async(napi_fixtures)
        .await
        .map_err(|e| format!("{e}"))?
        .await
        .map_err(|e| format!("{e}"))
    })
  })
}

fn make_bdd_step_hook(
  cb: Arc<TestCallbackFn>,
) -> Arc<
  dyn for<'a> Fn(
      &'a mut ferridriver_bdd::world::BrowserWorld,
      &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = std::result::Result<(), String>> + Send + 'a>>
    + Send
    + Sync,
> {
  Arc::new(move |world, _step_text| {
    let cb = Arc::clone(&cb);
    let fixtures = fixtures_with_bdd_params(world, None, None, None);
    Box::pin(async move {
      let napi_fixtures = crate::test_fixtures::TestFixtures::from_resolved(fixtures);
      cb.call_async(napi_fixtures)
        .await
        .map_err(|e| format!("{e}"))?
        .await
        .map_err(|e| format!("{e}"))
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
  fixtures.bdd_data_table = table.map(|t| t.iter().map(|r| r.clone()).collect());
  fixtures.bdd_doc_string = docstring.map(|s| s.to_string());

  fixtures
}

async fn resolve_requested_fixtures(
  pool: &ferridriver_test::fixture::FixturePool,
  requested_fixtures: &[String],
) -> std::result::Result<(), String> {
  for name in requested_fixtures {
    pool.resolve(name).await?;
  }
  Ok(())
}

/// Call a JS test callback with fixtures and await the returned Promise.
async fn call_js_test(
  tsfn: &TestCallbackFn,
  fixtures: crate::test_fixtures::TestFixtures,
) -> std::result::Result<(), String> {
  let t = std::time::Instant::now();
  let result = match tsfn.call_async(fixtures).await {
    Ok(promise) => promise.await.map_err(|e| format!("{e}")),
    Err(e) => Err(format!("{e}")),
  };
  tracing::debug!(target: "ferridriver::napi", elapsed_us = t.elapsed().as_micros() as u64, "call_js_test");
  result
}

/// In-memory result collector for returning test outcomes to TS.
struct ResultCollector {
  results: Vec<TestResultItem>,
}

impl ResultCollector {
  fn new() -> Self {
    Self { results: Vec::new() }
  }
}

/// Reporter that collects results into a shared ResultCollector.
struct ResultCollectorReporter(Arc<tokio::sync::Mutex<ResultCollector>>);

#[async_trait::async_trait]
impl ferridriver_test::reporter::Reporter for ResultCollectorReporter {
  async fn on_event(&mut self, event: &ferridriver_test::reporter::ReporterEvent) {
    if let ferridriver_test::reporter::ReporterEvent::TestFinished { test_id, outcome } = event {
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
    }
  }

  async fn finalize(&mut self) -> std::result::Result<(), String> {
    Ok(())
  }
}

/// Convert NAPI project config to Rust `ProjectConfig`.
fn napi_project_to_rust(napi: &NapiProjectConfig) -> ferridriver_test::config::ProjectConfig {
  let browser_config = if napi.browser.is_some()
    || napi.backend.is_some()
    || napi.channel.is_some()
    || napi.headed.is_some()
    || napi.viewport_width.is_some()
    || napi.is_mobile.is_some()
    || napi.has_touch.is_some()
    || napi.color_scheme.is_some()
    || napi.locale.is_some()
  {
    let mut bc = ferridriver_test::config::BrowserConfig::default();
    if let Some(ref b) = napi.browser {
      bc.browser.clone_from(b);
    }
    if let Some(ref b) = napi.backend {
      bc.backend.clone_from(b);
    }
    if let Some(ref ch) = napi.channel {
      bc.channel = Some(ch.clone());
    }
    if let Some(headed) = napi.headed {
      bc.headless = !headed;
    }
    if let Some(w) = napi.viewport_width {
      if let Some(h) = napi.viewport_height {
        bc.viewport = Some(ferridriver_test::config::ViewportConfig {
          width: w as i64,
          height: h as i64,
        });
      }
    }
    if let Some(m) = napi.is_mobile {
      bc.context.is_mobile = m;
    }
    if let Some(t) = napi.has_touch {
      bc.context.has_touch = t;
    }
    if let Some(ref cs) = napi.color_scheme {
      bc.context.color_scheme = Some(cs.clone());
    }
    if let Some(ref l) = napi.locale {
      bc.context.locale = Some(l.clone());
    }
    Some(bc)
  } else {
    None
  };

  ferridriver_test::config::ProjectConfig {
    name: napi.name.clone(),
    test_match: napi.test_match.clone(),
    test_ignore: napi.test_ignore.clone(),
    test_dir: napi.test_dir.clone(),
    browser: browser_config,
    output_dir: napi.output_dir.clone(),
    snapshot_dir: napi.snapshot_dir.clone(),
    retries: napi.retries.map(|r| r as u32),
    timeout: napi.timeout.map(|t| t as u64),
    repeat_each: napi.repeat_each.map(|r| r as u32),
    fully_parallel: napi.fully_parallel,
    grep: napi.grep.clone(),
    grep_invert: napi.grep_invert.clone(),
    dependencies: napi.dependencies.clone().unwrap_or_default(),
    teardown: napi.teardown.clone(),
    tag: napi.tag.clone(),
    ..Default::default()
  }
}

/// Convert NAPI web server config to Rust `WebServerConfig`.
fn napi_web_server_to_rust(napi: &NapiWebServerConfig) -> ferridriver_test::config::WebServerConfig {
  ferridriver_test::config::WebServerConfig {
    command: napi.command.clone(),
    static_dir: napi.static_dir.clone(),
    url: napi.url.clone(),
    port: napi.port.map(|p| p as u16).unwrap_or(0),
    timeout: napi.timeout.map(|t| t as u64).unwrap_or(30000),
    cwd: napi.cwd.clone(),
    ..Default::default()
  }
}
