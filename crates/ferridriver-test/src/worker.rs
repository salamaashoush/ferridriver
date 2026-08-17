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
use std::time::{Duration, Instant, SystemTime};

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

#[derive(Clone)]
struct EffectiveContextConfig {
  context: ContextConfig,
  default_viewport: Option<ViewportConfig>,
  viewport_override: Option<ViewportConfig>,
  request_base_url: Option<String>,
}

impl EffectiveContextConfig {
  /// Identity of the contexts this config produces, for
  /// [`crate::context_pool`]. Everything `build_context_options` reads
  /// goes in, so two tests share a key exactly when a context built for
  /// one is a valid context for the other. Serializing rather than
  /// hashing keeps a new config field from silently falling out of the
  /// key the way a hand-written `Hash` impl would.
  fn pool_key(&self, backend: ferridriver::backend::BackendKind) -> crate::context_pool::PoolKey {
    let parts = serde_json::json!({
      "backend": format!("{backend:?}"),
      "context": &self.context,
      "viewport": self.viewport_override.as_ref().or(self.default_viewport.as_ref()),
      "baseUrl": &self.request_base_url,
    });
    parts.to_string()
  }
}

enum TestBrowserState {
  Empty,
  Context(Arc<ferridriver::ContextRef>),
  Page {
    ctx: Arc<ferridriver::ContextRef>,
    page: Arc<ferridriver::Page>,
  },
  Failed(ferridriver::FerriError),
}

struct TestBrowserResources {
  handle: Arc<crate::runner::BrowserHandle>,
  effective: EffectiveContextConfig,
  output_dir: std::path::PathBuf,
  state: Mutex<TestBrowserState>,
  trace: Option<TraceSpec>,
  pool: Arc<crate::context_pool::ContextPool>,
  /// The test asked for the `page` fixture, so resolving `context` first
  /// may as well take a pooled context+page pair — Playwright's `page`
  /// fixture is `context.newPage()`, so the pair is what the test ends up
  /// with either way. A test that wants only `context` must not be handed
  /// a page it never opened, so it takes the un-pooled path.
  wants_page: bool,
}

/// Per-test trace recording request: tracing starts the moment the
/// test's context materializes (contexts are created lazily by the
/// page/context fixtures) and publishes the composite key so
/// `TestInfo` step spans and the worker's stop path find the recorder.
struct TraceSpec {
  title: String,
  /// Trace stream name — the test's stable id, so a viewer can find the
  /// recording on disk while it is still being written.
  name: String,
  /// This worker's in-progress trace directory.
  traces_dir: std::path::PathBuf,
  /// Flush each event as it happens, for a UI following the run.
  live: bool,
  composite: Arc<std::sync::Mutex<Option<String>>>,
}

pub(crate) fn is_retryable_bidi_page_error(err: &ferridriver::FerriError) -> bool {
  let s = err.to_string();
  s.contains("DiscardedBrowsingContextError")
    || s.contains("BrowsingContext does no longer exist")
    || s.contains("BiDi error 'no such frame'")
    || s.contains("BiDi error 'no such window'")
}

async fn ensure_page_alive(page: &Arc<ferridriver::Page>) -> ferridriver::Result<()> {
  // Health check via raw `Runtime.evaluate("1")` — only fired when
  // [`needs_alive_check`] returns true. CDP backends don't need it:
  // `Target.attachedToTarget` only fires after the renderer's V8
  // context is up, and the per-page `enable_domains` parallel batch
  // (Page.enable + Runtime.enable) returns only when the V8 context
  // is ready to accept commands. Keep the check for BiDi where the
  // startup sequence is genuinely racy (Firefox occasionally returns
  // `BrowsingContext` before its underlying `Window` is fully wired
  // up — observed in `is_retryable_bidi_page_error`).
  page.inner().evaluate("1").await.map(|_| ())
}

/// Returns true when [`ensure_page_alive`] should fire on a freshly
/// created page. Only BiDi needs the probe; CDP and Playwright WebKit
/// pages skip the check (~1 RTT per test saved).
fn needs_alive_check(backend: ferridriver::backend::BackendKind) -> bool {
  matches!(backend, ferridriver::backend::BackendKind::Bidi)
}

pub(crate) async fn create_ready_page(
  ctx: &ferridriver::ContextRef,
  backend: ferridriver::backend::BackendKind,
) -> ferridriver::error::Result<Arc<ferridriver::Page>> {
  let page = ctx.new_page().await?;
  if needs_alive_check(backend) {
    ensure_page_alive(&page).await?;
  }
  Ok(page)
}

impl TestBrowserResources {
  fn new(
    handle: Arc<crate::runner::BrowserHandle>,
    effective: EffectiveContextConfig,
    output_dir: std::path::PathBuf,
    trace: Option<TraceSpec>,
    pool: Arc<crate::context_pool::ContextPool>,
    wants_page: bool,
  ) -> Self {
    Self {
      handle,
      effective,
      output_dir,
      state: Mutex::new(TestBrowserState::Empty),
      trace,
      pool,
      wants_page,
    }
  }

  /// Take a context+page pair for this test, from the pool when one was
  /// pre-created with matching options and inline otherwise.
  ///
  /// Backends that share the persistent default context have nothing to
  /// pool — there is only ever one container — so they fall through to
  /// the un-pooled path.
  async fn acquire_pooled(
    &self,
    browser: &Arc<ferridriver::Browser>,
  ) -> Option<ferridriver::error::Result<(Arc<ferridriver::ContextRef>, Arc<ferridriver::Page>)>> {
    if !browser.supports_isolated_contexts() {
      return None;
    }
    let backend = browser.backend_kind();
    let opts = build_context_options(&self.effective, &self.output_dir, backend);
    let key = self.effective.pool_key(backend);
    Some(Box::pin(self.pool.acquire(browser, &key, &opts, backend)).await)
  }

  /// Start the per-test trace on a freshly created context.
  async fn start_tracing(&self, ctx: &ferridriver::ContextRef) {
    let Some(spec) = &self.trace else { return };
    let options = ferridriver::trace::TracingStartOptions {
      name: Some(spec.name.clone()),
      title: Some(spec.title.clone()),
      screenshots: true,
      snapshots: true,
      // Steps carry their .feature / call-site stack frames; embedding
      // the referenced files lights up the viewer's Source tab.
      sources: true,
      streaming: ferridriver::trace::TraceStreaming::from_live(spec.live),
    };
    ctx.set_traces_dir(spec.traces_dir.clone()).await;
    match ctx.tracing().start(options).await {
      Ok(()) => {
        let composite = ctx.composite();
        *spec.composite.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(composite.clone());
        // Publish for the UI server's live-trace snapshot endpoint (the
        // `bdd --ui` viewer polls it while the test runs). Keyed by the
        // test's full name — the same id the client sees on testStarted.
        crate::ui_server::register_live_trace(&spec.title, &composite);
      },
      Err(e) => tracing::warn!(target: "ferridriver::worker", "trace start failed: {e}"),
    }
  }

  /// Discard a live trace on a context that is being torn down without
  /// the worker's explicit stop (page-creation failure, BiDi retry).
  async fn discard_tracing(&self, ctx: &ferridriver::ContextRef) {
    let Some(spec) = &self.trace else { return };
    let started = spec
      .composite
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .take()
      .is_some();
    if started {
      crate::ui_server::unregister_live_trace(&spec.title);
      let _ = ctx
        .tracing()
        .stop(ferridriver::trace::TracingStopOptions::default())
        .await;
    }
  }

  /// The test's live context, if one has been created. Never creates.
  async fn current_context(&self) -> Option<Arc<ferridriver::ContextRef>> {
    let state = self.state.lock().await;
    match &*state {
      TestBrowserState::Context(ctx) | TestBrowserState::Page { ctx, .. } => Some(Arc::clone(ctx)),
      TestBrowserState::Empty | TestBrowserState::Failed(_) => None,
    }
  }

  async fn context(&self) -> ferridriver::error::Result<Arc<ferridriver::ContextRef>> {
    let mut state = self.state.lock().await;
    match &mut *state {
      TestBrowserState::Context(ctx) => Ok(Arc::clone(ctx)),
      TestBrowserState::Page { ctx, .. } => Ok(Arc::clone(ctx)),
      TestBrowserState::Failed(err) => Err(err.clone()),
      TestBrowserState::Empty => {
        let browser = self.handle.get().await?;
        if self.wants_page
          && let Some(pooled) = Box::pin(self.acquire_pooled(&browser)).await
        {
          let (ctx, page) = pooled?;
          self.start_tracing(&ctx).await;
          *state = TestBrowserState::Page {
            ctx: Arc::clone(&ctx),
            page,
          };
          return Ok(ctx);
        }
        let opts = build_context_options(&self.effective, &self.output_dir, browser.backend_kind());
        let ctx = Arc::new(new_test_context(&browser, opts).await?);
        self.start_tracing(&ctx).await;
        *state = TestBrowserState::Context(Arc::clone(&ctx));
        Ok(ctx)
      },
    }
  }

  #[tracing::instrument(skip_all, name = "page_fixture")]
  async fn page(&self) -> ferridriver::error::Result<Arc<ferridriver::Page>> {
    let mut state = self.state.lock().await;
    match &mut *state {
      TestBrowserState::Page { page, .. } => Ok(Arc::clone(page)),
      TestBrowserState::Failed(err) => Err(err.clone()),
      TestBrowserState::Context(ctx) => {
        let browser = self.handle.get().await?;
        let backend = browser.backend_kind();
        let page = create_ready_page(ctx, backend).await?;
        let ctx = Arc::clone(ctx);
        *state = TestBrowserState::Page {
          ctx,
          page: Arc::clone(&page),
        };
        Ok(page)
      },
      TestBrowserState::Empty => {
        let browser = self.handle.get().await?;
        let backend = browser.backend_kind();
        if let Some(pooled) = Box::pin(self.acquire_pooled(&browser)).await {
          let (ctx, page) = pooled?;
          self.start_tracing(&ctx).await;
          *state = TestBrowserState::Page {
            ctx,
            page: Arc::clone(&page),
          };
          return Ok(page);
        }
        let opts = build_context_options(&self.effective, &self.output_dir, backend);
        let ctx = Arc::new(new_test_context(&browser, opts.clone()).await?);
        self.start_tracing(&ctx).await;
        match create_ready_page(&ctx, backend).await {
          Ok(page) => {
            *state = TestBrowserState::Page {
              ctx: Arc::clone(&ctx),
              page: Arc::clone(&page),
            };
            Ok(page)
          },
          Err(err) => {
            if is_retryable_bidi_page_error(&err) {
              self.discard_tracing(&ctx).await;
              let _ = ctx.close().await;
              let ctx = Arc::new(new_test_context(&browser, opts).await?);
              self.start_tracing(&ctx).await;
              let page = create_ready_page(&ctx, backend).await?;
              *state = TestBrowserState::Page {
                ctx,
                page: Arc::clone(&page),
              };
              return Ok(page);
            }
            *state = TestBrowserState::Failed(err.clone());
            Err(err)
          },
        }
      },
    }
  }

  async fn close(&self) {
    let mut state = self.state.lock().await;
    match std::mem::replace(&mut *state, TestBrowserState::Empty) {
      TestBrowserState::Context(ctx) => {
        self.discard_tracing(&ctx).await;
        close_test_context(&ctx).await;
      },
      TestBrowserState::Page { ctx, page } => {
        self.discard_tracing(&ctx).await;
        // When a backend shares the persistent default context the
        // page is the only per-test resource we own — closing the
        // context itself would tear down the persistent default and
        // break later tests. For isolated-context backends (CDP, BiDi,
        // Playwright WebKit) the context's `disposeBrowserContext`
        // already closes every page in it, so an explicit `page.close()`
        // would only add a redundant `closeTarget` round-trip per test
        // (~3-5ms each on the bench's tight loop).
        if ctx.name() == "default" {
          let _ = page.close().await;
        } else {
          drop(page);
        }
        close_test_context(&ctx).await;
      },
      TestBrowserState::Empty | TestBrowserState::Failed(_) => {},
    }
  }
}

/// Open a per-test browsing container. Backends that support
/// isolated contexts get a fresh `Browser::new_context(None)`. All
/// current backends — CDP pipe, CDP raw, BiDi/Firefox, and Playwright
/// WebKit — create real isolated contexts; the shared-default fallback
/// remains for any future backend that reports otherwise.
async fn new_test_context(
  browser: &Arc<ferridriver::Browser>,
  opts: ferridriver::options::BrowserContextOptions,
) -> ferridriver::error::Result<ferridriver::ContextRef> {
  if browser.supports_isolated_contexts() {
    browser.new_context().options(opts).await
  } else {
    tracing::warn!(
      target: "ferridriver::worker",
      "backend shares a default context — per-test context options are not applied",
    );
    Ok(browser.default_context())
  }
}

/// Drop a per-test context. Skips `ctx.close()` when the context is
/// the shared default container — closing it would tear down the
/// only browsing context available on a backend that shares the
/// persistent default. All current backends use isolated contexts, so
/// this guard only fires for a shared-default fallback.
async fn close_test_context(ctx: &ferridriver::ContextRef) {
  if ctx.name() == "default" {
    return;
  }
  let _ = ctx.close().await;
}

fn build_effective_context_config(config: &TestConfig, test: &crate::model::TestCase) -> EffectiveContextConfig {
  let mut ctx_config = config.browser.use_options.clone();
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
    if let Some(v) = opts.get("testIdAttribute").and_then(|v| v.as_str()) {
      ctx_config.test_id_attribute = Some(v.to_string());
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
    if let Some(geo) = opts.get("geolocation").and_then(|v| v.as_object())
      && let (Some(lat), Some(lon)) = (
        geo.get("latitude").and_then(|v| v.as_f64()),
        geo.get("longitude").and_then(|v| v.as_f64()),
      )
    {
      ctx_config.geolocation = Some(crate::config::GeolocationConfig {
        latitude: lat,
        longitude: lon,
        accuracy: geo.get("accuracy").and_then(|v| v.as_f64()),
      });
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
    if let Some(creds) = opts.get("httpCredentials").and_then(|v| v.as_object())
      && let (Some(user), Some(pass)) = (
        creds.get("username").and_then(|v| v.as_str()),
        creds.get("password").and_then(|v| v.as_str()),
      )
    {
      ctx_config.http_credentials = Some(crate::config::HttpCredentialsConfig {
        username: user.to_string(),
        password: pass.to_string(),
        origin: creds.get("origin").and_then(|v| v.as_str()).map(String::from),
      });
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
    .or_else(|| config.base_url.clone())
    .or_else(crate::config::base_url_from_env);

  if ctx_config.storage_state.is_none() {
    ctx_config.storage_state.clone_from(&config.storage_state);
  }

  EffectiveContextConfig {
    context: ctx_config,
    default_viewport: config.browser.viewport.clone(),
    viewport_override,
    request_base_url,
  }
}

fn build_suite_effective_context_config(config: &TestConfig) -> EffectiveContextConfig {
  let mut ctx_config = config.browser.use_options.clone();
  if ctx_config.storage_state.is_none() {
    ctx_config.storage_state.clone_from(&config.storage_state);
  }

  EffectiveContextConfig {
    context: ctx_config,
    default_viewport: config.browser.viewport.clone(),
    viewport_override: None,
    request_base_url: config.base_url.clone().or_else(crate::config::base_url_from_env),
  }
}

/// Lower the effective test config into the `BrowserContextOptions` bag
/// passed to `browser.new_context()`. Creation-time options matter:
/// document-time overrides (locale, userAgent, timezone) must be in the
/// context's stashed options BEFORE the first page's process spawns —
/// WebKit in particular latches languages at target creation and a
/// post-attach `Playwright.setLanguages` never reaches the already
/// running web process.
fn build_context_options(
  effective: &EffectiveContextConfig,
  output_dir: &std::path::Path,
  backend_kind: ferridriver::backend::BackendKind,
) -> ferridriver::options::BrowserContextOptions {
  let ctx_config = &effective.context;
  let mut opts = ferridriver::options::BrowserContextOptions::default();
  // Playwright WebKit rejects several context-options fields outright
  // on launchPersistentContext; degrade silently when the user hasn't
  // explicitly opted in.
  let is_webkit = matches!(backend_kind, ferridriver::backend::BackendKind::WebKit);

  let viewport = effective
    .viewport_override
    .as_ref()
    .or(effective.default_viewport.as_ref());
  if let Some(vp) = viewport {
    opts.viewport = ferridriver::options::ViewportOption::Size {
      width: vp.width,
      height: vp.height,
    };
  }
  opts.device_scale_factor = ctx_config.device_scale_factor;
  if ctx_config.is_mobile {
    opts.is_mobile = Some(true);
  }
  if ctx_config.has_touch {
    opts.has_touch = Some(true);
  }
  opts.color_scheme = ctx_config.color_scheme.clone().into();
  opts.reduced_motion = ctx_config.reduced_motion.clone().into();
  opts.forced_colors = ctx_config.forced_colors.clone().into();
  opts.locale.clone_from(&ctx_config.locale);
  opts.timezone_id.clone_from(&ctx_config.timezone_id);
  if let Some(ref geo) = ctx_config.geolocation {
    opts.geolocation = Some(ferridriver::options::Geolocation {
      latitude: geo.latitude,
      longitude: geo.longitude,
      accuracy: geo.accuracy.unwrap_or(0.0),
    });
  }
  if ctx_config.offline {
    opts.offline = Some(true);
  }
  if !ctx_config.permissions.is_empty() {
    opts.permissions = Some(ctx_config.permissions.clone());
  }
  if !ctx_config.extra_http_headers.is_empty() {
    opts.extra_http_headers = Some(
      ctx_config
        .extra_http_headers
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect(),
    );
  }
  opts.user_agent.clone_from(&ctx_config.user_agent);
  opts.test_id_attribute.clone_from(&ctx_config.test_id_attribute);
  // Plumb the test config's `baseURL` into the BrowserContext bag so
  // `page.goto('/route')` resolves against it. Previously the value
  // was only stored as `request_base_url` for the API-request
  // fixture, leaving relative `page.goto` paths to fail with "Cannot
  // navigate to invalid URL" — Playwright resolves these via the
  // context's baseURL option, mirror that.
  if opts.base_url.is_none() {
    opts.base_url.clone_from(&effective.request_base_url);
  }
  if !ctx_config.java_script_enabled {
    opts.java_script_enabled = Some(false);
  }
  if ctx_config.bypass_csp && !is_webkit {
    opts.bypass_csp = Some(true);
  }
  if ctx_config.ignore_https_errors && !is_webkit {
    opts.ignore_https_errors = Some(true);
  }
  // Note: `ctx_config.accept_downloads` defaults to `true` (Playwright
  // parity). We deliberately don't pass that through to
  // `BrowserContextOptions.accept_downloads` here — doing so makes
  // `apply_context_options` fire `Browser.setDownloadBehavior` on
  // every per-test page, which is ~3-5ms per test on the bench's
  // tight loop. The page-level lazy `enable_download_behavior` (fired
  // on first `wait_for_download` / `page.on('download')`) handles the
  // CDP command when a test actually needs it. Tests that opt OUT
  // (`acceptDownloads: false`) still flow through, since opts.deny is
  // an explicit decision the bag has to encode.
  if !ctx_config.accept_downloads && !is_webkit {
    opts.accept_downloads = Some(false);
  }
  if ctx_config.accept_downloads && !is_webkit {
    let _ = std::fs::create_dir_all(output_dir.join("downloads"));
  }
  if let Some(ref creds) = ctx_config.http_credentials {
    opts.http_credentials = Some(ferridriver::options::HttpCredentials {
      username: creds.username.clone(),
      password: creds.password.clone(),
      origin: None,
      send: None,
    });
  }
  if ctx_config.service_workers.as_deref() == Some("block") {
    opts.service_workers = Some(ferridriver::options::ServiceWorkerPolicy::Block);
  }
  if let Some(ss_path) = ctx_config.storage_state.as_deref() {
    opts.storage_state = Some(ferridriver::options::StorageStateInput::Path(ss_path.into()));
  }

  opts
}

/// Worker-scope `browser` fixture backed by `BrowserHandle`. Added to the
/// custom_fixture_pool so every child suite/test pool can resolve it via
/// the parent chain. Lazy: launches on first `get("browser")`.
fn build_worker_browser_def(handle: Arc<crate::runner::BrowserHandle>) -> FixtureDef {
  FixtureDef {
    name: "browser".into(),
    scope: FixtureScope::Worker,
    dependencies: vec![],
    setup: Arc::new(move |_pool| {
      let handle = Arc::clone(&handle);
      Box::pin(async move {
        let browser = handle.get().await?;
        Ok(browser as Arc<dyn std::any::Any + Send + Sync>)
      })
    }),
    teardown: None,
    timeout: Duration::from_secs(30),
    auto: false,
  }
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
            let ctx = Box::pin(resources.context()).await?;
            Ok(ctx as Arc<dyn std::any::Any + Send + Sync>)
          })
        }
      }),
      teardown: None,
      timeout: Duration::from_secs(10),
      auto: false,
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
            let page = Box::pin(resources.page()).await?;
            Ok(page as Arc<dyn std::any::Any + Send + Sync>)
          })
        }
      }),
      teardown: None,
      timeout: Duration::from_secs(10),
      auto: false,
    },
  );

  defs
}

/// Worker-scope `request` fixture. Builds one [`HttpClient`] per worker
/// so the underlying reqwest connection pool, TLS context, and cookie
/// jar are reused across every test on this worker — saves the per-test
/// `reqwest::Client::builder().build()` cost (~1-10ms each on the bench).
///
/// `base_url` is captured from the worker's config; per-test
/// `use_options.base_url` overrides aren't honored at this scope. Tests
/// that need a different base URL should construct an `HttpClient`
/// inside the test body, or we expose a per-test override fixture
/// later (Playwright's `request` fixture has the same worker-scoped
/// shape — `playwright/types/test.d.ts` `APIRequestContext`).
fn build_worker_request_def(base_url: Option<String>) -> FixtureDef {
  FixtureDef {
    name: "request".into(),
    scope: FixtureScope::Worker,
    dependencies: vec![],
    setup: Arc::new(move |_pool| {
      let base_url = base_url.clone();
      Box::pin(async move {
        Ok(Arc::new(ferridriver::http_client::HttpClient::new(
          ferridriver::http_client::HttpClientOptions {
            base_url,
            ..Default::default()
          },
        )) as Arc<dyn std::any::Any + Send + Sync>)
      })
    }),
    teardown: None,
    timeout: Duration::from_secs(10),
    auto: false,
  }
}

fn build_test_fixture_defs(resources: Arc<TestBrowserResources>) -> FxHashMap<String, FixtureDef> {
  build_browser_fixture_defs(resources, FixtureScope::Test)
}

fn build_suite_fixture_defs(resources: Arc<TestBrowserResources>) -> FxHashMap<String, FixtureDef> {
  build_browser_fixture_defs(resources, FixtureScope::Worker)
}

/// Result of a single test execution within a worker.
pub struct WorkerTestResult {
  /// Shared with the reporter event that carried it: an outcome holds
  /// the attempt's screenshots and step tree, and copying it per
  /// consumer is the single largest allocation a finished test makes.
  pub outcome: Arc<TestOutcome>,
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
  /// Unique across the whole run, including projects executing at the
  /// same time: it names this worker's scratch directory and is what a
  /// UI is told, so two workers sharing a number would share artifacts.
  pub id: u32,
  /// Which of this runner's worker slots this is (`0..workers`). The
  /// number a test sees as `parallelIndex`.
  pub slot: u32,
  config: Arc<TestConfig>,
  event_bus: Option<EventBus>,
  /// Flush trace events as they happen, because something is watching
  /// this run (a UI following the live trace). Off for a plain run: the
  /// trace is only read once it has been zipped, so buffering wins.
  live_traces: bool,
  /// Contexts pre-created for this worker's upcoming tests. Shared by
  /// every test the worker runs; drained when the worker exits.
  pool: Arc<crate::context_pool::ContextPool>,
}

/// Directory-safe name for a test's artifact folder under `outputDir`.
/// Titles and file paths are user-controlled and may contain path
/// separators or `..` — folding each path-hostile component keeps every
/// artifact inside `outputDir` (Playwright sanitizes the same way).
fn artifact_dir_name(full_name: &str) -> String {
  full_name
    .chars()
    .map(|c| match c {
      '/' | '\\' | ':' => '-',
      c if c.is_control() => '-',
      c => c,
    })
    .collect::<String>()
    .replace("..", "-")
}

/// `TestInfo.outputDir` / `snapshotDir` are user-facing and absolute in
/// Playwright (`testInfo.outputDir: "Absolute path to a directory..."`);
/// the config keeps its relative form for display. A relative dir also
/// breaks consumers that resolve paths against a different working
/// directory — e.g. the browser stat'ing a `setInputFiles` path.
fn absolutize(p: std::path::PathBuf) -> std::path::PathBuf {
  std::path::absolute(&p).unwrap_or(p)
}

impl Worker {
  pub fn new(id: u32, slot: u32, config: Arc<TestConfig>, event_bus: Option<EventBus>, live_traces: bool) -> Self {
    let pool = crate::context_pool::ContextPool::new(config.context_prewarm as usize);
    Self {
      id,
      slot,
      config,
      event_bus,
      live_traces,
      pool,
    }
  }

  /// The run facts every outcome this worker produces carries: which
  /// worker and slot ran it, under which project, when it started, and
  /// what it was declared to do. Sites fill the rest.
  fn outcome_base(&self, test: &crate::model::TestCase, started_at: SystemTime) -> TestOutcome {
    TestOutcome {
      project_name: self.config.name.clone().unwrap_or_default(),
      worker_index: self.id,
      parallel_index: self.slot,
      start_time: started_at,
      expected_status: test.expected_status,
      timeout: test
        .timeout
        .unwrap_or_else(|| Duration::from_millis(self.config.timeout)),
      metadata: self.config.metadata.clone(),
      ..Default::default()
    }
  }

  /// The `expect` block this worker's project resolved to. Every
  /// assertion a test makes — in a hook, in the body, in a fixture —
  /// defaults from it, which is why the whole test runs inside the
  /// scope rather than just the body.
  fn expect_config(&self) -> Arc<crate::config::ExpectConfig> {
    Arc::new(self.config.resolved_expect(None))
  }

  fn create_suite_test_info(&self, suite_key: &str) -> Arc<TestInfo> {
    Arc::new(TestInfo {
      test_id: crate::model::TestId {
        file: suite_key.to_string(),
        suite: None,
        name: "suite hooks".to_string(),
        line: None,
        column: None,
      },
      title_path: vec![suite_key.to_string(), "suite hooks".to_string()],
      retry: 0,
      worker_index: self.id,
      parallel_index: self.slot,
      repeat_each_index: 0,
      output_dir: absolutize(
        self
          .config
          .output_dir
          .join("__suite_hooks__")
          .join(sanitize_filename(suite_key)),
      ),
      snapshot_dir: absolutize(
        self
          .config
          .snapshot_dir
          .as_ref()
          .map(std::path::PathBuf::from)
          .unwrap_or_else(|| std::path::PathBuf::from("__snapshots__")),
      ),
      snapshot_path_template: self.config.snapshot_path_template.clone(),
      update_snapshots: self.config.update_snapshots,
      ignore_snapshots: self.config.ignore_snapshots,
      attachments: Arc::new(Mutex::new(Vec::new())),
      steps: Arc::new(Mutex::new(Vec::new())),
      soft_errors: Arc::new(std::sync::Mutex::new(Vec::new())),
      errors: Arc::new(Mutex::new(Vec::new())),
      snapshot_suffix: Arc::new(Mutex::new(String::new())),
      column: None,
      project: None,
      config_snapshot: Some(Arc::clone(&self.config)),
      expect: Arc::new(self.config.resolved_expect(None)),
      config_dir: self.config.config_dir.clone().unwrap_or_default(),
      test_dir: self
        .config
        .test_dir
        .as_ref()
        .map_or_else(std::path::PathBuf::new, std::path::PathBuf::from),
      snapshot_names: Arc::new(std::sync::Mutex::new(crate::snapshot_path::SnapshotNames::default())),
      aria_snapshot_names: Arc::new(std::sync::Mutex::new(crate::snapshot_path::SnapshotNames::default())),
      timeout: Duration::from_millis(self.config.timeout),
      tags: Vec::new(),
      start_time: Instant::now(),
      event_bus: self.event_bus.clone(),
      annotations: Arc::new(Mutex::new(Vec::new())),
      trace_composite: Arc::new(std::sync::Mutex::new(None)),
      trace_step_calls: Arc::new(std::sync::Mutex::new(rustc_hash::FxHashMap::default())),
      open_steps: Arc::new(tokio::sync::Mutex::new(Vec::new())),
      output: std::sync::Arc::new(std::sync::Mutex::new(crate::model::TestOutput::default())),
    })
  }

  #[tracing::instrument(skip_all, fields(worker_id = self.id))]
  pub async fn run(
    &self,
    browser_handle: Arc<crate::runner::BrowserHandle>,
    custom_fixture_pool: FixturePool,
    rx: async_channel::Receiver<WorkItem>,
    result_tx: mpsc::Sender<WorkerTestResult>,
    stop_flag: Arc<std::sync::atomic::AtomicBool>,
  ) {
    if let Some(event_bus) = &self.event_bus {
      event_bus.emit(ReporterEvent::WorkerStarted { worker_id: self.id });
    }

    // Register the worker-scope `browser` + `request` fixtures on the
    // custom pool so child suite/test pools resolve them via the parent
    // chain. The backing `BrowserHandle` makes the browser launch lazy;
    // the `HttpClient` is built once per worker so its reqwest pool,
    // TLS context, and cookie jar are reused across every test on this
    // worker.
    let mut worker_defs: FxHashMap<String, FixtureDef> = FxHashMap::default();
    worker_defs.insert("browser".into(), build_worker_browser_def(Arc::clone(&browser_handle)));
    worker_defs.insert("request".into(), build_worker_request_def(self.config.base_url.clone()));
    let custom_fixture_pool = custom_fixture_pool.child_with_defs(worker_defs, FixtureScope::Worker);

    let mut active_suites: FxHashMap<String, SuiteState> = FxHashMap::default();

    while let Ok(item) = rx.recv().await {
      // `--max-failures` / `-x` flips this flag; drop any items that were
      // already buffered in the channel rather than processing them.
      if stop_flag.load(std::sync::atomic::Ordering::SeqCst) {
        break;
      }
      match item {
        WorkItem::Single(assignment) => {
          let result = ferridriver_expect::with_expect_config(
            self.expect_config(),
            Box::pin(self.run_single(&browser_handle, &custom_fixture_pool, &mut active_suites, assignment)),
          )
          .await;
          if result_tx.send(result).await.is_err() {
            break;
          }
        },
        WorkItem::Serial(batch) => {
          let results =
            Box::pin(self.run_serial_batch(&browser_handle, &custom_fixture_pool, &mut active_suites, batch)).await;
          for result in results {
            if result_tx.send(result).await.is_err() {
              break;
            }
          }
        },
      }
      // Yield so the runner can observe the just-sent result and trip the
      // stop flag (for `--max-failures` / `-x`) before this worker races
      // to pull the next item out of the buffered channel.
      tokio::task::yield_now().await;
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
            column: None,
          };
          if let Some(event_bus) = &self.event_bus {
            event_bus.emit(ReporterEvent::StepStarted(Arc::new(
              crate::reporter::StepStartedEvent {
                test_id: synthetic_id.clone(),
                step_id: step_id.clone(),
                parent_step_id: None,
                title: step_title.clone(),
                category: StepCategory::Hook,
                location: None,
              },
            )));
          }
          let start = Instant::now();
          let result = run_caught(hook(state.fixture_pool.clone())).await;
          let duration = start.elapsed();
          let error = result.as_ref().err().map(|e| format!("{e}"));
          if let Some(event_bus) = &self.event_bus {
            event_bus.emit(ReporterEvent::StepFinished(Arc::new(
              crate::reporter::StepFinishedEvent {
                test_id: synthetic_id,
                step_id,
                title: step_title,
                category: StepCategory::Hook,
                duration,
                error: error.clone(),
                metadata: None,
                annotations: Vec::new(),
              },
            )));
          }
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

    // Close contexts pre-created for tests that never arrived, before
    // the browser shuts down under them.
    self.pool.drain().await;

    // Graceful browser close — only fires when the worker actually
    // launched a browser via `BrowserHandle::get`. Tests that never
    // touched a browser-dependent fixture skip the close handshake
    // because no browser was launched in the first place.
    browser_handle.close().await;

    if let Some(event_bus) = &self.event_bus {
      event_bus.emit(ReporterEvent::WorkerFinished { worker_id: self.id });
    }
  }

  /// Run a serial batch: all tests in order, skip rest on failure.
  async fn run_serial_batch(
    &self,
    browser: &Arc<crate::runner::BrowserHandle>,
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
        let outcome = Arc::new(TestOutcome {
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
          annotations: test.annotations.clone(),
          ..self.outcome_base(test, SystemTime::now())
        });
        if let Some(event_bus) = &self.event_bus {
          event_bus.emit(ReporterEvent::TestFinished {
            outcome: Arc::clone(&outcome),
          });
        }
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

      let result = ferridriver_expect::with_expect_config(
        self.expect_config(),
        Box::pin(self.run_single(browser, custom_pool, active_suites, assignment)),
      )
      .await;
      if result.outcome.status == TestStatus::Failed || result.outcome.status == TestStatus::TimedOut {
        serial_failed = true;
      }
      results.push(result);
    }

    results
  }

  /// Describe the live test for the debug hook.
  ///
  /// The context name is what makes a stop useful: a client that attaches
  /// to the bound browser without it lands on a fresh context and sees none
  /// of the state the test built. `None` when there is nothing to look at
  /// (no context yet, no browser launched) — stopping then would block the
  /// run for an empty page.
  async fn debug_test(
    browser: &Arc<crate::runner::BrowserHandle>,
    resources: &Arc<TestBrowserResources>,
    test_id: &crate::model::TestId,
    error: Option<String>,
  ) -> Option<crate::debug::DebugTest> {
    let context = resources.current_context().await?;
    Some(crate::debug::DebugTest {
      test: test_id.full_name(),
      location: test_id.line.map(|line| format!("{}:{line}", test_id.file)),
      error,
      browser: browser.peek()?,
      context: context.name().to_string(),
    })
  }

  /// Run a single test with full hook lifecycle.
  #[tracing::instrument(skip_all, fields(worker_id = self.id, test, attempt = assignment.attempt))]
  async fn run_single(
    &self,
    browser: &Arc<crate::runner::BrowserHandle>,
    custom_pool: &FixturePool,
    active_suites: &mut FxHashMap<String, SuiteState>,
    assignment: TestAssignment,
  ) -> WorkerTestResult {
    let test = &assignment.test;
    let test_id = test.id.clone();
    tracing::Span::current().record("test", test_id.full_name().as_str());
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
      // Suite-hook contexts are not traced: per-test traces belong to
      // tests, and beforeAll/afterAll containers have no outcome to
      // attach one to.
      let suite_resources = Arc::new(TestBrowserResources::new(
        Arc::clone(browser),
        build_suite_effective_context_config(&self.config),
        suite_test_info.output_dir.clone(),
        None,
        Arc::clone(&self.pool),
        false,
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

    // Worker-scope `auto: true` fixtures resolve once before beforeAll runs.
    for name in suite_state.fixture_pool.auto_fixture_names_for(FixtureScope::Worker) {
      if let Err(e) = suite_state.fixture_pool.resolve(&name).await {
        tracing::warn!(target: "ferridriver::worker", "auto fixture '{name}' (suite) failed: {e}");
      }
    }

    if !suite_state.before_all_ran && !hooks.before_all.is_empty() {
      for (i, hook) in hooks.before_all.iter().enumerate() {
        let step_title = if hooks.before_all.len() == 1 {
          "beforeAll".to_string()
        } else {
          format!("beforeAll [{i}]")
        };
        if let Some(event_bus) = &self.event_bus {
          event_bus.emit(ReporterEvent::StepStarted(Arc::new(
            crate::reporter::StepStartedEvent {
              test_id: test_id.clone(),
              step_id: format!("hook:beforeAll:{suite_key}:{i}"),
              parent_step_id: None,
              title: step_title.clone(),
              category: StepCategory::Hook,
              location: None,
            },
          )));
        }
        let start = Instant::now();
        let result = run_caught(hook(suite_state.fixture_pool.clone())).await;
        let duration = start.elapsed();
        let error = result.as_ref().err().map(|e| e.message.clone());
        if let Some(event_bus) = &self.event_bus {
          event_bus.emit(ReporterEvent::StepFinished(Arc::new(
            crate::reporter::StepFinishedEvent {
              test_id: test_id.clone(),
              step_id: format!("hook:beforeAll:{suite_key}:{i}"),
              title: step_title,
              category: StepCategory::Hook,
              duration,
              error: error.clone(),
              metadata: None,
              annotations: Vec::new(),
            },
          )));
        }
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
      let outcome = Arc::new(TestOutcome {
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
        annotations: test.annotations.clone(),
        ..self.outcome_base(test, SystemTime::now())
      });
      if let Some(event_bus) = &self.event_bus {
        event_bus.emit(ReporterEvent::TestFinished {
          outcome: Arc::clone(&outcome),
        });
      }
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
      let outcome = Arc::new(TestOutcome {
        test_id: test_id.clone(),
        status: TestStatus::Skipped,
        duration: Duration::ZERO,
        attempt,
        max_attempts,
        annotations: test.annotations.clone(),
        ..self.outcome_base(test, SystemTime::now())
      });
      if let Some(event_bus) = &self.event_bus {
        event_bus.emit(ReporterEvent::TestFinished {
          outcome: Arc::clone(&outcome),
        });
      }
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

    if let Some(event_bus) = &self.event_bus {
      event_bus.emit(ReporterEvent::TestStarted {
        test_id: test_id.clone(),
        attempt,
        worker_id: self.id,
      });
    }

    // Evaluate Fail condition: if condition matches, expect failure (invert pass/fail).
    let mut expected_status = test.expected_status;
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
    let started_at = SystemTime::now();
    let effective_config = build_effective_context_config(&self.config, test);
    let trace_composite: Arc<std::sync::Mutex<Option<String>>> = Arc::new(std::sync::Mutex::new(None));

    // Create TestInfo for this test execution.
    let test_info = Arc::new(TestInfo {
      test_id: test_id.clone(),
      title_path: test_id.title_path(),
      retry: attempt.saturating_sub(1),
      worker_index: self.id,
      parallel_index: self.slot,
      repeat_each_index: 0,
      output_dir: absolutize(self.config.output_dir.join(artifact_dir_name(&test_id.full_name()))),
      snapshot_dir: absolutize(
        self
          .config
          .snapshot_dir
          .as_ref()
          .map(std::path::PathBuf::from)
          .unwrap_or_else(|| std::path::PathBuf::from("__snapshots__")),
      ),
      snapshot_path_template: self.config.snapshot_path_template.clone(),
      update_snapshots: self.config.update_snapshots,
      ignore_snapshots: self.config.ignore_snapshots,
      attachments: Arc::new(Mutex::new(Vec::new())),
      steps: Arc::new(Mutex::new(Vec::new())),
      soft_errors: Arc::new(std::sync::Mutex::new(Vec::new())),
      errors: Arc::new(Mutex::new(Vec::new())),
      snapshot_suffix: Arc::new(Mutex::new(String::new())),
      column: None,
      project: None,
      config_snapshot: Some(Arc::clone(&self.config)),
      expect: Arc::new(self.config.resolved_expect(None)),
      config_dir: self.config.config_dir.clone().unwrap_or_default(),
      test_dir: self
        .config
        .test_dir
        .as_ref()
        .map_or_else(std::path::PathBuf::new, std::path::PathBuf::from),
      snapshot_names: Arc::new(std::sync::Mutex::new(crate::snapshot_path::SnapshotNames::default())),
      aria_snapshot_names: Arc::new(std::sync::Mutex::new(crate::snapshot_path::SnapshotNames::default())),
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
      event_bus: self.event_bus.clone(),
      annotations: Arc::new(Mutex::new(Vec::new())),
      trace_composite: Arc::clone(&trace_composite),
      trace_step_calls: Arc::new(std::sync::Mutex::new(rustc_hash::FxHashMap::default())),
      open_steps: Arc::new(tokio::sync::Mutex::new(Vec::new())),
      output: std::sync::Arc::new(std::sync::Mutex::new(crate::model::TestOutput::default())),
    });
    let trace_spec = self.config.trace.should_record(attempt, false).then(|| TraceSpec {
      title: test_id.full_name(),
      // The stream is named after the test, in this worker's artifacts
      // directory: that is the whole contract a viewer following a
      // running test relies on — it asks for `<testId>.json` there.
      name: test_id.stable_id(self.config.name.as_deref().unwrap_or_default()),
      traces_dir: crate::artifacts::traces_dir(&self.config.output_dir, self.id as usize),
      live: self.live_traces,
      composite: Arc::clone(&trace_composite),
    });
    let resources = Arc::new(TestBrowserResources::new(
      Arc::clone(browser),
      effective_config,
      test_info.output_dir.clone(),
      trace_spec,
      Arc::clone(&self.pool),
      fixture_requests.iter().any(|f| f == "page"),
    ));
    let test_pool = custom_pool.child_with_defs(build_test_fixture_defs(Arc::clone(&resources)), FixtureScope::Test);
    test_pool.inject("test_info", Arc::clone(&test_info));

    // Playwright `auto: true` fixtures resolve regardless of whether
    // the test body destructured them. Walk the full def graph for
    // this scope (and any narrower parents) and pre-resolve.
    for name in test_pool.auto_fixture_names_for(FixtureScope::Test) {
      if let Err(e) = test_pool.resolve(&name).await {
        tracing::warn!(target: "ferridriver::worker", "auto fixture '{name}' failed: {e}");
      }
    }

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
            let () = resources.close().await;
            let duration = start.elapsed();
            let failure = TestFailure::wrap("failed to create page", e);
            let outcome = Arc::new(TestOutcome {
              test_id: test_id.clone(),
              status: TestStatus::Failed,
              duration,
              attempt,
              max_attempts,
              errors: vec![failure.clone()],
              error: Some(failure),
              annotations: test.annotations.clone(),
              ..self.outcome_base(test, started_at)
            });
            if let Some(event_bus) = &self.event_bus {
              event_bus.emit(ReporterEvent::TestFinished {
                outcome: Arc::clone(&outcome),
              });
            }
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
      let result = ferridriver_expect::with_sink(
        Arc::clone(&test_info) as Arc<dyn ferridriver_expect::SoftSink>,
        run_caught(hook(test_pool.clone(), Arc::clone(&test_info))),
      )
      .await;
      let err_msg = result.as_ref().err().map(|e| e.message.clone());
      step_handle.end(err_msg).await;
      if let Err(e) = result {
        before_each_err = Some(e);
        break;
      }
    }

    let debug_hook = crate::debug::debug_hook();
    // `--debug`: the body has not started and the context is live, which is
    // the point Playwright publishes a test from
    // (`runAfterCreateBrowserContext`). The hook arms rather than blocks —
    // the first API call is where the run actually stops.
    //
    // The context is normally created by the `page` fixture, i.e. inside
    // the body — too late for a client to attach before the first call.
    // Debugging is the one mode that pays for creating it up front.
    if let Some(hook) = &debug_hook {
      if let Err(e) = resources.context().await {
        tracing::warn!(target: "ferridriver::worker", "--debug: no context to publish: {e}");
      }
      if let Some(live) = Self::debug_test(browser, &resources, &test_id, None).await {
        hook.test_starting(live).await;
      }
    }

    let timeout_result = if let Some(err) = before_each_err {
      Ok(Err(err))
    } else {
      // Soft assertions raised anywhere in the body land on this test's
      // own collector and fail it at the end, instead of stopping here.
      ferridriver::pause::run_within(
        timeout_dur,
        ferridriver_expect::with_sink(
          Arc::clone(&test_info) as Arc<dyn ferridriver_expect::SoftSink>,
          run_caught((test.test_fn)(test_pool.clone())),
        ),
      )
      .await
    };

    // Hold here, before `afterEach` and before the context closes, so
    // whoever attaches sees the page the failure left rather than its
    // wreckage. Only on a failure: a passing test has nothing to look at.
    if let Some(hook) = &debug_hook {
      let failure = match &timeout_result {
        Ok(Err(e)) => Some(e.message.clone()),
        Err(_) => Some(format!("test timed out after {}ms", timeout_dur.as_millis())),
        Ok(Ok(())) => None,
      };
      if let Some(error) = failure
        && let Some(live) = Self::debug_test(browser, &resources, &test_id, Some(error)).await
      {
        hook.test_failed(live).await;
      }
    }

    for (i, hook) in hooks.after_each.iter().enumerate() {
      let title = if hooks.after_each.len() == 1 {
        "afterEach".to_string()
      } else {
        format!("afterEach [{i}]")
      };
      let step_handle = test_info.begin_step(&title, StepCategory::Hook).await;
      let result = ferridriver_expect::with_sink(
        Arc::clone(&test_info) as Arc<dyn ferridriver_expect::SoftSink>,
        run_caught(hook(test_pool.clone(), Arc::clone(&test_info))),
      )
      .await;
      let err_msg = result.as_ref().err().map(|e| e.message.clone());
      step_handle.end(err_msg).await;
      if let Err(e) = result {
        tracing::warn!(target: "ferridriver::worker", "afterEach error: {e}");
      }
    }

    // Release the debugger's hold before the artifacts are collected: the
    // session it published points at this test's context, and the next
    // test must not inherit either it or the gate it armed.
    if let Some(hook) = &debug_hook {
      hook.test_finished().await;
    }

    if page_for_artifacts.is_none() {
      page_for_artifacts = test_pool.try_get_cached::<ferridriver::Page>("page");
    }
    let test_failed = timeout_result.as_ref().is_err() || timeout_result.as_ref().is_ok_and(|r| r.is_err());
    let screenshot = if test_failed && self.config.screenshot_on_failure {
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
    // The failure itself goes into the trace, not just the call that
    // raised it: the viewer's Errors tab is built from these, and an
    // assertion message is what a reader is looking for first.
    if test_failed {
      let message = match &timeout_result {
        Ok(Err(e)) => Some(e.message.clone()),
        Err(_) => Some(format!("Test timeout of {}ms exceeded.", timeout_dur.as_millis())),
        Ok(Ok(())) => None,
      };
      let composite = trace_composite
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
      if let (Some(composite), Some(message)) = (composite, message) {
        let stack = test_id
          .line
          .map(|line| ferridriver::trace::StackFrame {
            file: test_id.file.clone(),
            line: u32::try_from(line).unwrap_or_default(),
            column: 0,
          })
          .into_iter()
          .collect();
        ferridriver::trace::record_error(&composite, message, stack);
      }
    }

    // Mirror the failure screenshot into the trace as an `attach`
    // action (Playwright's test runner does the same), so the viewer's
    // Attachments tab carries it alongside the timeline.
    if let Some(ref png) = screenshot {
      let composite = trace_composite
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
      if let Some(composite) = composite
        && let Some(mut span) = ferridriver::trace::begin_custom_action(
          &composite,
          ferridriver::trace::CustomAction {
            class: "Test",
            method: "attach",
            title: "attach \"screenshot-on-failure\"".to_string(),
            params: serde_json::json!({}),
            parent_id: None,
            step_id: None,
            backdate_ms: 0.0,
            stack: Vec::new(),
          },
        )
      {
        span.attach("screenshot-on-failure", "image/png", png.clone());
        span.finish_message(None);
      }
    }
    // Stop the per-test trace while the context is still alive: export
    // to disk when the mode retains it, discard otherwise. Retention
    // keys off the RAW body result (same signal as screenshot-on-failure
    // and buffered video): an expected-failure test that fails as
    // expected still retains its trace under retain-on-failure — the
    // trace shows the deliberate failure.
    let trace_path = {
      let started = trace_composite
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
        .is_some();
      // Stop the UI live-trace poller from exporting a recorder that is
      // about to be torn down (the finished zip takes over at finish).
      if started {
        crate::ui_server::unregister_live_trace(&test_id.full_name());
      }
      match (started, resources.current_context().await) {
        (true, Some(ctx)) => {
          let path = self.config.trace.should_write(attempt, test_failed).then(|| {
            let _ = std::fs::create_dir_all(&test_info.output_dir);
            test_info.output_dir.join(format!(
              "{}-attempt{}.trace.zip",
              sanitize_filename(&test_id.name),
              attempt
            ))
          });
          match ctx
            .tracing()
            .stop(ferridriver::trace::TracingStopOptions { path: path.clone() })
            .await
          {
            Ok(()) => path,
            Err(e) => {
              tracing::warn!(target: "ferridriver::worker", "trace stop failed: {e}");
              None
            },
          }
        },
        _ => None,
      }
    };
    resources.close().await;

    let duration = start.elapsed();
    let result = (timeout_result, screenshot, video_path, Some(test_pool));
    let (timeout_result, screenshot, video_path, test_pool) = result;

    let mut attachments = Vec::new();
    if let Some(ref png) = screenshot {
      let _ = std::fs::create_dir_all(&test_info.output_dir);
      let screenshot_path = test_info.output_dir.join(format!("test-failed-attempt{attempt}.png"));
      match std::fs::write(&screenshot_path, png) {
        Ok(()) => attachments.push(Attachment {
          name: "screenshot-on-failure".into(),
          content_type: "image/png".into(),
          body: AttachmentBody::Path(screenshot_path),
          step_id: None,
        }),
        Err(e) => {
          tracing::warn!(target: "ferridriver::worker", "screenshot write failed: {e}");
          attachments.push(Attachment {
            name: "screenshot-on-failure".into(),
            content_type: "image/png".into(),
            body: AttachmentBody::Bytes(png.clone()),
            step_id: None,
          });
        },
      }
    }
    if let Some(path) = trace_path {
      attachments.push(Attachment {
        name: "trace".into(),
        content_type: "application/zip".into(),
        body: AttachmentBody::Path(path),
        step_id: None,
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
          let outcome = Arc::new(TestOutcome {
            test_id: test_id.clone(),
            status: TestStatus::Skipped,
            duration: start.elapsed(),
            attempt,
            max_attempts,
            annotations: test.annotations.clone(),
            ..self.outcome_base(test, started_at)
          });
          if let Some(event_bus) = &self.event_bus {
            event_bus.emit(ReporterEvent::TestFinished {
              outcome: Arc::clone(&outcome),
            });
          }
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
    if let Some(ref pool) = test_pool
      && let Ok(modifiers) = pool.get::<crate::TestModifiers>("__test_modifiers").await
    {
      if modifiers.expected_failure.load(std::sync::atomic::Ordering::Relaxed) {
        expected_status = ExpectedStatus::Fail;
      }
      // Runtime slow: annotate via test_info for reporters.
      if modifiers.slow.load(std::sync::atomic::Ordering::Relaxed) {
        test_info.annotate("slow", "test.slow() called at runtime").await;
      }
      // timeout_override: already elapsed for this attempt, but log for debugging.
      if let Ok(guard) = modifiers.timeout_override.lock()
        && let Some(ms) = *guard
      {
        tracing::debug!(target: "ferridriver::worker", "test.setTimeout({ms}ms) called at runtime");
      }
    }

    // Soft assertions are part of WHAT HAPPENED, so they settle before
    // `test.fail()` is consulted: a test that only failed softly did
    // fail, and `test.fail()` must be able to expect that.
    let soft_errors = test_info.drain_soft_errors();
    let (raw_status, raw_error) = if !soft_errors.is_empty() && raw_status == TestStatus::Passed {
      let msg = soft_errors
        .iter()
        .map(|e| format!("  - {}", e.message))
        .collect::<Vec<_>>()
        .join("\n");
      (
        TestStatus::Failed,
        Some(TestFailure {
          message: format!("{} soft assertion(s) failed:\n{msg}", soft_errors.len()),
          stack: None,
          diff: None,
          screenshot: None,
        }),
      )
    } else {
      (raw_status, raw_error)
    };

    // `test.fail()` does not rewrite what happened: the attempt keeps the
    // status it ended with, and `expected_status` says which one counts as
    // success. Every consumer compares the two through
    // `model::outcome_kind` — inverting here instead would report a
    // `test.fail` test as `passed` in the JSON report, where Playwright
    // reports `failed` with `expectedStatus: "failed"`.
    let (status, error) = match (&raw_status, &expected_status) {
      (TestStatus::Passed, ExpectedStatus::Fail) => (
        TestStatus::Passed,
        Some(TestFailure {
          message: "Expected to fail, but passed.".into(),
          stack: None,
          diff: None,
          screenshot: None,
        }),
      ),
      _ => (raw_status, raw_error),
    };

    // Collect tracked test steps and attachments.
    let steps = test_info.steps.lock().await.clone();
    let info_attachments = test_info.attachments.lock().await.clone();
    attachments.extend(info_attachments);

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
          step_id: None,
        });
      } else {
        let _ = std::fs::remove_file(path);
      }
    }

    // Merge compile-time annotations with runtime annotations.
    let mut annotations = test.annotations.clone();
    annotations.extend(test_info.get_annotations().await);

    // What the test printed belongs to the test, not to the process:
    // reporters, the HTML report and the UI's terminal read it here.
    let output = test_info
      .output
      .lock()
      .map(|mut held| std::mem::take(&mut *held))
      .unwrap_or_default();

    // `errors` leads with the hard failure and then the soft assertions
    // the test collected: reporters that show every error read it, and
    // `error` stays the first for the ones that show one.
    let mut errors: Vec<TestFailure> = error.iter().cloned().collect();
    errors.extend(soft_errors);
    let outcome = Arc::new(TestOutcome {
      test_id: test_id.clone(),
      status,
      duration,
      attempt,
      max_attempts,
      error,
      errors,
      attachments,
      steps,
      stdout: output.stdout,
      stderr: output.stderr,
      annotations,
      // The effective expectation, not the declared one: `test.fail()`
      // called inside the body arms after the base was built.
      expected_status,
      ..self.outcome_base(test, started_at)
    });

    if let Some(event_bus) = &self.event_bus {
      event_bus.emit(ReporterEvent::TestFinished {
        outcome: Arc::clone(&outcome),
      });
    }

    let should_retry = crate::model::outcome_kind(&[outcome.status], outcome.expected_status)
      == crate::model::TestOutcomeKind::Unexpected
      && attempt < max_attempts;

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

std::thread_local! {
  /// Location + backtrace captured by the panic hook, read by
  /// `run_caught` right after `catch_unwind` returns on the same thread
  /// (unwinding resolves within the poll that panicked).
  static LAST_PANIC_STACK: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Chain a capture step onto the process panic hook (once) so test
/// failures carry the panic's location and backtrace, not just its
/// message. The previous hook still runs, keeping default stderr output.
fn install_panic_capture() {
  static HOOK: std::sync::Once = std::sync::Once::new();
  HOOK.call_once(|| {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
      let location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
        .unwrap_or_else(|| "<unknown>".to_string());
      let backtrace = std::backtrace::Backtrace::force_capture();
      LAST_PANIC_STACK.with(|cell| *cell.borrow_mut() = Some(format!("panicked at {location}\n{backtrace}")));
      previous(info);
    }));
  });
}

/// Await a test or hook future, converting a panic (`assert!`, `unwrap`,
/// ...) into a `TestFailure` so std assertion macros fail the single test
/// instead of unwinding through the worker.
async fn run_caught<F>(fut: F) -> Result<(), TestFailure>
where
  F: std::future::Future<Output = Result<(), TestFailure>>,
{
  use futures::FutureExt;
  install_panic_capture();
  match std::panic::AssertUnwindSafe(fut).catch_unwind().await {
    Ok(result) => result,
    Err(payload) => {
      let message = payload
        .downcast_ref::<&'static str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("panic with non-string payload");
      let stack = LAST_PANIC_STACK.with(|cell| cell.borrow_mut().take());
      Err(TestFailure {
        message: format!("panicked: {message}"),
        stack,
        diff: None,
        screenshot: None,
      })
    },
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
    ..Default::default()
  };
  page.screenshot().options(opts).await.ok()
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
    "mobile" => browser.use_options.is_mobile,
    "touch" => browser.use_options.has_touch,
    "dark" => browser.use_options.color_scheme.as_deref() == Some("dark"),
    "light" => browser.use_options.color_scheme.as_deref() == Some("light"),
    "offline" => browser.use_options.offline,
    "bypass-csp" => browser.use_options.bypass_csp,

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
