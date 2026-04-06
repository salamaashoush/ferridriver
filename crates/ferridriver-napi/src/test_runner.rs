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
}

/// Metadata for a registered test.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct TestMeta {
  pub id: String,
  pub title: String,
  pub file: String,
  pub modifier: String,
  pub timeout: Option<f64>,
  pub retries: Option<i32>,
  pub tags: Option<Vec<String>>,
  /// Suite ID this test belongs to (from register_suite).
  pub suite_id: Option<String>,
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

  /// Run all registered tests. Launches browsers, dispatches in parallel,
  /// calls JS callbacks with Page fixtures, handles retries, returns summary.
  ///
  /// This is the hot path — everything runs from Rust for maximum performance.
  #[napi]
  #[allow(clippy::too_many_lines)]
  pub async fn run(&self) -> Result<RunSummary> {
    let tests = self.tests.lock().await;
    let num_workers = self.config.workers as usize;
    let total_tests = tests.len();

    if total_tests == 0 {
      return Ok(RunSummary {
        total: 0, passed: 0, failed: 0, skipped: 0, flaky: 0,
        duration_ms: 0.0, results: Vec::new(),
      });
    }

    // Forbid-only check: fail if any test has modifier "only" and forbid_only is set.
    if self.config.forbid_only {
      let only_tests: Vec<&str> = tests
        .iter()
        .filter(|t| t.meta.modifier == "only")
        .map(|t| t.meta.title.as_str())
        .collect();
      if !only_tests.is_empty() {
        let msg = format!(
          "Error: test.only() found in {} test(s):\n{}",
          only_tests.len(),
          only_tests.iter().map(|t| format!("  {t}")).collect::<Vec<_>>().join("\n")
        );
        return Err(napi::Error::from_reason(msg));
      }
    }

    // Only-filtering: if any test has modifier "only", keep only those.
    let has_only = tests.iter().any(|t| t.meta.modifier == "only");
    let only_indices: Vec<usize> = if has_only {
      tests.iter().enumerate()
        .filter(|(_, t)| t.meta.modifier == "only")
        .map(|(i, _)| i)
        .collect()
    } else {
      (0..tests.len()).collect()
    };
    // Last-failed filtering: read @rerun.txt and keep only matching tests.
    let run_indices: Vec<usize> = if self.last_failed {
      let rerun_path = self.config.output_dir.join("@rerun.txt");
      if let Ok(content) = std::fs::read_to_string(&rerun_path) {
        let rerun_set: rustc_hash::FxHashSet<&str> = content.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        if rerun_set.is_empty() {
          only_indices
        } else {
          only_indices.into_iter().filter(|&i| {
            rerun_set.contains(tests[i].meta.file.as_str())
              || rerun_set.contains(tests[i].meta.title.as_str())
          }).collect()
        }
      } else {
        only_indices
      }
    } else {
      only_indices
    };
    let total_tests = run_indices.len();

    let start = Instant::now();

    // Only spawn as many workers as there are tests.
    let actual_workers = num_workers.min(total_tests);

    // Build work queue: (test_index, attempt).
    let (work_tx, work_rx) = async_channel::unbounded::<(usize, u32)>();
    for &i in &run_indices {
      let _ = work_tx.send((i, 1)).await;
    }

    // Collect results.
    let _results: Arc<Mutex<Vec<TestResultItem>>> = Arc::new(Mutex::new(Vec::new()));
    let (done_tx, mut done_rx) = tokio::sync::mpsc::channel::<(usize, TestResultItem, bool)>(256);

    // Spawn workers — each launches its own browser on-demand.
    let launch_opts = build_launch_options(&self.config.browser);
    let mut worker_handles = Vec::new();
    for _worker_id in 0..actual_workers {
      let rx = work_rx.clone();
      let tx = done_tx.clone();
      let timeout_ms = self.config.timeout;
      let max_retries = self.config.retries;
      let base_url = self.config.base_url.clone();
      let opts = launch_opts.clone();

      // Collect references to test callbacks and metadata.
      // ThreadsafeFunction is Clone (Arc-based).
      let test_callbacks: Vec<_> = tests.iter().map(|t| Arc::clone(&t.callback)).collect();
      let test_metas: Vec<_> = tests.iter().map(|t| t.meta.clone()).collect();

      let handle = tokio::spawn(async move {
        // Lazy browser launch — only when this worker picks up its first test.
        let mut browser: Option<Arc<ferridriver::Browser>> = None;

        while let Ok((test_idx, attempt)) = rx.recv().await {
          let meta = &test_metas[test_idx];
          let cb = &test_callbacks[test_idx];

          // Skip.
          if meta.modifier == "skip" || meta.modifier == "fixme" {
            let result = TestResultItem {
              id: meta.id.clone(),
              title: meta.title.clone(),
              status: "skipped".into(),
              duration_ms: 0.0,
              attempt: attempt as i32,
              error_message: None,
            };
            let _ = tx.send((test_idx, result, false)).await;
            continue;
          }

          // Lazy browser launch on first real test.
          if browser.is_none() {
            match ferridriver::Browser::launch(opts.clone()).await {
              Ok(b) => { browser = Some(Arc::new(b)); }
              Err(e) => {
                let result = TestResultItem {
                  id: meta.id.clone(), title: meta.title.clone(),
                  status: "failed".into(), duration_ms: 0.0,
                  attempt: attempt as i32,
                  error_message: Some(format!("browser launch failed: {e}")),
                };
                let _ = tx.send((test_idx, result, false)).await;
                continue;
              }
            }
          }
          let browser_ref = browser.as_ref().unwrap();

          // Create isolated page. Navigate to base_url if set (CT mode).
          let ctx = browser_ref.new_context();
          let page_result = match &base_url {
            Some(url) => {
              match ctx.new_page().await {
                Ok(p) => p.goto(url, None).await.map(|()| p),
                Err(e) => Err(e),
              }
            }
            None => ctx.new_page().await,
          };

          let test_start = Instant::now();
          let test_timeout = meta.timeout
            .map(|t| Duration::from_millis(t as u64))
            .unwrap_or(Duration::from_millis(timeout_ms));

          let result = match page_result {
            Ok(page) => {
              let napi_page = crate::page::Page::wrap(page);

              // Call the JS test body with the Page fixture.
              let call_result = tokio::time::timeout(
                test_timeout,
                call_js_test(cb, napi_page),
              ).await;

              let duration = test_start.elapsed();
              let _ = ctx.close().await;

              match call_result {
                Ok(Ok(())) => TestResultItem {
                  id: meta.id.clone(),
                  title: meta.title.clone(),
                  status: "passed".into(),
                  duration_ms: duration.as_secs_f64() * 1000.0,
                  attempt: attempt as i32,
                  error_message: None,
                },
                Ok(Err(e)) => TestResultItem {
                  id: meta.id.clone(),
                  title: meta.title.clone(),
                  status: "failed".into(),
                  duration_ms: duration.as_secs_f64() * 1000.0,
                  attempt: attempt as i32,
                  error_message: Some(e),
                },
                Err(_) => TestResultItem {
                  id: meta.id.clone(),
                  title: meta.title.clone(),
                  status: "timed out".into(),
                  duration_ms: test_timeout.as_secs_f64() * 1000.0,
                  attempt: attempt as i32,
                  error_message: Some(format!("test timed out after {test_timeout:?}")),
                },
              }
            }
            Err(e) => {
              let _ = ctx.close().await;
              TestResultItem {
                id: meta.id.clone(),
                title: meta.title.clone(),
                status: "failed".into(),
                duration_ms: test_start.elapsed().as_secs_f64() * 1000.0,
                attempt: attempt as i32,
                error_message: Some(format!("page creation failed: {e}")),
              }
            }
          };

          let retries = meta.retries.unwrap_or(max_retries as i32) as u32;
          let should_retry = result.status != "passed"
            && result.status != "skipped"
            && attempt <= retries;

          let _ = tx.send((test_idx, result, should_retry)).await;
        }
      });
      worker_handles.push(handle);
    }
    drop(done_tx);
    drop(tests); // Release tests lock.

    // Collect results, handle retries.
    let mut all_results: Vec<TestResultItem> = Vec::new();
    let mut completed = 0usize;

    while let Some((test_idx, result, should_retry)) = done_rx.recv().await {
      if should_retry {
        let next_attempt = result.attempt as u32 + 1;
        let _ = work_tx.send((test_idx, next_attempt)).await;
      } else {
        completed += 1;
      }
      all_results.push(result);

      if completed >= total_tests {
        work_tx.close();
      }
    }

    for handle in worker_handles {
      let _ = handle.await;
    }

    let duration = start.elapsed();

    // Browsers are closed when worker tasks drop (lazy-owned by each worker).

    // Compute summary.
    // For flaky detection: group by test ID, if last attempt passed but earlier failed → flaky.
    let mut final_results: Vec<TestResultItem> = Vec::new();
    let mut passed = 0i32;
    let mut failed = 0i32;
    let mut skipped = 0i32;
    let mut flaky = 0i32;

    // Group results by test ID, take the last attempt.
    let mut by_id: rustc_hash::FxHashMap<String, Vec<&TestResultItem>> = rustc_hash::FxHashMap::default();
    for r in &all_results {
      by_id.entry(r.id.clone()).or_default().push(r);
    }
    for attempts in by_id.values() {
      let last = attempts.last().unwrap();
      let mut item = (*last).clone();
      if last.status == "passed" && attempts.len() > 1 {
        item.status = "flaky".into();
        flaky += 1;
        passed += 1;
      } else {
        match item.status.as_str() {
          "passed" => passed += 1,
          "skipped" => skipped += 1,
          _ => failed += 1,
        }
      }
      final_results.push(item);
    }

    Ok(RunSummary {
      total: total_tests as i32,
      passed,
      failed,
      skipped,
      flaky,
      duration_ms: duration.as_secs_f64() * 1000.0,
      results: final_results,
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

fn build_launch_options(
  bc: &ferridriver_test::config::BrowserConfig,
) -> ferridriver::options::LaunchOptions {
  use ferridriver::backend::BackendKind;
  let backend = match bc.backend.as_str() {
    "cdp-raw" => BackendKind::CdpRaw,
    #[cfg(target_os = "macos")]
    "webkit" => BackendKind::WebKit,
    _ => BackendKind::CdpPipe,
  };
  ferridriver::options::LaunchOptions {
    backend,
    headless: bc.headless,
    executable_path: bc.executable_path.clone(),
    args: bc.args.clone(),
    viewport: bc.viewport.as_ref().map(|v| ferridriver::options::ViewportConfig {
      width: v.width, height: v.height, ..Default::default()
    }),
    ..Default::default()
  }
}
