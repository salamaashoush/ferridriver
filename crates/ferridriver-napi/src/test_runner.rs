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
  /// Annotations — same as Rust TestAnnotation, deserialized from JSON.
  pub annotations: Vec<serde_json::Value>,
  /// Fixture overrides from test.use() — merged with global config by the worker.
  pub use_options: Option<serde_json::Value>,
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
struct RegisteredHook {
  meta: HookMeta,
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
  tests: Mutex<Vec<RegisteredTest>>,
  suites: Mutex<Vec<RegisteredSuite>>,
  hooks: Mutex<Vec<RegisteredHook>>,
}

#[napi]
impl TestRunner {
  /// Create a new test runner.
  #[napi(factory)]
  pub async fn create(config: Option<TestRunnerConfig>) -> Result<Self> {
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
    let tc = ferridriver_test::config::resolve_config(&overrides)
      .map_err(|e| napi::Error::new(Status::GenericFailure, e))?;

    Ok(Self {
      config: tc,
      last_failed: cfg.last_failed.unwrap_or(false),
      watch: cfg.watch.unwrap_or(false),
      grep: cfg.grep.clone(),
      tests: Mutex::new(Vec::new()),
      suites: Mutex::new(Vec::new()),
      hooks: Mutex::new(Vec::new()),
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
    hooks.push(RegisteredHook {
      meta,
      callback: Arc::new(tsfn),
    });
    Ok(())
  }

  /// Run all registered tests through the core TestRunner pipeline.
  ///
  /// Converts registered JS tests into a TestPlan, delegates to the core runner
  /// for browser launch, parallel dispatch, retries, filtering, and reporting.
  #[napi]
  #[allow(clippy::too_many_lines)]
  pub async fn run(&self) -> Result<RunSummary> {
    use ferridriver_test::model::*;

    let tests = self.tests.lock().await;

    if tests.is_empty() {
      return Ok(RunSummary {
        total: 0,
        passed: 0,
        failed: 0,
        skipped: 0,
        flaky: 0,
        duration_ms: 0.0,
        results: Vec::new(),
      });
    }

    // Convert registered JS tests into core TestCase objects.
    let browser_config = self.config.browser.clone();
    let test_cases: Vec<TestCase> = tests
      .iter()
      .map(|t| {
        let cb = Arc::clone(&t.callback);
        let meta = t.meta.clone();
        let bcfg = browser_config.clone();

        // Deserialize annotations from JSON — same type as Rust core.
        let annotations: Vec<TestAnnotation> = meta
          .annotations
          .iter()
          .filter_map(|v| serde_json::from_value::<TestAnnotation>(v.clone()).ok())
          .collect();

        TestCase {
          id: TestId {
            file: meta.file.clone(),
            suite: meta.suite_id.clone(),
            name: meta.title.clone(),
            line: None,
          },
          test_fn: Arc::new(move |pool| {
            let cb = Arc::clone(&cb);
            let bcfg = bcfg.clone();
            Box::pin(async move {
              // Pull ALL fixtures from the pool (created by the core worker).
              let page: Arc<ferridriver::Page> =
                pool.get("page").await.map_err(|e| TestFailure::from(format!("fixture 'page': {e}")))?;
              let context: Arc<ferridriver::context::ContextRef> =
                pool.get("context").await.map_err(|e| TestFailure::from(format!("fixture 'context': {e}")))?;
              let test_info: Arc<ferridriver_test::model::TestInfo> =
                pool.get("test_info").await.map_err(|e| TestFailure::from(format!("fixture 'test_info': {e}")))?;
              let request: Arc<ferridriver::api_request::APIRequestContext> =
                pool.get("request").await.map_err(|e| TestFailure::from(format!("fixture 'request': {e}")))?;

              // Create shared modifiers — worker reads these after callback returns.
              let modifiers = Arc::new(ferridriver_test::model::TestModifiers::default());
              pool.inject("__test_modifiers", Arc::clone(&modifiers)).await;

              // Build the TestFixtures object for JS.
              let fixtures = crate::test_fixtures::TestFixtures::new(
                (*page).clone(),
                (*context).clone(),
                request,
                Arc::clone(&test_info),
                Arc::clone(&modifiers),
                bcfg.browser.clone(),
                bcfg.headless,
                bcfg.context.is_mobile,
                bcfg.context.has_touch,
                bcfg.context.color_scheme.clone(),
                bcfg.context.locale.clone(),
                bcfg.channel.clone(),
              );

              // Call JS callback with full fixtures.
              call_js_test(&cb, fixtures).await.map_err(|e| TestFailure {
                message: e,
                stack: None,
                diff: None,
                screenshot: None,
              })
            })
          }),
          fixture_requests: vec![
            "browser".into(),
            "context".into(),
            "page".into(),
            "test_info".into(),
            "request".into(),
          ],
          expected_status: ExpectedStatus::Pass,
          annotations,
          timeout: meta.timeout.map(|t| std::time::Duration::from_millis(t as u64)),
          retries: meta.retries.map(|r| r as u32),
          use_options: meta.use_options.clone(),
        }
      })
      .collect();

    let total = test_cases.len();
    drop(tests); // Release lock before running.

    // Build TestPlan.
    let plan = TestPlan {
      suites: vec![TestSuite {
        name: "tests".into(),
        file: "".into(),
        tests: test_cases,
        hooks: Hooks::default(),
        annotations: Vec::new(),
        mode: SuiteMode::Parallel,
      }],
      total_tests: total,
      shard: None,
    };

    // Build CLI overrides.
    let overrides = ferridriver_test::config::CliOverrides {
      last_failed: self.last_failed,
      grep: self.grep.clone(),
      ..Default::default()
    };

    // Run through core pipeline on a SEPARATE Tokio runtime.
    //
    // Critical: the core runner uses `tokio::spawn` for parallel workers, and those
    // workers call TSFN callbacks back to the JS main thread. If we run on the NAPI
    // runtime, the JS thread blocks waiting for `run()` to resolve, but the TSFN
    // callbacks need the JS thread → deadlock.
    //
    // Solution: run the core pipeline on a dedicated multi-thread runtime spawned on
    // a blocking thread. The NAPI async method yields (`spawn_blocking().await`),
    // freeing the JS main thread to process TSFN callbacks.
    let collector = Arc::new(tokio::sync::Mutex::new(ResultCollector::new()));
    let start = Instant::now();
    let config = self.config.clone();
    let watch = self.watch;
    let collector_clone = Arc::clone(&collector);

    tokio::task::spawn_blocking(move || {
      let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build test runner tokio runtime");

      rt.block_on(async move {
        let mut runner = ferridriver_test::runner::TestRunner::new(config, overrides);
        runner.add_reporter(Box::new(ResultCollectorReporter(collector_clone)));

        if watch {
          let cwd = std::env::current_dir().unwrap_or_default();
          let _exit_code = runner.run_watch(move |_changed| plan.clone(), cwd).await;
        } else {
          let _exit_code = runner.run(plan).await;
        }
      });
    })
    .await
    .map_err(|e| napi::Error::from_reason(format!("test runner task failed: {e}")))?;

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

/// Call a JS test callback with fixtures and await the returned Promise.
async fn call_js_test(
  tsfn: &TestCallbackFn,
  fixtures: crate::test_fixtures::TestFixtures,
) -> std::result::Result<(), String> {
  // ThreadsafeFunction::call_async sends TestFixtures to the JS thread,
  // calls the callback, and returns the result as a Future.
  match tsfn.call_async(fixtures).await {
    Ok(promise) => promise.await.map_err(|e| format!("{e}")),
    Err(e) => Err(format!("{e}")),
  }
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
