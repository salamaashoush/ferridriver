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

use crate::config::{ContextConfig, TestConfig, ViewportConfig};
use crate::dispatcher::{SerialBatch, TestAssignment, WorkItem};
use crate::fixture::{FixtureDef, FixturePool, FixtureScope};
use crate::model::{
  Attachment, AttachmentBody, ExpectedStatus, Hooks, StepCategory, TestAnnotation, TestFailure, TestInfo, TestOutcome,
  TestStatus,
};
use crate::reporter::{EventBus, ReporterEvent};

struct PreparedPage {
  ctx: ferridriver::ContextRef,
  page_result: Result<Arc<ferridriver::Page>, String>,
}

#[derive(Clone)]
struct EffectiveContextConfig {
  context: ContextConfig,
  default_viewport: Option<ViewportConfig>,
  viewport_override: Option<ViewportConfig>,
  request_base_url: Option<String>,
}

enum TestBrowserState {
  Empty(Option<tokio::task::JoinHandle<PreparedPage>>),
  Context(Arc<ferridriver::ContextRef>),
  Page {
    ctx: Arc<ferridriver::ContextRef>,
    page: Arc<ferridriver::Page>,
  },
  Failed(String),
}

struct TestBrowserResources {
  browser: Arc<ferridriver::Browser>,
  effective: EffectiveContextConfig,
  output_dir: std::path::PathBuf,
  state: Mutex<TestBrowserState>,
}

fn is_retryable_bidi_page_error(err: &str) -> bool {
  err.contains("DiscardedBrowsingContextError")
    || err.contains("BrowsingContext does no longer exist")
    || err.contains("BiDi error 'no such frame'")
    || err.contains("BiDi error 'no such window'")
}

async fn ensure_page_alive(page: &Arc<ferridriver::Page>) -> Result<(), String> {
  page.evaluate("1").await.map(|_| ())
}

async fn create_ready_page(ctx: &ferridriver::ContextRef) -> Result<Arc<ferridriver::Page>, String> {
  let page = ctx.new_page().await?;
  ensure_page_alive(&page).await?;
  Ok(page)
}

impl TestBrowserResources {
  fn new(
    browser: Arc<ferridriver::Browser>,
    effective: EffectiveContextConfig,
    output_dir: std::path::PathBuf,
    prepared_page: Option<tokio::task::JoinHandle<PreparedPage>>,
  ) -> Self {
    Self {
      browser,
      effective,
      output_dir,
      state: Mutex::new(TestBrowserState::Empty(prepared_page)),
    }
  }

  async fn context(&self) -> Result<Arc<ferridriver::ContextRef>, String> {
    let mut state = self.state.lock().await;
    match &mut *state {
      TestBrowserState::Context(ctx) => Ok(Arc::clone(ctx)),
      TestBrowserState::Page { ctx, .. } => Ok(Arc::clone(ctx)),
      TestBrowserState::Failed(err) => Err(err.clone()),
      TestBrowserState::Empty(prepared_page) => {
        if let Some(handle) = prepared_page.take() {
          handle.abort();
        }
        let ctx = Arc::new(self.browser.new_context());
        *state = TestBrowserState::Context(Arc::clone(&ctx));
        Ok(ctx)
      },
    }
  }

  async fn page(&self) -> Result<Arc<ferridriver::Page>, String> {
    let mut state = self.state.lock().await;
    match &mut *state {
      TestBrowserState::Page { page, .. } => Ok(Arc::clone(page)),
      TestBrowserState::Failed(err) => Err(err.clone()),
      TestBrowserState::Context(ctx) => {
        let page = create_ready_page(ctx).await?;
        apply_page_config(&page, &self.effective, &self.output_dir).await?;
        let ctx = Arc::clone(ctx);
        *state = TestBrowserState::Page {
          ctx,
          page: Arc::clone(&page),
        };
        Ok(page)
      },
      TestBrowserState::Empty(prepared_page) => {
        let prepared = match prepared_page.take() {
          Some(handle) => match handle.await {
            Ok(prepared) => prepared,
            Err(_) => {
              let ctx = self.browser.new_context();
              let page_result = ctx.new_page().await;
              PreparedPage { ctx, page_result }
            },
          },
          None => {
            let ctx = self.browser.new_context();
            let page_result = ctx.new_page().await;
            PreparedPage { ctx, page_result }
          },
        };

        match prepared.page_result {
          Ok(page) => {
            if let Err(err) = ensure_page_alive(&page).await {
              if is_retryable_bidi_page_error(&err) {
                let _ = prepared.ctx.close().await;
                let ctx = Arc::new(self.browser.new_context());
                let page = create_ready_page(&ctx).await?;
                apply_page_config(&page, &self.effective, &self.output_dir).await?;
                *state = TestBrowserState::Page {
                  ctx,
                  page: Arc::clone(&page),
                };
                return Ok(page);
              }
              *state = TestBrowserState::Failed(err.clone());
              return Err(err);
            }
            apply_page_config(&page, &self.effective, &self.output_dir).await?;
            let ctx = Arc::new(prepared.ctx);
            *state = TestBrowserState::Page {
              ctx,
              page: Arc::clone(&page),
            };
            Ok(page)
          },
          Err(err) => {
            *state = TestBrowserState::Failed(err.clone());
            Err(err)
          },
        }
      },
    }
  }

  async fn close(&self) {
    let mut state = self.state.lock().await;
    match std::mem::replace(&mut *state, TestBrowserState::Empty(None)) {
      TestBrowserState::Empty(Some(handle)) => handle.abort(),
      TestBrowserState::Context(ctx) => {
        let _ = ctx.close().await;
      },
      TestBrowserState::Page { ctx, .. } => {
        let _ = ctx.close().await;
      },
      TestBrowserState::Empty(None) | TestBrowserState::Failed(_) => {},
    }
  }
}

fn build_effective_context_config(config: &TestConfig, test: &crate::model::TestCase) -> EffectiveContextConfig {
  let mut ctx_config = config.browser.context.clone();
  if let Some(ref opts) = test.use_options {
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
    if let Some(arr) = opts.get("permissions").and_then(|v| v.as_array()) {
      let perms: Vec<String> = arr.iter().filter_map(|v| v.as_str().map(String::from)).collect();
      if !perms.is_empty() {
        ctx_config.permissions = perms;
      }
    }
    if let Some(obj) = opts.get("extraHTTPHeaders").and_then(|v| v.as_object()) {
      let headers: std::collections::BTreeMap<String, String> = obj
        .iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
        .collect();
      if !headers.is_empty() {
        ctx_config.extra_http_headers = headers;
      }
    }
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

  let viewport_override = test.use_options.as_ref().and_then(|opts| {
    opts.get("viewport").and_then(|v| {
      let w = v.get("width").and_then(|w| w.as_i64());
      let h = v.get("height").and_then(|h| h.as_i64());
      match (w, h) {
        (Some(w), Some(h)) => Some(ViewportConfig { width: w, height: h }),
        _ => None,
      }
    })
  });

  let request_base_url = test
    .use_options
    .as_ref()
    .and_then(|opts| opts.get("baseURL").and_then(|v| v.as_str()).map(String::from))
    .or_else(|| config.base_url.clone());

  if ctx_config.storage_state.is_none() {
    ctx_config.storage_state = config.storage_state.clone();
  }

  EffectiveContextConfig {
    context: ctx_config,
    default_viewport: config.browser.viewport.clone(),
    viewport_override,
    request_base_url,
  }
}

fn build_suite_effective_context_config(config: &TestConfig) -> EffectiveContextConfig {
  let mut ctx_config = config.browser.context.clone();
  if ctx_config.storage_state.is_none() {
    ctx_config.storage_state = config.storage_state.clone();
  }

  EffectiveContextConfig {
    context: ctx_config,
    default_viewport: config.browser.viewport.clone(),
    viewport_override: None,
    request_base_url: config.base_url.clone(),
  }
}

async fn apply_page_config(
  page: &Arc<ferridriver::Page>,
  effective: &EffectiveContextConfig,
  output_dir: &std::path::Path,
) -> Result<(), String> {
  let ctx_config = &effective.context;
  let viewport = effective
    .viewport_override
    .as_ref()
    .or(effective.default_viewport.as_ref());

  let has_ctx_overrides = effective.viewport_override.is_some()
    || ctx_config.is_mobile
    || ctx_config.has_touch
    || ctx_config
      .device_scale_factor
      .is_some_and(|d| (d - 1.0).abs() > f64::EPSILON);
  if has_ctx_overrides {
    if let Some(vp) = viewport {
      page
        .set_viewport(&ferridriver::options::ViewportConfig {
          width: vp.width,
          height: vp.height,
          device_scale_factor: ctx_config.device_scale_factor.unwrap_or(1.0),
          is_mobile: ctx_config.is_mobile,
          has_touch: ctx_config.has_touch,
          is_landscape: ctx_config.is_mobile && vp.width > vp.height,
        })
        .await?;
    }
  }
  if ctx_config.color_scheme.is_some() {
    page
      .emulate_media(&ferridriver::options::EmulateMediaOptions {
        color_scheme: ctx_config.color_scheme.clone(),
        ..Default::default()
      })
      .await?;
  }
  if let Some(ref locale) = ctx_config.locale {
    page.set_locale(locale).await?;
  }
  if let Some(ref tz) = ctx_config.timezone_id {
    page.set_timezone(tz).await?;
  }
  if let Some(ref geo) = ctx_config.geolocation {
    page
      .set_geolocation(geo.latitude, geo.longitude, geo.accuracy.unwrap_or(0.0))
      .await?;
  }
  if ctx_config.offline {
    page.set_network_state(true, 0.0, -1.0, -1.0).await?;
  }
  if !ctx_config.permissions.is_empty() {
    page.grant_permissions(&ctx_config.permissions, None).await?;
  }
  if !ctx_config.extra_http_headers.is_empty() {
    let headers: rustc_hash::FxHashMap<String, String> = ctx_config
      .extra_http_headers
      .iter()
      .map(|(k, v)| (k.clone(), v.clone()))
      .collect();
    page.set_extra_http_headers(&headers).await?;
  }
  if let Some(ref ua) = ctx_config.user_agent {
    page.set_user_agent(ua).await?;
  }
  if !ctx_config.java_script_enabled {
    page.set_javascript_enabled(false).await?;
  }
  if ctx_config.reduced_motion.is_some() || ctx_config.forced_colors.is_some() {
    page
      .emulate_media(&ferridriver::options::EmulateMediaOptions {
        color_scheme: None,
        reduced_motion: ctx_config.reduced_motion.clone(),
        forced_colors: ctx_config.forced_colors.clone(),
        ..Default::default()
      })
      .await?;
  }
  if ctx_config.bypass_csp {
    page.set_bypass_csp(true).await?;
  }
  if ctx_config.ignore_https_errors {
    page.set_ignore_certificate_errors(true).await?;
  }
  if ctx_config.accept_downloads {
    let download_dir = output_dir.join("downloads");
    let _ = std::fs::create_dir_all(&download_dir);
    page
      .set_download_behavior("allowAndName", &download_dir.display().to_string())
      .await?;
  }
  if let Some(ref creds) = ctx_config.http_credentials {
    page.set_http_credentials(&creds.username, &creds.password).await?;
  }
  if ctx_config.service_workers.as_deref() == Some("block") {
    page.set_service_workers_blocked(true).await?;
  }
  if let Some(ss_path) = ctx_config.storage_state.as_deref() {
    let path = std::path::Path::new(ss_path);
    match std::fs::read_to_string(path) {
      Ok(json_str) => match serde_json::from_str::<serde_json::Value>(&json_str) {
        Ok(state) => page.set_storage_state(&state).await?,
        Err(e) => tracing::warn!(target: "ferridriver::worker", "parse storage state {}: {e}", path.display()),
      },
      Err(e) => tracing::warn!(target: "ferridriver::worker", "read storage state {}: {e}", path.display()),
    }
  }
  Ok(())
}

fn build_browser_fixture_defs(
  resources: Arc<TestBrowserResources>,
  scope: FixtureScope,
) -> FxHashMap<String, FixtureDef> {
  let mut defs = FxHashMap::default();

  defs.insert(
    "context".into(),
    FixtureDef {
      name: "context".into(),
      scope,
      dependencies: vec![],
      setup: Arc::new({
        let resources = Arc::clone(&resources);
        move |_pool| {
          let resources = Arc::clone(&resources);
          Box::pin(async move {
            let ctx = resources.context().await?;
            Ok(ctx as Arc<dyn std::any::Any + Send + Sync>)
          })
        }
      }),
      teardown: None,
      timeout: Duration::from_secs(10),
    },
  );

  defs.insert(
    "page".into(),
    FixtureDef {
      name: "page".into(),
      scope,
      dependencies: vec![],
      setup: Arc::new({
        let resources = Arc::clone(&resources);
        move |_pool| {
          let resources = Arc::clone(&resources);
          Box::pin(async move {
            let page = resources.page().await?;
            Ok(page as Arc<dyn std::any::Any + Send + Sync>)
          })
        }
      }),
      teardown: None,
      timeout: Duration::from_secs(10),
    },
  );

  defs.insert(
    "request".into(),
    FixtureDef {
      name: "request".into(),
      scope,
      dependencies: vec![],
      setup: Arc::new({
        let base_url = resources.effective.request_base_url.clone();
        move |_pool| {
          let base_url = base_url.clone();
          Box::pin(async move {
            Ok(Arc::new(ferridriver::api_request::APIRequestContext::new(
              ferridriver::api_request::RequestContextOptions {
                base_url,
                ..Default::default()
              },
            )) as Arc<dyn std::any::Any + Send + Sync>)
          })
        }
      }),
      teardown: None,
      timeout: Duration::from_secs(10),
    },
  );

  defs
}

fn build_test_fixture_defs(resources: Arc<TestBrowserResources>) -> FxHashMap<String, FixtureDef> {
  build_browser_fixture_defs(resources, FixtureScope::Test)
}

fn build_suite_fixture_defs(resources: Arc<TestBrowserResources>) -> FxHashMap<String, FixtureDef> {
  build_browser_fixture_defs(resources, FixtureScope::Worker)
}

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
  fixture_pool: FixturePool,
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

  fn spawn_prepared_page(browser: Arc<ferridriver::Browser>) -> tokio::task::JoinHandle<PreparedPage> {
    tokio::spawn(async move {
      let ctx = browser.new_context();
      let page_result = create_ready_page(&ctx).await;
      PreparedPage { ctx, page_result }
    })
  }

  fn create_suite_test_info(&self, suite_key: &str) -> Arc<TestInfo> {
    Arc::new(TestInfo {
      test_id: crate::model::TestId {
        file: suite_key.to_string(),
        suite: None,
        name: "suite hooks".to_string(),
        line: None,
      },
      title_path: vec![suite_key.to_string(), "suite hooks".to_string()],
      retry: 0,
      worker_index: self.id,
      parallel_index: self.id,
      repeat_each_index: 0,
      output_dir: self
        .config
        .output_dir
        .join("__suite_hooks__")
        .join(sanitize_filename(suite_key)),
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
      timeout: Duration::from_millis(self.config.timeout),
      tags: Vec::new(),
      start_time: Instant::now(),
      event_bus: Some(self.event_bus.clone()),
      annotations: Arc::new(Mutex::new(Vec::new())),
    })
  }

  #[tracing::instrument(skip_all, fields(worker_id = self.id))]
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
    custom_fixture_pool.inject("browser", Arc::clone(&browser));

    let mut active_suites: FxHashMap<String, SuiteState> = FxHashMap::default();
    let mut prepared_page = Some(Self::spawn_prepared_page(Arc::clone(&browser)));

    while let Ok(item) = rx.recv().await {
      match item {
        WorkItem::Single(assignment) => {
          let result = self
            .run_single(
              &browser,
              &custom_fixture_pool,
              &mut active_suites,
              &mut prepared_page,
              assignment,
            )
            .await;
          if result_tx.send(result).await.is_err() {
            break;
          }
        },
        WorkItem::Serial(batch) => {
          let results = self
            .run_serial_batch(
              &browser,
              &custom_fixture_pool,
              &mut active_suites,
              &mut prepared_page,
              batch,
            )
            .await;
          for result in results {
            if result_tx.send(result).await.is_err() {
              break;
            }
          }
        },
      }
    }

    if let Some(handle) = prepared_page.take() {
      match handle.await {
        Ok(prepared) => {
          let _ = prepared.ctx.close().await;
        },
        Err(_) => {},
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
          let result = hook(state.fixture_pool.clone()).await;
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

    for state in active_suites.values() {
      state.fixture_pool.teardown_all().await;
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
    prepared_page: &mut Option<tokio::task::JoinHandle<PreparedPage>>,
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

      let result = self
        .run_single(browser, custom_pool, active_suites, prepared_page, assignment)
        .await;
      if result.outcome.status == TestStatus::Failed || result.outcome.status == TestStatus::TimedOut {
        serial_failed = true;
      }
      results.push(result);
    }

    results
  }

  /// Run a single test with full hook lifecycle.
  #[tracing::instrument(skip_all, fields(worker_id = self.id, test, attempt = assignment.attempt))]
  async fn run_single(
    &self,
    browser: &Arc<ferridriver::Browser>,
    custom_pool: &FixturePool,
    active_suites: &mut FxHashMap<String, SuiteState>,
    prepared_page: &mut Option<tokio::task::JoinHandle<PreparedPage>>,
    assignment: TestAssignment,
  ) -> WorkerTestResult {
    let test = &assignment.test;
    let test_id = test.id.clone();
    tracing::Span::current().record("test", &test_id.full_name().as_str());
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
    let suite_state = active_suites.entry(suite_key.clone()).or_insert_with(|| {
      let suite_test_info = self.create_suite_test_info(&suite_key);
      let suite_resources = Arc::new(TestBrowserResources::new(
        Arc::clone(browser),
        build_suite_effective_context_config(&self.config),
        suite_test_info.output_dir.clone(),
        None,
      ));
      let suite_pool = custom_pool.child_with_defs(build_suite_fixture_defs(suite_resources), FixtureScope::Worker);
      suite_pool.inject("test_info", suite_test_info);

      SuiteState {
        before_all_ran: false,
        before_all_failed: false,
        hooks: Arc::clone(&hooks),
        fixture_pool: suite_pool,
      }
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
        let result = hook(suite_state.fixture_pool.clone()).await;
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
    let effective_config = build_effective_context_config(&self.config, test);
    let current_prepared_page = prepared_page.take();
    *prepared_page = Some(Self::spawn_prepared_page(Arc::clone(browser)));

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
    let resources = Arc::new(TestBrowserResources::new(
      Arc::clone(browser),
      effective_config,
      test_info.output_dir.clone(),
      current_prepared_page,
    ));
    let test_pool = custom_pool.child_with_defs(build_test_fixture_defs(Arc::clone(&resources)), FixtureScope::Test);
    test_pool.inject("test_info", Arc::clone(&test_info));

    enum VideoHandle {
      Eager(ferridriver::video::VideoRecordingHandle),
      Buffered(ferridriver::video::BufferedRecordingHandle),
    }

    let mut page_for_artifacts = None;
    let video_handle: Option<VideoHandle> = match self.config.video.mode {
      crate::config::VideoMode::Off => None,
      crate::config::VideoMode::On | crate::config::VideoMode::RetainOnFailure => {
        match test_pool.get::<ferridriver::Page>("page").await {
          Ok(page) => {
            page_for_artifacts = Some(Arc::clone(&page));
            let _ = std::fs::create_dir_all(&test_info.output_dir);
            match self.config.video.mode {
              crate::config::VideoMode::On => {
                let ext = ferridriver::video::video_extension();
                let video_path =
                  test_info
                    .output_dir
                    .join(format!("{}-attempt{}.{ext}", sanitize_filename(&test_id.name), attempt));
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
              crate::config::VideoMode::Off => None,
            }
          },
          Err(e) => {
            let _ = resources.close().await;
            let duration = start.elapsed();
            let outcome = TestOutcome {
              test_id: test_id.clone(),
              status: TestStatus::Failed,
              duration,
              attempt,
              max_attempts,
              error: Some(TestFailure::from(format!("failed to create page: {e}"))),
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
              should_retry: attempt <= max_retries,
              test_fn,
              test_id,
              fixture_requests,
              suite_key,
              hooks,
            };
          },
        }
      },
    };

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

    let timeout_result = if let Some(err) = before_each_err {
      Ok(Err(err))
    } else {
      tokio::time::timeout(timeout_dur, (test.test_fn)(test_pool.clone())).await
    };

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

    if page_for_artifacts.is_none() {
      page_for_artifacts = test_pool.try_get_cached::<ferridriver::Page>("page");
    }
    let test_failed = timeout_result.as_ref().is_err() || timeout_result.as_ref().is_ok_and(|r| r.is_err());
    let screenshot = if test_failed {
      if let Some(ref page) = page_for_artifacts {
        capture_screenshot(page).await
      } else {
        None
      }
    } else {
      None
    };
    let video_path = match (video_handle, page_for_artifacts.as_ref()) {
      (Some(VideoHandle::Eager(handle)), Some(page)) => match handle.stop(page).await {
        Ok(path) => Some(path),
        Err(e) => {
          tracing::warn!(target: "ferridriver::worker", "video stop failed: {e}");
          None
        },
      },
      (Some(VideoHandle::Buffered(handle)), Some(page)) => {
        if test_failed {
          let ext = ferridriver::video::video_extension();
          let video_path =
            test_info
              .output_dir
              .join(format!("{}-attempt{}.{ext}", sanitize_filename(&test_id.name), attempt));
          let _ = std::fs::create_dir_all(&test_info.output_dir);
          match handle.encode(page, video_path).await {
            Ok(path) => Some(path),
            Err(e) => {
              tracing::warn!(target: "ferridriver::worker", "video encode failed: {e}");
              None
            },
          }
        } else {
          handle.discard(page).await;
          None
        }
      },
      _ => None,
    };
    resources.close().await;

    let duration = start.elapsed();
    let result = (timeout_result, screenshot, video_path, Some(test_pool));
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
