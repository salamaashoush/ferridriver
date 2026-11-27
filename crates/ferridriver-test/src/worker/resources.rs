//! Per-test/per-suite browser, context, page, trace, and fixture resource management.

use std::sync::Arc;
use std::time::Duration;

use rustc_hash::FxHashMap;
use tokio::sync::Mutex;

use super::context_config::{EffectiveContextConfig, build_context_options};
use crate::fixture::{FixtureDef, FixtureScope};

enum TestBrowserState {
  Empty,
  Context(Arc<ferridriver::ContextRef>),
  Page {
    ctx: Arc<ferridriver::ContextRef>,
    page: Arc<ferridriver::Page>,
  },
  Failed(ferridriver::FerriError),
}

pub(super) struct TestBrowserResources {
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
pub(super) struct TraceSpec {
  pub(super) key: String,
  pub(super) title: String,
  /// Trace stream name — the test's stable id, so a viewer can find the
  /// recording on disk while it is still being written.
  pub(super) name: String,
  /// This worker's in-progress trace directory.
  pub(super) traces_dir: std::path::PathBuf,
  /// Flush each event as it happens, for a UI following the run.
  pub(super) live: bool,
  /// What `use: { trace: { … } }` asked to record. Only the flags it
  /// spelled are here; the rest keep the runner's defaults.
  pub(super) capture: crate::config::TraceOption,
  pub(super) composite: Arc<std::sync::Mutex<Option<String>>>,
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
/// created page. Only `BiDi` needs the probe; CDP and Playwright `WebKit`
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
  pub(super) fn new(
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
  pub(super) async fn acquire_pooled(
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

  /// The `browser` a test or hook is handed: the worker's browser, whose
  /// `newContext` starts from this test's `use` options, as Playwright's
  /// does.
  pub(super) async fn browser(&self) -> ferridriver::error::Result<Arc<ferridriver::Browser>> {
    let browser = self.handle.get().await?;
    let opts = build_context_options(&self.effective, &self.output_dir, browser.backend_kind());
    Ok(Arc::new(browser.inheriting_context_options(opts)))
  }

  /// Everything a context needs before a test touches it.
  ///
  /// One seam for every path that produces a context — pooled, fresh, or
  /// reused — so a per-context setting cannot be applied on some of them
  /// and not the others.
  pub(super) async fn adopt_context(&self, ctx: &ferridriver::ContextRef) {
    // Playwright applies `use: { actionTimeout, navigationTimeout }` to
    // the context after creating it; neither is a `newContext` option
    // there or here.
    if let Some(ms) = self.effective.context.action_timeout {
      ctx.set_default_timeout(ms);
    }
    if let Some(ms) = self.effective.context.navigation_timeout {
      ctx.set_default_navigation_timeout(ms);
    }
    self.start_tracing(ctx).await;
  }

  /// Start the per-test trace on a freshly created context.
  pub(super) async fn start_tracing(&self, ctx: &ferridriver::ContextRef) {
    let Some(spec) = &self.trace else { return };
    let options = ferridriver::trace::TracingStartOptions {
      name: Some(spec.name.clone()),
      title: Some(spec.title.clone()),
      screenshots: spec.capture.screenshots.unwrap_or(true),
      snapshots: spec.capture.snapshots.unwrap_or(true),
      // Steps carry their .feature / call-site stack frames; embedding
      // the referenced files lights up the viewer's Source tab.
      sources: spec.capture.sources.unwrap_or(true),
      attachments: spec.capture.attachments.unwrap_or(true),
      streaming: ferridriver::trace::TraceStreaming::from_live(spec.live),
    };
    ctx.set_traces_dir(spec.traces_dir.clone()).await;
    match ctx.tracing().start().options(options).await {
      Ok(()) => {
        let composite = ctx.composite();
        *spec.composite.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(composite.clone());
        // Publish for the UI server's live-trace snapshot endpoint (the
        // `bdd --ui` viewer polls it while the test runs). Keyed by the
        // test's execution key — the same id the client sees on testStarted.
        crate::ui_server::register_live_trace(&spec.key, &composite);
      },
      Err(e) => tracing::warn!(target: "ferridriver::worker", "trace start failed: {e}"),
    }
  }

  /// Discard a live trace on a context that is being torn down without
  /// the worker's explicit stop (page-creation failure, `BiDi` retry).
  pub(super) async fn discard_tracing(&self, ctx: &ferridriver::ContextRef) {
    let Some(spec) = &self.trace else { return };
    let started = spec
      .composite
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .take()
      .is_some();
    if started {
      crate::ui_server::unregister_live_trace(&spec.key);
      let _ = ctx.tracing().stop().await;
    }
  }

  /// The test's live context, if one has been created. Never creates.
  pub(super) async fn current_context(&self) -> Option<Arc<ferridriver::ContextRef>> {
    let state = self.state.lock().await;
    match &*state {
      TestBrowserState::Context(ctx) | TestBrowserState::Page { ctx, .. } => Some(Arc::clone(ctx)),
      TestBrowserState::Empty | TestBrowserState::Failed(_) => None,
    }
  }

  pub(super) async fn context(&self) -> ferridriver::error::Result<Arc<ferridriver::ContextRef>> {
    let mut state = self.state.lock().await;
    match &mut *state {
      TestBrowserState::Context(ctx) | TestBrowserState::Page { ctx, .. } => Ok(Arc::clone(ctx)),
      TestBrowserState::Failed(err) => Err(err.clone()),
      TestBrowserState::Empty => {
        let browser = self.handle.get().await?;
        if self.wants_page
          && let Some(pooled) = Box::pin(self.acquire_pooled(&browser)).await
        {
          let (ctx, page) = pooled?;
          self.adopt_context(&ctx).await;
          *state = TestBrowserState::Page {
            ctx: Arc::clone(&ctx),
            page,
          };
          return Ok(ctx);
        }
        let opts = build_context_options(&self.effective, &self.output_dir, browser.backend_kind());
        let (ctx, page) = new_test_context(&browser, opts).await?;
        let ctx = Arc::new(ctx);
        self.adopt_context(&ctx).await;
        *state = match page {
          Some(page) => TestBrowserState::Page {
            ctx: Arc::clone(&ctx),
            page,
          },
          None => TestBrowserState::Context(Arc::clone(&ctx)),
        };
        Ok(ctx)
      },
    }
  }

  #[tracing::instrument(skip_all, name = "page_fixture")]
  pub(super) async fn page(&self) -> ferridriver::error::Result<Arc<ferridriver::Page>> {
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
          self.adopt_context(&ctx).await;
          *state = TestBrowserState::Page {
            ctx,
            page: Arc::clone(&page),
          };
          return Ok(page);
        }
        let opts = build_context_options(&self.effective, &self.output_dir, backend);
        let (ctx, page) = new_test_context(&browser, opts.clone()).await?;
        let ctx = Arc::new(ctx);
        self.adopt_context(&ctx).await;
        let page = match page {
          Some(page) => Ok(page),
          None => create_ready_page(&ctx, backend).await,
        };
        match page {
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
              let (ctx, page) = new_test_context(&browser, opts).await?;
              let ctx = Arc::new(ctx);
              self.adopt_context(&ctx).await;
              let page = match page {
                Some(page) => page,
                None => create_ready_page(&ctx, backend).await?,
              };
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

  pub(super) async fn close(&self) {
    let mut state = self.state.lock().await;
    match std::mem::replace(&mut *state, TestBrowserState::Empty) {
      TestBrowserState::Context(ctx) => {
        self.discard_tracing(&ctx).await;
        close_test_context(&ctx).await;
      },
      TestBrowserState::Page { ctx, page } => {
        self.discard_tracing(&ctx).await;
        // A backend that shares the persistent default context keeps
        // both: closing the context would tear down the only one there
        // is, and its page is the tab the browser must never be without.
        // The next test's reset replaces them. For isolated-context
        // backends (CDP, BiDi, Playwright WebKit) the context's
        // `disposeBrowserContext` already closes every page in it, so an
        // explicit `page.close()` would only add a redundant
        // `closeTarget` round-trip per test (~3-5ms each on the bench's
        // tight loop).
        drop(page);
        close_test_context(&ctx).await;
      },
      TestBrowserState::Empty | TestBrowserState::Failed(_) => {},
    }
  }
}

/// Open a per-test browsing container. Backends that support
/// isolated contexts get a fresh `Browser::new_context(None)`. All
/// desktop backends create isolated contexts. Android Chrome has only
/// its device context, which every test on the worker therefore shares:
/// it is reset to a new context's state and comes back with the one
/// blank page it must always keep. It retains the device viewport unless
/// one is explicitly set.
async fn new_test_context(
  browser: &Arc<ferridriver::Browser>,
  opts: ferridriver::options::BrowserContextOptions,
) -> ferridriver::error::Result<(ferridriver::ContextRef, Option<Arc<ferridriver::Page>>)> {
  if browser.supports_isolated_contexts() {
    Ok((browser.new_context().options(opts).await?, None))
  } else {
    let mut opts = opts;
    if opts.viewport == ferridriver::options::ViewportOption::Default {
      opts.viewport = ferridriver::options::ViewportOption::Null;
    }
    let (context, page) = Box::pin(browser.reset_default_context(opts)).await?;
    Ok((context, Some(page)))
  }
}

/// Drop a per-test context. Skips `ctx.close()` when the context is
/// the shared default container — closing it would tear down the
/// only browsing context available on a backend that shares the
/// persistent default, including Android Chrome.
async fn close_test_context(ctx: &ferridriver::ContextRef) {
  if ctx.name() == "default" {
    return;
  }
  let _ = ctx.close().await;
}

/// The worker-scope fixture the test-scoped `browser` depends on: it
/// launches the worker's browser at worker scope, outside any test's
/// timeout, as Playwright's worker `browser` launches.
pub(super) const WORKER_BROWSER_LAUNCH: &str = "_browserLaunch";

/// Worker-scope `browser` fixture backed by `BrowserHandle`, registered
/// under `browser` and under [`WORKER_BROWSER_LAUNCH`]. Added to the
/// `custom_fixture_pool` so every child suite/test pool can resolve it via
/// the parent chain. Lazy: launches on first `get`.
pub(super) fn build_worker_browser_def(handle: Arc<crate::runner::BrowserHandle>) -> FixtureDef {
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
    timeout: Duration::ZERO,
    auto: false,
  }
}

fn build_browser_fixture_defs(
  resources: &Arc<TestBrowserResources>,
  scope: FixtureScope,
) -> FxHashMap<String, FixtureDef> {
  // A test-scoped context or page is set up under the test's own budget,
  // as Playwright's are: a cold browser start the test timeout allows must
  // not fail on a shorter cap of the fixture's own. Suite hooks run under
  // no budget, so their fixtures keep one.
  let timeout = if scope == FixtureScope::Test {
    Duration::ZERO
  } else {
    Duration::from_secs(10)
  };
  let mut defs = FxHashMap::default();

  // Shadows the worker's own `browser` for the tests and hooks this pool
  // runs, over the browser the worker-scope launch already started.
  defs.insert(
    "browser".into(),
    FixtureDef {
      name: "browser".into(),
      scope,
      dependencies: vec![WORKER_BROWSER_LAUNCH.into()],
      setup: Arc::new({
        let resources = Arc::clone(resources);
        move |_pool| {
          let resources = Arc::clone(&resources);
          Box::pin(async move {
            let browser = Box::pin(resources.browser()).await?;
            Ok(browser as Arc<dyn std::any::Any + Send + Sync>)
          })
        }
      }),
      teardown: None,
      timeout,
      auto: false,
    },
  );

  defs.insert(
    "context".into(),
    FixtureDef {
      name: "context".into(),
      scope,
      dependencies: vec!["browser".into()],
      setup: Arc::new({
        let resources = Arc::clone(resources);
        move |_pool| {
          let resources = Arc::clone(&resources);
          Box::pin(async move {
            let ctx = Box::pin(resources.context()).await?;
            Ok(ctx as Arc<dyn std::any::Any + Send + Sync>)
          })
        }
      }),
      teardown: None,
      timeout,
      auto: false,
    },
  );

  defs.insert(
    "page".into(),
    FixtureDef {
      name: "page".into(),
      scope,
      dependencies: vec!["browser".into()],
      setup: Arc::new({
        let resources = Arc::clone(resources);
        move |_pool| {
          let resources = Arc::clone(&resources);
          Box::pin(async move {
            let page = Box::pin(resources.page()).await?;
            Ok(page as Arc<dyn std::any::Any + Send + Sync>)
          })
        }
      }),
      teardown: None,
      timeout,
      auto: false,
    },
  );

  defs
}

pub(super) fn build_request_def(
  base_url: Option<String>,
  http_credentials: Vec<ferridriver::options::HttpCredentials>,
  proxy: Option<ferridriver::options::ProxyConfig>,
) -> FixtureDef {
  FixtureDef {
    name: "request".into(),
    scope: FixtureScope::Test,
    dependencies: vec![],
    setup: Arc::new(move |_pool| {
      let base_url = base_url.clone();
      let http_credentials = http_credentials.clone();
      let proxy = proxy.clone();
      Box::pin(async move {
        Ok(Arc::new(ferridriver::http_client::HttpClient::new(
          ferridriver::http_client::HttpClientOptions {
            base_url,
            http_credentials,
            proxy,
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

pub(super) fn build_test_fixture_defs(resources: &Arc<TestBrowserResources>) -> FxHashMap<String, FixtureDef> {
  build_browser_fixture_defs(resources, FixtureScope::Test)
}

pub(super) fn build_suite_fixture_defs(resources: &Arc<TestBrowserResources>) -> FxHashMap<String, FixtureDef> {
  build_browser_fixture_defs(resources, FixtureScope::Worker)
}
