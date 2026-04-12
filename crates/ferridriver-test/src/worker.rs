//! Worker: owns a browser instance, executes hooks, creates fresh context+page per test.
//!
//! Hook execution model (matching Playwright):
//! - beforeAll: once per suite PER WORKER, tracked in `active_suites` map
//! - afterAll: when worker finishes, for every suite that had beforeAll run
//! - beforeEach: before every test, gets the test's fixture pool
//! - afterEach: after every test (even on failure), gets the test's fixture pool
//!
//! Serial batches: all tests run in order on this worker. On first failure, remaining
//! tests are skipped but afterAll still runs.

use std::sync::Arc;
use std::time::{Duration, Instant};

use rustc_hash::FxHashMap;
use tokio::sync::{Mutex, mpsc};

use crate::config::TestConfig;
use crate::dispatcher::{SerialBatch, TestAssignment, WorkItem};
use crate::fixture::{FixturePool, FixtureScope};
use crate::model::{
  Attachment, AttachmentBody, ExpectedStatus, Hooks, StepCategory, TestAnnotation, TestFailure, TestInfo, TestOutcome,
  TestStatus,
};
use crate::reporter::{EventBus, ReporterEvent};

/// Result of a single test execution within a worker.
pub struct WorkerTestResult {
  pub outcome: TestOutcome,
  pub should_retry: bool,
  pub test_fn: crate::model::TestFn,
  pub test_id: crate::model::TestId,
  pub fixture_requests: Vec<String>,
  pub suite_key: String,
  pub hooks: Arc<Hooks>,
}

/// Per-suite state tracked on this worker.
struct SuiteState {
  before_all_ran: bool,
  before_all_failed: bool,
  hooks: Arc<Hooks>,
}

/// A worker that owns a browser and processes tests sequentially.
pub struct Worker {
  pub id: u32,
  config: Arc<TestConfig>,
  event_bus: EventBus,
}

impl Worker {
  pub fn new(id: u32, config: Arc<TestConfig>, event_bus: EventBus) -> Self {
    Self { id, config, event_bus }
  }

  pub async fn run(
    &self,
    browser: Arc<ferridriver::Browser>,
    custom_fixture_pool: FixturePool,
    rx: async_channel::Receiver<WorkItem>,
    result_tx: mpsc::Sender<WorkerTestResult>,
  ) {
    self
      .event_bus
      .emit(ReporterEvent::WorkerStarted { worker_id: self.id })
      .await;

    let mut active_suites: FxHashMap<String, SuiteState> = FxHashMap::default();

    while let Ok(item) = rx.recv().await {
      match item {
        WorkItem::Single(assignment) => {
          let result = self
            .run_single(&browser, &custom_fixture_pool, &mut active_suites, assignment)
            .await;
          if result_tx.send(result).await.is_err() {
            break;
          }
        },
        WorkItem::Serial(batch) => {
          let results = self
            .run_serial_batch(&browser, &custom_fixture_pool, &mut active_suites, batch)
            .await;
          for result in results {
            if result_tx.send(result).await.is_err() {
              break;
            }
          }
        },
      }
    }

    // Run afterAll for every suite that had beforeAll on this worker.
    for (suite_key, state) in &active_suites {
      if state.before_all_ran {
        for (i, hook) in state.hooks.after_all.iter().enumerate() {
          let step_title = if state.hooks.after_all.len() == 1 {
            "afterAll".to_string()
          } else {
            format!("afterAll [{i}]")
          };
          // afterAll has no test context — emit synthetic step events.
          let step_id = format!("hook:afterAll:{suite_key}:{i}");
          // Use a synthetic TestId for the suite.
          let synthetic_id = crate::model::TestId {
            file: suite_key.clone(),
            suite: None,
            name: step_title.clone(),
            line: None,
          };
          self
            .event_bus
            .emit(ReporterEvent::StepStarted(Box::new(
              crate::reporter::StepStartedEvent {
                test_id: synthetic_id.clone(),
                step_id: step_id.clone(),
                parent_step_id: None,
                title: step_title.clone(),
                category: StepCategory::Hook,
              },
            )))
            .await;
          let start = Instant::now();
          let result = hook(custom_fixture_pool.clone()).await;
          let duration = start.elapsed();
          let error = result.as_ref().err().map(|e| format!("{e}"));
          self
            .event_bus
            .emit(ReporterEvent::StepFinished(Box::new(
              crate::reporter::StepFinishedEvent {
                test_id: synthetic_id,
                step_id,
                title: step_title,
                category: StepCategory::Hook,
                duration,
                error: error.clone(),
                metadata: None,
              },
            )))
            .await;
          if let Err(e) = result {
            tracing::warn!(target: "ferridriver::worker", "afterAll error: {e}");
          }
        }
      }
    }

    custom_fixture_pool.teardown_all().await;

    self
      .event_bus
      .emit(ReporterEvent::WorkerFinished { worker_id: self.id })
      .await;
  }

  /// Run a serial batch: all tests in order, skip rest on failure.
  async fn run_serial_batch(
    &self,
    browser: &Arc<ferridriver::Browser>,
    custom_pool: &FixturePool,
    active_suites: &mut FxHashMap<String, SuiteState>,
    batch: SerialBatch,
  ) -> Vec<WorkerTestResult> {
    let mut results = Vec::with_capacity(batch.assignments.len());
    let mut serial_failed = false;

    for assignment in batch.assignments {
      if serial_failed {
        // Skip remaining tests in the serial suite.
        let test = &assignment.test;
        let outcome = TestOutcome {
          test_id: test.id.clone(),
          status: TestStatus::Skipped,
          duration: Duration::ZERO,
          attempt: assignment.attempt,
          max_attempts: test.retries.unwrap_or(self.config.retries) + 1,
          error: Some(TestFailure {
            message: "skipped due to previous failure in serial suite".into(),
            stack: None,
            diff: None,
            screenshot: None,
          }),
          attachments: Vec::new(),
          steps: Vec::new(),
          stdout: String::new(),
          stderr: String::new(),
          annotations: test.annotations.clone(),
          metadata: self.config.metadata.clone(),
        };
        self
          .event_bus
          .emit(ReporterEvent::TestFinished {
            test_id: test.id.clone(),
            outcome: outcome.clone(),
          })
          .await;
        results.push(WorkerTestResult {
          outcome,
          should_retry: false,
          test_fn: Arc::clone(&test.test_fn),
          test_id: test.id.clone(),
          fixture_requests: test.fixture_requests.clone(),
          suite_key: assignment.suite_key,
          hooks: assignment.hooks,
        });
        continue;
      }

      let result = self.run_single(browser, custom_pool, active_suites, assignment).await;
      if result.outcome.status == TestStatus::Failed || result.outcome.status == TestStatus::TimedOut {
        serial_failed = true;
      }
      results.push(result);
    }

    results
  }

  /// Run a single test with full hook lifecycle.
  async fn run_single(
    &self,
    browser: &Arc<ferridriver::Browser>,
    custom_pool: &FixturePool,
    active_suites: &mut FxHashMap<String, SuiteState>,
    assignment: TestAssignment,
  ) -> WorkerTestResult {
    let test = &assignment.test;
    let test_id = test.id.clone();
    let test_fn = Arc::clone(&test.test_fn);
    let fixture_requests = test.fixture_requests.clone();
    let attempt = assignment.attempt;
    let max_retries = test.retries.unwrap_or(self.config.retries);
    let max_attempts = max_retries + 1;
    let suite_key = assignment.suite_key.clone();

    tracing::debug!(
      target: "ferridriver::worker",
      worker = self.id,
      test = test_id.full_name(),
      attempt,
      max_attempts,
      "dispatching test",
    );
    let hooks = Arc::clone(&assignment.hooks);

    // ── beforeAll (once per suite on this worker) ──
    let suite_state = active_suites.entry(suite_key.clone()).or_insert_with(|| SuiteState {
      before_all_ran: false,
      before_all_failed: false,
      hooks: Arc::clone(&hooks),
    });

    if !suite_state.before_all_ran && !hooks.before_all.is_empty() {
      for (i, hook) in hooks.before_all.iter().enumerate() {
        let step_title = if hooks.before_all.len() == 1 {
          "beforeAll".to_string()
        } else {
          format!("beforeAll [{i}]")
        };
        self
          .event_bus
          .emit(ReporterEvent::StepStarted(Box::new(
            crate::reporter::StepStartedEvent {
              test_id: test_id.clone(),
              step_id: format!("hook:beforeAll:{suite_key}:{i}"),
              parent_step_id: None,
              title: step_title.clone(),
              category: StepCategory::Hook,
            },
          )))
          .await;
        let start = Instant::now();
        let result = hook(custom_pool.clone()).await;
        let duration = start.elapsed();
        let error = result.as_ref().err().map(|e| e.message.clone());
        self
          .event_bus
          .emit(ReporterEvent::StepFinished(Box::new(
            crate::reporter::StepFinishedEvent {
              test_id: test_id.clone(),
              step_id: format!("hook:beforeAll:{suite_key}:{i}"),
              title: step_title,
              category: StepCategory::Hook,
              duration,
              error: error.clone(),
              metadata: None,
            },
          )))
          .await;
        if let Err(e) = result {
          tracing::error!(target: "ferridriver::worker", "beforeAll failed for {suite_key}: {e}");
          suite_state.before_all_failed = true;
          break;
        }
      }
      suite_state.before_all_ran = true;
    }

    // If beforeAll failed, skip this test.
    if suite_state.before_all_failed {
      let outcome = TestOutcome {
        test_id: test_id.clone(),
        status: TestStatus::Skipped,
        duration: Duration::ZERO,
        attempt,
        max_attempts,
        error: Some(TestFailure {
          message: format!("skipped: beforeAll failed for suite '{suite_key}'"),
          stack: None,
          diff: None,
          screenshot: None,
        }),
        attachments: Vec::new(),
        steps: Vec::new(),
        stdout: String::new(),
        stderr: String::new(),
        annotations: test.annotations.clone(),
        metadata: self.config.metadata.clone(),
      };
      self
        .event_bus
        .emit(ReporterEvent::TestFinished {
          test_id: test_id.clone(),
          outcome: outcome.clone(),
        })
        .await;
      return WorkerTestResult {
        outcome,
        should_retry: false,
        test_fn,
        test_id,
        fixture_requests,
        suite_key,
        hooks,
      };
    }

    // Check for skip/fixme (with conditional evaluation).
    let browser_config = &self.config.browser;
    let should_skip = test.annotations.iter().any(|a| match a {
      TestAnnotation::Skip { condition: None, .. } => true,
      TestAnnotation::Skip {
        condition: Some(cond), ..
      } => evaluate_condition(cond, browser_config),
      TestAnnotation::Fixme { condition: None, .. } => true,
      TestAnnotation::Fixme {
        condition: Some(cond), ..
      } => evaluate_condition(cond, browser_config),
      _ => false,
    });
    if should_skip {
      let outcome = TestOutcome {
        test_id: test_id.clone(),
        status: TestStatus::Skipped,
        duration: Duration::ZERO,
        attempt,
        max_attempts,
        error: None,
        attachments: Vec::new(),
        steps: Vec::new(),
        stdout: String::new(),
        stderr: String::new(),
        annotations: test.annotations.clone(),
        metadata: self.config.metadata.clone(),
      };
      self
        .event_bus
        .emit(ReporterEvent::TestFinished {
          test_id: test_id.clone(),
          outcome: outcome.clone(),
        })
        .await;
      return WorkerTestResult {
        outcome,
        should_retry: false,
        test_fn,
        test_id,
        fixture_requests,
        suite_key,
        hooks,
      };
    }

    self
      .event_bus
      .emit(ReporterEvent::TestStarted {
        test_id: test_id.clone(),
        attempt,
      })
      .await;

    // Evaluate Fail condition: if condition matches, expect failure (invert pass/fail).
    let mut expected_status = test.expected_status.clone();
    for ann in &test.annotations {
      if let TestAnnotation::Fail { condition, .. } = ann {
        let applies = match condition {
          None => true,
          Some(cond) => evaluate_condition(cond, browser_config),
        };
        if applies {
          expected_status = ExpectedStatus::Fail;
        }
      }
    }

    // Timeout with slow multiplier (conditional).
    let mut timeout_dur = test.timeout.unwrap_or(Duration::from_millis(self.config.timeout));
    let is_slow = test.annotations.iter().any(|a| match a {
      TestAnnotation::Slow { condition: None, .. } => true,
      TestAnnotation::Slow {
        condition: Some(cond), ..
      } => evaluate_condition(cond, browser_config),
      _ => false,
    });
    if is_slow {
      timeout_dur *= 3;
    }

    let start = Instant::now();

    // Create fresh isolated context + page.
    let ctx = browser.new_context();
    let page_result = ctx.new_page().await;

    // Apply context config to the page (Playwright's `use` block).
    // Merge per-test use_options over global config (test.use() overrides).
    if let Ok(ref page) = page_result {
      let mut ctx_config = self.config.browser.context.clone();
      if let Some(ref opts) = test.use_options {
        // Merge use_options fields over the global context config.
        if let Some(v) = opts.get("locale").and_then(|v| v.as_str()) {
          ctx_config.locale = Some(v.to_string());
        }
        if let Some(v) = opts.get("colorScheme").and_then(|v| v.as_str()) {
          ctx_config.color_scheme = Some(v.to_string());
        }
        if let Some(v) = opts.get("timezoneId").and_then(|v| v.as_str()) {
          ctx_config.timezone_id = Some(v.to_string());
        }
        if let Some(v) = opts.get("isMobile").and_then(|v| v.as_bool()) {
          ctx_config.is_mobile = v;
        }
        if let Some(v) = opts.get("hasTouch").and_then(|v| v.as_bool()) {
          ctx_config.has_touch = v;
        }
        if let Some(v) = opts.get("offline").and_then(|v| v.as_bool()) {
          ctx_config.offline = v;
        }
        if let Some(v) = opts.get("javaScriptEnabled").and_then(|v| v.as_bool()) {
          ctx_config.java_script_enabled = v;
        }
        if let Some(v) = opts.get("bypassCSP").and_then(|v| v.as_bool()) {
          ctx_config.bypass_csp = v;
        }
        if let Some(v) = opts.get("userAgent").and_then(|v| v.as_str()) {
          ctx_config.user_agent = Some(v.to_string());
        }
        if let Some(v) = opts.get("deviceScaleFactor").and_then(|v| v.as_f64()) {
          ctx_config.device_scale_factor = Some(v);
        }
        if let Some(v) = opts.get("reducedMotion").and_then(|v| v.as_str()) {
          ctx_config.reduced_motion = Some(v.to_string());
        }
        if let Some(v) = opts.get("forcedColors").and_then(|v| v.as_str()) {
          ctx_config.forced_colors = Some(v.to_string());
        }
        if let Some(v) = opts.get("serviceWorkers").and_then(|v| v.as_str()) {
          ctx_config.service_workers = Some(v.to_string());
        }
        if let Some(v) = opts.get("storageState").and_then(|v| v.as_str()) {
          ctx_config.storage_state = Some(v.to_string());
        }
        if let Some(v) = opts.get("acceptDownloads").and_then(|v| v.as_bool()) {
          ctx_config.accept_downloads = v;
        }
        if let Some(v) = opts.get("ignoreHTTPSErrors").and_then(|v| v.as_bool()) {
          ctx_config.ignore_https_errors = v;
        }
        // Geolocation: { latitude, longitude, accuracy? }
        if let Some(geo) = opts.get("geolocation").and_then(|v| v.as_object()) {
          if let (Some(lat), Some(lon)) = (
            geo.get("latitude").and_then(|v| v.as_f64()),
            geo.get("longitude").and_then(|v| v.as_f64()),
          ) {
            ctx_config.geolocation = Some(crate::config::GeolocationConfig {
              latitude: lat,
              longitude: lon,
              accuracy: geo.get("accuracy").and_then(|v| v.as_f64()),
            });
          }
        }
        // Permissions: string[]
        if let Some(arr) = opts.get("permissions").and_then(|v| v.as_array()) {
          let perms: Vec<String> = arr.iter().filter_map(|v| v.as_str().map(String::from)).collect();
          if !perms.is_empty() {
            ctx_config.permissions = perms;
          }
        }
        // Extra HTTP headers: Record<string, string>
        if let Some(obj) = opts.get("extraHTTPHeaders").and_then(|v| v.as_object()) {
          let headers: std::collections::BTreeMap<String, String> = obj
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
            .collect();
          if !headers.is_empty() {
            ctx_config.extra_http_headers = headers;
          }
        }
        // HTTP credentials: { username, password, origin? }
        if let Some(creds) = opts.get("httpCredentials").and_then(|v| v.as_object()) {
          if let (Some(user), Some(pass)) = (
            creds.get("username").and_then(|v| v.as_str()),
            creds.get("password").and_then(|v| v.as_str()),
          ) {
            ctx_config.http_credentials = Some(crate::config::HttpCredentialsConfig {
              username: user.to_string(),
              password: pass.to_string(),
              origin: creds.get("origin").and_then(|v| v.as_str()).map(String::from),
            });
          }
        }
      }
      // Viewport override from use_options: { width, height }
      let viewport_override = test.use_options.as_ref().and_then(|opts| {
        opts.get("viewport").and_then(|v| {
          let w = v.get("width").and_then(|w| w.as_i64());
          let h = v.get("height").and_then(|h| h.as_i64());
          match (w, h) {
            (Some(w), Some(h)) => Some(crate::config::ViewportConfig { width: w, height: h }),
            _ => None,
          }
        })
      });

      let ctx_config = &ctx_config;
      // Viewport + mobile/touch emulation.
      // Skip if no context-level overrides — new_page() already set the base viewport.
      // Only re-send if test.use() changed viewport, or context needs mobile/touch/scale.
      let effective_vp = viewport_override.as_ref().or(self.config.browser.viewport.as_ref());
      let has_ctx_overrides = viewport_override.is_some()
        || ctx_config.is_mobile
        || ctx_config.has_touch
        || ctx_config
          .device_scale_factor
          .is_some_and(|d| (d - 1.0).abs() > f64::EPSILON);
      if has_ctx_overrides {
        if let Some(vp) = effective_vp {
          let _ = page
            .set_viewport(&ferridriver::options::ViewportConfig {
              width: vp.width,
              height: vp.height,
              device_scale_factor: ctx_config.device_scale_factor.unwrap_or(1.0),
              is_mobile: ctx_config.is_mobile,
              has_touch: ctx_config.has_touch,
              is_landscape: ctx_config.is_mobile && vp.width > vp.height,
            })
            .await;
        }
      }
      // Color scheme / media emulation.
      if ctx_config.color_scheme.is_some() {
        let _ = page
          .emulate_media(&ferridriver::options::EmulateMediaOptions {
            color_scheme: ctx_config.color_scheme.clone(),
            ..Default::default()
          })
          .await;
      }
      // Locale.
      if let Some(ref locale) = ctx_config.locale {
        let _ = page.set_locale(locale).await;
      }
      // Timezone.
      if let Some(ref tz) = ctx_config.timezone_id {
        let _ = page.set_timezone(tz).await;
      }
      // Geolocation.
      if let Some(ref geo) = ctx_config.geolocation {
        let _ = page
          .set_geolocation(geo.latitude, geo.longitude, geo.accuracy.unwrap_or(0.0))
          .await;
      }
      // Offline mode.
      if ctx_config.offline {
        let _ = page.set_network_state(true, 0.0, -1.0, -1.0).await;
      }
      // Permissions.
      if !ctx_config.permissions.is_empty() {
        let _ = page.grant_permissions(&ctx_config.permissions, None).await;
      }
      // Extra HTTP headers.
      if !ctx_config.extra_http_headers.is_empty() {
        let headers: rustc_hash::FxHashMap<String, String> = ctx_config
          .extra_http_headers
          .iter()
          .map(|(k, v)| (k.clone(), v.clone()))
          .collect();
        let _ = page.set_extra_http_headers(&headers).await;
      }
      // User agent.
      if let Some(ref ua) = ctx_config.user_agent {
        let _ = page.set_user_agent(ua).await;
      }
      // JavaScript enabled/disabled.
      if !ctx_config.java_script_enabled {
        let _ = page.set_javascript_enabled(false).await;
      }
      // Reduced motion + forced colors via emulate_media.
      if ctx_config.reduced_motion.is_some() || ctx_config.forced_colors.is_some() {
        let _ = page
          .emulate_media(&ferridriver::options::EmulateMediaOptions {
            color_scheme: None, // Already set above if needed.
            reduced_motion: ctx_config.reduced_motion.clone(),
            forced_colors: ctx_config.forced_colors.clone(),
            ..Default::default()
          })
          .await;
      }
      // Bypass CSP (must be before first navigation).
      if ctx_config.bypass_csp {
        let _ = page.set_bypass_csp(true).await;
      }
      // Ignore HTTPS certificate errors.
      if ctx_config.ignore_https_errors {
        let _ = page.set_ignore_certificate_errors(true).await;
      }
      // Accept downloads — configure download behavior.
      if ctx_config.accept_downloads {
        let download_dir = self.config.output_dir.join("downloads");
        let _ = std::fs::create_dir_all(&download_dir);
        let _ = page
          .set_download_behavior("allowAndName", &download_dir.display().to_string())
          .await;
      }
      // HTTP credentials (basic auth).
      if let Some(ref creds) = ctx_config.http_credentials {
        let _ = page.set_http_credentials(&creds.username, &creds.password).await;
      }
      // Block service workers.
      if ctx_config.service_workers.as_deref() == Some("block") {
        let _ = page.set_service_workers_blocked(true).await;
      }
    }

    // Create TestInfo for this test execution.
    let test_info = Arc::new(TestInfo {
      test_id: test_id.clone(),
      title_path: {
        let mut path = Vec::new();
        path.push(test_id.file.clone());
        if let Some(ref s) = test_id.suite {
          path.push(s.clone());
        }
        path.push(test_id.name.clone());
        path
      },
      retry: attempt.saturating_sub(1),
      worker_index: self.id,
      parallel_index: self.id,
      repeat_each_index: 0,
      output_dir: self.config.output_dir.join(test_id.full_name()),
      snapshot_dir: self
        .config
        .snapshot_dir
        .as_ref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("__snapshots__")),
      snapshot_path_template: self.config.snapshot_path_template.clone(),
      update_snapshots: self.config.update_snapshots,
      attachments: Arc::new(Mutex::new(Vec::new())),
      steps: Arc::new(Mutex::new(Vec::new())),
      soft_errors: Arc::new(Mutex::new(Vec::new())),
      timeout: timeout_dur,
      tags: test
        .annotations
        .iter()
        .filter_map(|a| match a {
          TestAnnotation::Tag(t) => Some(t.clone()),
          _ => None,
        })
        .collect(),
      start_time: start,
      event_bus: Some(self.event_bus.clone()),
      annotations: Arc::new(Mutex::new(Vec::new())),
    });

    let result = match page_result {
      Ok(page) => {
        let test_pool = custom_pool.child(FixtureScope::Test);
        test_pool.inject("browser", Arc::clone(browser));
        test_pool.inject("context", Arc::new(ctx.clone()));
        test_pool.inject("page", Arc::clone(&page));
        test_pool.inject("test_info", Arc::clone(&test_info));

        // ── Request fixture (API testing context) ──
        // baseURL from test.use() overrides global config.
        let effective_base_url = test
          .use_options
          .as_ref()
          .and_then(|opts| opts.get("baseURL").and_then(|v| v.as_str()).map(String::from))
          .or_else(|| self.config.base_url.clone());
        let request_ctx = Arc::new(ferridriver::api_request::APIRequestContext::new(
          ferridriver::api_request::RequestContextOptions {
            base_url: effective_base_url,
            ..Default::default()
          },
        ));
        test_pool.inject("request", request_ctx);

        // ── Storage state (apply before any test code) ──
        // Context-level storage_state takes precedence over top-level.
        let ss_path = self
          .config
          .browser
          .context
          .storage_state
          .as_ref()
          .or(self.config.storage_state.as_ref());
        if let Some(ss_path) = ss_path {
          let path = std::path::Path::new(ss_path);
          match std::fs::read_to_string(path) {
            Ok(json_str) => match serde_json::from_str::<serde_json::Value>(&json_str) {
              Ok(state) => {
                if let Err(e) = page.set_storage_state(&state).await {
                  tracing::warn!(target: "ferridriver::worker", "set_storage_state failed: {e}");
                }
              },
              Err(e) => {
                tracing::warn!(target: "ferridriver::worker", "parse storage state {}: {e}", path.display());
              },
            },
            Err(e) => {
              tracing::warn!(target: "ferridriver::worker", "read storage state {}: {e}", path.display());
            },
          }
        }

        // ── Video recording (start before any test code) ──
        // retain-on-failure: buffer frames in memory, only encode if test fails (zero ffmpeg cost for passing tests)
        // on: eager mode, pipe frames to ffmpeg in real-time
        enum VideoHandle {
          Eager(ferridriver::video::VideoRecordingHandle),
          Buffered(ferridriver::video::BufferedRecordingHandle),
        }
        let video_handle: Option<VideoHandle> = match self.config.video.mode {
          crate::config::VideoMode::Off => None,
          crate::config::VideoMode::On => {
            let ext = ferridriver::video::video_extension();
            let video_path =
              test_info
                .output_dir
                .join(format!("{}-attempt{}.{ext}", sanitize_filename(&test_id.name), attempt));
            let _ = std::fs::create_dir_all(&test_info.output_dir);
            match ferridriver::video::start_recording(
              &page,
              video_path,
              self.config.video.width,
              self.config.video.height,
              80,
            )
            .await
            {
              Ok(h) => Some(VideoHandle::Eager(h)),
              Err(e) => {
                tracing::warn!(target: "ferridriver::worker", "video start failed: {e}");
                None
              },
            }
          },
          crate::config::VideoMode::RetainOnFailure => {
            match ferridriver::video::start_buffered_recording(
              &page,
              self.config.video.width,
              self.config.video.height,
              80,
            )
            .await
            {
              Ok(h) => Some(VideoHandle::Buffered(h)),
              Err(e) => {
                tracing::warn!(target: "ferridriver::worker", "video start failed: {e}");
                None
              },
            }
          },
        };

        // ── beforeEach hooks ──
        let mut before_each_err = None;
        for (i, hook) in hooks.before_each.iter().enumerate() {
          let title = if hooks.before_each.len() == 1 {
            "beforeEach".to_string()
          } else {
            format!("beforeEach [{i}]")
          };
          let step_handle = test_info.begin_step(&title, StepCategory::Hook).await;
          let result = hook(test_pool.clone(), Arc::clone(&test_info)).await;
          let err_msg = result.as_ref().err().map(|e| e.message.clone());
          step_handle.end(err_msg).await;
          if let Err(e) = result {
            before_each_err = Some(e);
            break;
          }
        }

        let r = if let Some(err) = before_each_err {
          Ok(Err(err))
        } else {
          tokio::time::timeout(timeout_dur, (test.test_fn)(test_pool.clone())).await
        };

        // ── afterEach hooks (ALWAYS run, even on failure) ──
        for (i, hook) in hooks.after_each.iter().enumerate() {
          let title = if hooks.after_each.len() == 1 {
            "afterEach".to_string()
          } else {
            format!("afterEach [{i}]")
          };
          let step_handle = test_info.begin_step(&title, StepCategory::Hook).await;
          let result = hook(test_pool.clone(), Arc::clone(&test_info)).await;
          let err_msg = result.as_ref().err().map(|e| e.message.clone());
          step_handle.end(err_msg).await;
          if let Err(e) = result {
            tracing::warn!(target: "ferridriver::worker", "afterEach error: {e}");
          }
        }

        // Screenshot on failure (before context close).
        let screenshot = if r.as_ref().is_err() || r.as_ref().is_ok_and(|r| r.is_err()) {
          capture_screenshot(&page).await
        } else {
          None
        };

        // ── Stop video recording (before context close, page session must be alive) ──
        let test_failed = r.as_ref().is_err() || r.as_ref().is_ok_and(|r| r.is_err());
        let video_path = match video_handle {
          Some(VideoHandle::Eager(handle)) => match handle.stop(&page).await {
            Ok(path) => Some(path),
            Err(e) => {
              tracing::warn!(target: "ferridriver::worker", "video stop failed: {e}");
              None
            },
          },
          Some(VideoHandle::Buffered(handle)) => {
            if test_failed {
              // Test failed — encode the buffered frames to a video file.
              let ext = ferridriver::video::video_extension();
              let video_path =
                test_info
                  .output_dir
                  .join(format!("{}-attempt{}.{ext}", sanitize_filename(&test_id.name), attempt));
              let _ = std::fs::create_dir_all(&test_info.output_dir);
              match handle.encode(&page, video_path).await {
                Ok(path) => Some(path),
                Err(e) => {
                  tracing::warn!(target: "ferridriver::worker", "video encode failed: {e}");
                  None
                },
              }
            } else {
              // Test passed — discard frames, no encoding cost.
              handle.discard(&page).await;
              None
            }
          },
          None => None,
        };

        let _ = ctx.close().await;
        (r, screenshot, video_path, Some(test_pool))
      },
      Err(e) => {
        let _ = ctx.close().await;
        (
          Ok(Err(TestFailure {
            message: format!("failed to create page: {e}"),
            stack: None,
            diff: None,
            screenshot: None,
          })),
          None,
          None,
          None,
        )
      },
    };

    let duration = start.elapsed();
    let (timeout_result, screenshot, video_path, test_pool) = result;

    let mut attachments = Vec::new();
    if let Some(ref png) = screenshot {
      attachments.push(Attachment {
        name: "screenshot-on-failure".into(),
        content_type: "image/png".into(),
        body: AttachmentBody::Bytes(png.clone()),
      });
    }

    let (raw_status, raw_error) = match timeout_result {
      Ok(Ok(())) => (TestStatus::Passed, None),
      Ok(Err(failure)) => {
        // Runtime skip: test body called test.skip() — treat as skip, not failure.
        // This mirrors Playwright's TestSkipError thrown by test.skip() inside body.
        if failure.message.contains("__FERRIDRIVER_SKIP__:") {
          let reason = failure.message.split("__FERRIDRIVER_SKIP__:").nth(1).unwrap_or("");
          tracing::debug!(target: "ferridriver::worker", "test skipped at runtime: {reason}");
          let outcome = TestOutcome {
            test_id: test_id.clone(),
            status: TestStatus::Skipped,
            duration: start.elapsed(),
            attempt,
            max_attempts,
            error: None,
            attachments: Vec::new(),
            steps: Vec::new(),
            stdout: String::new(),
            stderr: String::new(),
            annotations: test.annotations.clone(),
            metadata: self.config.metadata.clone(),
          };
          self
            .event_bus
            .emit(ReporterEvent::TestFinished {
              test_id: test_id.clone(),
              outcome: outcome.clone(),
            })
            .await;
          return WorkerTestResult {
            outcome,
            should_retry: false,
            test_fn,
            test_id,
            fixture_requests,
            suite_key,
            hooks,
          };
        }

        let mut failure = failure;
        if failure.screenshot.is_none() {
          failure.screenshot = screenshot;
        }
        (TestStatus::Failed, Some(failure))
      },
      Err(_) => (
        TestStatus::TimedOut,
        Some(TestFailure {
          message: format!("test timed out after {timeout_dur:?}"),
          stack: None,
          diff: None,
          screenshot,
        }),
      ),
    };

    // Read runtime modifiers set by test body (via NAPI TestInfo.skip/fail/slow/setTimeout).
    // These are injected into the fixture pool by the NAPI test_fn closure.
    if let Some(ref pool) = test_pool {
      if let Ok(modifiers) = pool.get::<crate::TestModifiers>("__test_modifiers").await {
        if modifiers.expected_failure.load(std::sync::atomic::Ordering::Relaxed) {
          expected_status = ExpectedStatus::Fail;
        }
        // Runtime slow: annotate via test_info for reporters.
        if modifiers.slow.load(std::sync::atomic::Ordering::Relaxed) {
          test_info.annotate("slow", "test.slow() called at runtime").await;
        }
        // timeout_override: already elapsed for this attempt, but log for debugging.
        if let Ok(guard) = modifiers.timeout_override.lock() {
          if let Some(ms) = *guard {
            tracing::debug!(target: "ferridriver::worker", "test.setTimeout({ms}ms) called at runtime");
          }
        }
      }
    }

    // Expected failure inversion (test.fail() annotation OR runtime test.fail()).
    let (status, error) = match (&raw_status, &expected_status) {
      (TestStatus::Failed | TestStatus::TimedOut, ExpectedStatus::Fail) => (TestStatus::Passed, None),
      (TestStatus::Passed, ExpectedStatus::Fail) => (
        TestStatus::Failed,
        Some(TestFailure {
          message: "expected test to fail, but it passed".into(),
          stack: None,
          diff: None,
          screenshot: None,
        }),
      ),
      _ => (raw_status, raw_error),
    };

    // Collect soft assertion errors.
    let soft_errs = test_info.drain_soft_errors().await;
    let (status, error) = if !soft_errs.is_empty() && status == TestStatus::Passed {
      let msg = soft_errs
        .iter()
        .map(|e| format!("  - {}", e.message))
        .collect::<Vec<_>>()
        .join("\n");
      (
        TestStatus::Failed,
        Some(TestFailure {
          message: format!("{} soft assertion(s) failed:\n{msg}", soft_errs.len()),
          stack: None,
          diff: None,
          screenshot: None,
        }),
      )
    } else {
      (status, error)
    };

    // Collect tracked test steps and attachments.
    let steps = test_info.steps.lock().await.clone();
    let info_attachments = test_info.attachments.lock().await.clone();
    attachments.extend(info_attachments);

    // ── Trace recording ──
    // Uses should_write() to skip entirely for RetainOnFailure + passed tests
    // (no wasted ZIP write + delete). Serialization happens in-memory (borrows
    // steps, zero-copy for titles/errors), file I/O on spawn_blocking.
    let trace_mode = self.config.trace;
    let test_failed = status == TestStatus::Failed || status == TestStatus::TimedOut;
    if trace_mode.should_write(attempt, test_failed) {
      let mut recorder = crate::tracing::TraceRecorder::for_steps(&steps);
      recorder.record_steps(&steps);
      // Serialize to in-memory ZIP bytes (fast, no file I/O).
      match recorder.into_zip_bytes() {
        Ok(zip_bytes) => {
          let trace_path = test_info.output_dir.join(format!(
            "{}-attempt{}.trace.zip",
            sanitize_filename(&test_id.name),
            attempt
          ));
          // Offload file write to blocking thread so the async worker isn't stalled.
          let write_path = trace_path.clone();
          let write_result =
            tokio::task::spawn_blocking(move || crate::tracing::write_trace_file(&write_path, &zip_bytes)).await;
          match write_result {
            Ok(Ok(())) => {
              attachments.push(Attachment {
                name: "trace".into(),
                content_type: "application/zip".into(),
                body: AttachmentBody::Path(trace_path),
              });
            },
            Ok(Err(e)) => tracing::warn!(target: "ferridriver::worker", "trace write failed: {e}"),
            Err(e) => tracing::warn!(target: "ferridriver::worker", "trace task panicked: {e}"),
          }
        },
        Err(e) => tracing::warn!(target: "ferridriver::worker", "trace serialize failed: {e}"),
      }
    }

    // Attach or clean up video recording.
    // For buffered mode, video_path is only Some when the test failed (already filtered).
    // For eager mode, we keep or delete based on the mode.
    if let Some(ref path) = video_path {
      let keep = match self.config.video.mode {
        crate::config::VideoMode::On => true,
        crate::config::VideoMode::RetainOnFailure => true, // buffered mode already filtered
        crate::config::VideoMode::Off => false,
      };
      if keep && path.exists() {
        attachments.push(Attachment {
          name: "video".into(),
          content_type: ferridriver::video::video_content_type().into(),
          body: AttachmentBody::Path(path.clone()),
        });
      } else {
        let _ = std::fs::remove_file(path);
      }
    }

    // Merge compile-time annotations with runtime annotations.
    let mut annotations = test.annotations.clone();
    annotations.extend(test_info.get_annotations().await);

    let outcome = TestOutcome {
      test_id: test_id.clone(),
      status,
      duration,
      attempt,
      max_attempts,
      error,
      attachments,
      steps,
      stdout: String::new(),
      stderr: String::new(),
      annotations,
      metadata: self.config.metadata.clone(),
    };

    self
      .event_bus
      .emit(ReporterEvent::TestFinished {
        test_id: test_id.clone(),
        outcome: outcome.clone(),
      })
      .await;

    let should_retry =
      outcome.status != TestStatus::Passed && outcome.status != TestStatus::Skipped && attempt < max_attempts;

    WorkerTestResult {
      outcome,
      should_retry,
      test_fn,
      test_id,
      fixture_requests,
      suite_key,
      hooks,
    }
  }
}

/// Sanitize a test name for use as a filename.
fn sanitize_filename(name: &str) -> String {
  name
    .chars()
    .map(|c| {
      if c.is_alphanumeric() || c == '-' || c == '_' {
        c
      } else {
        '_'
      }
    })
    .collect()
}

async fn capture_screenshot(page: &ferridriver::Page) -> Option<Vec<u8>> {
  let opts = ferridriver::options::ScreenshotOptions {
    full_page: Some(true),
    format: Some("png".into()),
    quality: None,
  };
  page.screenshot(opts).await.ok()
}

/// Evaluate an annotation condition string against the current environment.
///
/// Mirrors Playwright's fixture-based condition system. Conditions match against
/// the browser config (equivalent to Playwright's `browserName`, `headless`,
/// `isMobile`, etc. fixtures from the `use` block).
///
/// ## Supported conditions
///
/// **Browser name** (Playwright's `browserName` fixture):
/// - `"chromium"`, `"chrome"` — matches browser name "chromium"
/// - `"firefox"` — matches browser name "firefox"
/// - `"webkit"` — matches browser name "webkit"
///
/// **Browser channel** (Playwright's `channel` fixture):
/// - `"msedge"`, `"chrome-beta"`, `"chrome-canary"`
///
/// **OS / platform:**
/// - `"linux"`, `"macos"` / `"darwin"`, `"windows"` / `"win32"`
///
/// **Browser mode** (Playwright's `headless` fixture):
/// - `"headed"`, `"headless"`
///
/// **Context options** (Playwright's `use` block fixtures):
/// - `"mobile"` — `isMobile` is true
/// - `"touch"` — `hasTouch` is true
/// - `"dark"` — `colorScheme` is "dark"
/// - `"light"` — `colorScheme` is "light"
/// - `"offline"` — offline network mode
/// - `"bypass-csp"` — CSP bypass enabled
///
/// **Environment:**
/// - `"ci"` — `CI` env var is set
/// - `"debug"` — debug build (`cfg!(debug_assertions)`)
/// - `"env:VAR_NAME"` — generic env var check, true if set and non-empty
///
/// **Operators:**
/// - `"!condition"` — negation (invert any condition)
/// - `"cond1+cond2"` — conjunction (AND), all must match
fn evaluate_condition(condition: &str, browser: &crate::config::BrowserConfig) -> bool {
  let condition = condition.trim();

  // Negation: !condition
  if let Some(inner) = condition.strip_prefix('!') {
    return !evaluate_condition(inner, browser);
  }

  // Conjunction: condition1+condition2+...
  if condition.contains('+') {
    return condition.split('+').all(|part| evaluate_condition(part, browser));
  }

  match condition {
    // OS conditions.
    "linux" => cfg!(target_os = "linux"),
    "macos" | "darwin" => cfg!(target_os = "macos"),
    "windows" | "win32" => cfg!(target_os = "windows"),

    // Browser name (Playwright's browserName fixture).
    "chromium" | "chrome" => browser.browser == "chromium",
    "webkit" => browser.browser == "webkit",
    "firefox" => browser.browser == "firefox",

    // Browser channel (Playwright's channel fixture).
    "msedge" => browser.channel.as_deref() == Some("msedge"),
    "chrome-beta" => browser.channel.as_deref() == Some("chrome-beta"),
    "chrome-canary" => browser.channel.as_deref() == Some("chrome-canary"),

    // Browser mode.
    "headed" => !browser.headless,
    "headless" => browser.headless,

    // Context options (Playwright's use block fixtures).
    "mobile" => browser.context.is_mobile,
    "touch" => browser.context.has_touch,
    "dark" => browser.context.color_scheme.as_deref() == Some("dark"),
    "light" => browser.context.color_scheme.as_deref() == Some("light"),
    "offline" => browser.context.offline,
    "bypass-csp" => browser.context.bypass_csp,

    // Environment.
    "ci" => std::env::var("CI").is_ok(),
    "debug" => cfg!(debug_assertions),

    // Generic env var: `env:VAR_NAME` — true if the env var is set and non-empty.
    // Example: `@skip(env:SKIP_SLOW_TESTS)`, `#[ferritest(skip = "env:NO_GPU")]`
    other if other.starts_with("env:") => {
      let var_name = &other[4..];
      std::env::var(var_name).is_ok_and(|v| !v.is_empty())
    },

    // Unknown condition: don't match.
    _ => false,
  }
}
