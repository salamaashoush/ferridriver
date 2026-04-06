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
use std::time::{Duration, Instant};

use napi::Result;
use napi_derive::napi;
use napi::Status;
use napi::threadsafe_function::ThreadsafeFunction;
use tokio::sync::Mutex;

/// Test callback TSFN type — async JS function receiving a Page, returning Promise<void>.
/// callee_handled=false (modern async), weak=true (doesn't block Node exit), unbounded queue.
/// Return type is Promise<()> because JS test bodies are async functions.
type TestCallbackFn = ThreadsafeFunction<crate::page::Page, napi::bindgen_prelude::Promise<()>, crate::page::Page, Status, false, true, 0>;

/// Test runner configuration from TypeScript.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct TestRunnerConfig {
  pub workers: Option<i32>,
  pub timeout: Option<f64>,
  pub retries: Option<i32>,
  pub headed: Option<bool>,
  pub backend: Option<String>,
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
      unsafe { std::env::set_var("FERRIDRIVER_DEBUG", debug); }
    }
    let verbose = cfg.verbose.unwrap_or(0) as u8;
    if verbose > 0 {
      ferridriver_test::logging::init(verbose);
    } else {
      ferridriver_test::logging::init_from_env();
    }
    let mut tc = ferridriver_test::TestConfig::default();

    if let Some(t) = cfg.timeout { tc.timeout = crate::types::f64_to_u64(t); }
    if let Some(w) = cfg.workers { tc.workers = w as u32; }
    if let Some(r) = cfg.retries { tc.retries = r as u32; }
    if let Some(headed) = cfg.headed { tc.browser.headless = !headed; }
    if let Some(ref b) = cfg.backend { tc.browser.backend.clone_from(b); }
    if let Some(ref p) = cfg.executable_path { tc.browser.executable_path = Some(p.clone()); }
    if let Some(ref args) = cfg.browser_args { tc.browser.args.clone_from(args); }
    if let Some(ref r) = cfg.reporter {
      tc.reporter = r.iter().map(|name| ferridriver_test::config::ReporterConfig {
        name: name.clone(), options: Default::default(),
      }).collect();
    }
    if let Some(ref url) = cfg.base_url { tc.base_url = Some(url.clone()); }
    if let Some(ref dir) = cfg.output_dir { tc.output_dir = dir.into(); }
    if let Some(ref patterns) = cfg.test_match { tc.test_match.clone_from(patterns); }
    if let Some(w) = cfg.viewport_width {
      if let Some(ref mut vp) = tc.browser.viewport { vp.width = w as i64; }
    }
    if let Some(h) = cfg.viewport_height {
      if let Some(ref mut vp) = tc.browser.viewport { vp.height = h as i64; }
    }
    if let Some(fo) = cfg.forbid_only { tc.forbid_only = fo; }
    if tc.workers == 0 {
      let cpus = std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(4);
      tc.workers = (cpus / 2).max(1);
    }

    Ok(Self {
      config: tc,
      last_failed: cfg.last_failed.unwrap_or(false),
      grep: cfg.grep.clone(),
      tests: Mutex::new(Vec::new()),
      suites: Mutex::new(Vec::new()),
      hooks: Mutex::new(Vec::new()),
    })
  }

  /// Register a test. The callback receives a Page and should return a Promise.
  /// Called from TS after loading each test file.
  #[napi(ts_args_type = "meta: TestMeta, callback: (page: Page) => Promise<void>")]
  pub fn register_test(
    &self,
    meta: TestMeta,
    callback: napi::bindgen_prelude::Function<'_, crate::page::Page, napi::bindgen_prelude::Promise<()>>,
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
    let mut tests = self.tests.try_lock()
      .map_err(|_| napi::Error::from_reason("tests lock contended during registration"))?;
    tests.push(RegisteredTest { meta, callback: Arc::new(tsfn) });
    Ok(())
  }

  /// Register a test suite (describe block). Returns a suite ID for test/hook registration.
  #[napi]
  pub fn register_suite(&self, meta: SuiteMeta) -> Result<String> {
    let id = format!("{}::{}", meta.file, meta.name);
    let mut suites = self.suites.try_lock()
      .map_err(|_| napi::Error::from_reason("suites lock contended"))?;
    suites.push(RegisteredSuite { meta, id: id.clone() });
    Ok(id)
  }

  /// Register a lifecycle hook for a suite.
  #[napi(ts_args_type = "meta: HookMeta, callback: (page: Page) => Promise<void>")]
  pub fn register_hook(
    &self,
    meta: HookMeta,
    callback: napi::bindgen_prelude::Function<'_, crate::page::Page, napi::bindgen_prelude::Promise<()>>,
  ) -> Result<()> {
    let tsfn = callback
      .build_threadsafe_function()
      .callee_handled::<false>()
      .weak::<true>()
      .max_queue_size::<0>()
      .build()?;

    let mut hooks = self.hooks.try_lock()
      .map_err(|_| napi::Error::from_reason("hooks lock contended"))?;
    hooks.push(RegisteredHook { meta, callback: Arc::new(tsfn) });
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
        total: 0, passed: 0, failed: 0, skipped: 0, flaky: 0,
        duration_ms: 0.0, results: Vec::new(),
      });
    }

    // Convert registered JS tests into core TestCase objects.
    let test_cases: Vec<TestCase> = tests
      .iter()
      .map(|t| {
        let cb = Arc::clone(&t.callback);
        let meta = t.meta.clone();

        // Deserialize annotations from JSON — same type as Rust core.
        let annotations: Vec<TestAnnotation> = meta.annotations
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
            Box::pin(async move {
              // Get Page from the fixture pool (created by the core worker).
              let page: std::sync::Arc<ferridriver::Page> = pool.get("page").await
                .map_err(|e| TestFailure {
                  message: format!("fixture 'page' failed: {e}"),
                  stack: None, diff: None, screenshot: None,
                })?;

              // Wrap as NAPI Page and call JS callback.
              let napi_page = crate::page::Page::wrap((*page).clone());
              call_js_test(&cb, napi_page).await.map_err(|e| TestFailure {
                message: e,
                stack: None, diff: None, screenshot: None,
              })
            })
          }),
          fixture_requests: vec!["page".into()],
          annotations,
          timeout: meta.timeout.map(|t| std::time::Duration::from_millis(t as u64)),
          retries: meta.retries.map(|r| r as u32),
          expected_status: ExpectedStatus::Pass,
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

    // Create reporters: no terminal reporter (TS CLI handles display).
    // Only add rerun reporter + result collector.
    let collector = Arc::new(tokio::sync::Mutex::new(ResultCollector::new()));
    let mut reporter_list: Vec<Box<dyn ferridriver_test::reporter::Reporter>> = Vec::new();
    reporter_list.push(Box::new(ferridriver_test::reporter::rerun::RerunReporter::new(
      self.config.output_dir.join("@rerun.txt"),
    )));
    reporter_list.push(Box::new(ResultCollectorReporter(Arc::clone(&collector))));
    let reporters = ferridriver_test::reporter::ReporterSet::new(reporter_list);

    // Run through core pipeline — same as Rust CLI and BDD.
    let start = Instant::now();
    let config = self.config.clone();
    let mut runner = ferridriver_test::runner::TestRunner::new(config, reporters, overrides);
    let _exit_code = runner.run(plan).await;
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
        "flaky" => { flaky += 1; passed += 1; }
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
  pub fn get_timeout(&self) -> f64 { self.config.timeout as f64 }
  #[napi]
  pub fn get_expect_timeout(&self) -> f64 { self.config.expect_timeout as f64 }
  #[napi]
  pub fn get_retries(&self) -> i32 { self.config.retries as i32 }
  #[napi]
  pub fn worker_count(&self) -> i32 { self.config.workers as i32 }
  #[napi]
  pub fn get_base_url(&self) -> Option<String> { self.config.base_url.clone() }

  /// Discover test files.
  #[napi]
  pub fn discover_files(&self, root_dir: String) -> Result<Vec<String>> {
    ferridriver_test::discovery::find_test_files(
      &root_dir, &self.config.test_match, &self.config.test_ignore,
    ).map_err(napi::Error::from_reason)
  }
}

/// Call a JS test callback with a Page and await the returned Promise.
async fn call_js_test(
  tsfn: &TestCallbackFn,
  page: crate::page::Page,
) -> std::result::Result<(), String> {
  // ThreadsafeFunction::call_async sends the Page to the JS thread,
  // calls the callback, and returns the result as a Future.
  // call_async returns Promise<()>, then we await the promise itself.
  match tsfn.call_async(page).await {
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

