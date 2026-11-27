//! `BrowserContext` class -- NAPI binding for `ferridriver::ContextRef`.

mod events;
mod options;

use events::build_context_event_callback;
pub(crate) use events::{BindingSourceJs, ContextEventArg};
pub(crate) use options::{
  NapiBrowserContextOptions, NapiHttpCredentialsInput, NapiProxyConfig, NapiStorageState, NapiStorageStateOptions,
  NapiViewportSize, RecordVideoOptionsJs, cookie_urls, http_credentials_list, resolve_storage_state_input,
};

use crate::error::IntoNapi;
use crate::page::{JsUrl, PredReturn};
use crate::types::CookieData;
use napi::Result;
use napi_derive::napi;

/// Isolated browser context with its own cookies, storage, and permissions.
/// Mirrors Playwright's `BrowserContext`.
#[napi]
pub struct BrowserContext {
  inner: ferridriver::ContextRef,
  /// `context.on` / `context.once` registrations, so `off(event,
  /// listener)` resolves the core `ListenerId` from JS function
  /// identity — the same registry `Page` keeps.
  listener_regs: crate::emitter::ListenerRegistry<ContextEventArg>,
  /// `context.route` predicate and handler-identity registrations.
  routes: crate::route_registry::RouteRegistry,
}

impl BrowserContext {
  pub(crate) fn wrap(inner: ferridriver::ContextRef) -> Self {
    Self {
      inner,
      listener_regs: crate::emitter::ListenerRegistry::new(),
      routes: crate::route_registry::RouteRegistry::new(),
    }
  }

  /// Shared body of the registration methods: build the threadsafe
  /// dispatch, keep a `FunctionRef` for identity removal, and register
  /// on the core emitter.
  fn register_listener(
    &self,
    event: &str,
    listener: &napi::bindgen_prelude::Function<'_, ContextEventArg, ()>,
    once: bool,
    front: bool,
  ) -> Result<()> {
    let fn_ref = listener.create_ref()?;
    let callback = build_context_event_callback(listener)?;
    let id = match (once, front) {
      (false, false) => self.inner.on(event, callback),
      (true, false) => self.inner.once(event, callback),
      (false, true) => self.inner.prepend_listener(event, callback),
      (true, true) => self.inner.prepend_once_listener(event, callback),
    };
    self.listener_regs.record(event, id.0, fn_ref);
    Ok(())
  }
}

crate::macros::napi_event_emitter!(
  target: BrowserContext,
  arg: ContextEventArg,
  ts_on: "event: 'page' | 'weberror' | 'download' | 'frameattached' | 'framedetached' | 'framenavigated' | 'pageclose' | 'pageload' | 'dialog' | 'dialogclosed' | 'console' | 'request' | 'response' | 'requestfinished' | 'requestfailed' | 'close', listener: (arg: WebError | Download | Frame | Page | Dialog | ConsoleMessage | Request | Response | BrowserContext) => void",
  ts_off: "event: 'page' | 'weberror' | 'download' | 'frameattached' | 'framedetached' | 'framenavigated' | 'pageclose' | 'pageload' | 'dialog' | 'dialogclosed' | 'console' | 'request' | 'response' | 'requestfinished' | 'requestfailed' | 'close', listener?: (arg: WebError | Download | Frame | Page | Dialog | ConsoleMessage | Request | Response | BrowserContext) => void",
  full
);

#[napi]
#[expect(
  clippy::needless_pass_by_value,
  reason = "napi-derive takes JS strings, functions and option bags only by value"
)]
impl BrowserContext {
  /// Context name.
  #[napi(getter)]
  pub fn name(&self) -> String {
    self.inner.name().to_string()
  }

  /// Playwright: `browserContext.tracing` — the Tracing controller.
  #[napi(getter)]
  pub fn tracing(&self) -> crate::tracing::Tracing {
    crate::tracing::Tracing::wrap(self.inner.clone())
  }

  /// Playwright: `browserContext.clock` — the fake-time controller.
  #[napi(getter)]
  pub fn clock(&self) -> crate::clock::Clock {
    crate::clock::Clock::wrap(self.inner.clone())
  }

  /// Playwright: `browserContext.credentials` — the virtual WebAuthn
  /// authenticator.
  #[napi(getter)]
  pub fn credentials(&self) -> crate::credentials::Credentials {
    crate::credentials::Credentials::wrap(self.inner.credentials())
  }

  /// Playwright: `browserContext.request` — the context-bound API
  /// request client sharing this context's cookies both ways
  /// (`page.request` returns the same context-bound client).
  #[napi(getter)]
  pub fn request(&self) -> crate::http_client::HttpClient {
    crate::http_client::HttpClient::wrap(self.inner.http_client())
  }

  /// Playwright: `browserContext.newCDPSession(page)`. Attaches a raw
  /// CDP session to the page's target. Chromium-only. Playwright also
  /// accepts an OOPIF `Frame`; ferridriver currently supports the
  /// `Page` form.
  #[napi(
    js_name = "newCDPSession",
    ts_args_type = "page: Page",
    ts_return_type = "Promise<CDPSession>"
  )]
  pub fn new_cdp_session(
    &self,
    env: napi::Env,
    page: napi::bindgen_prelude::Reference<crate::page::Page>,
  ) -> Result<napi::bindgen_prelude::AsyncBlock<crate::cdp_session::CDPSession>> {
    let core_page = page.inner_arc();
    let inner = self.inner.clone();
    napi::bindgen_prelude::AsyncBlockBuilder::new(async move {
      let session = inner.new_cdp_session(&core_page).await.into_napi()?;
      Ok(crate::cdp_session::CDPSession::wrap(session))
    })
    .build(&env)
  }

  /// Create a new page in this context.
  #[napi(ts_return_type = "Promise<Page>")]
  pub async fn new_page(&self) -> Result<crate::identity::PageObject> {
    let page = Box::pin(self.inner.new_page()).await.into_napi()?;
    Ok(crate::identity::PageObject(page))
  }

  /// Playwright: `browserContext.pages(): Page[]`. The pages open in
  /// this context, in the order they were opened.
  #[napi(ts_return_type = "Page[]")]
  pub fn pages(&self) -> Vec<crate::identity::PageObject> {
    // A page this handle has not wrapped before starts its frame
    // listener, which needs the runtime a synchronous call is outside of.
    let pages = napi::bindgen_prelude::within_runtime_if_available(|| self.inner.pages());
    pages.into_iter().map(crate::identity::PageObject).collect()
  }

  /// [`Self::pages`] after asking the browser which windows it has. A
  /// classic `WebDriver` session is not told of a window opened behind
  /// its back, or of one that closed; every other backend is, and this
  /// only waits for it. Not in Playwright, which drives no such browser.
  #[napi(ts_return_type = "Promise<Page[]>")]
  pub async fn discover_pages(&self) -> Result<Vec<crate::identity::PageObject>> {
    let pages = self.inner.discover_pages().await.into_napi()?;
    Ok(pages.into_iter().map(crate::identity::PageObject).collect())
  }

  // ── Cookies ──

  /// Playwright: `context.cookies(urls?: string | string[])`. With
  /// URLs, only the cookies a request to one of them would carry.
  #[napi(ts_args_type = "urls?: string | string[]")]
  pub async fn cookies(&self, urls: Option<napi::Either<String, Vec<String>>>) -> Result<Vec<CookieData>> {
    let cookies = self.inner.cookies_for(&cookie_urls(urls)).await.into_napi()?;
    Ok(cookies.iter().map(CookieData::from).collect())
  }

  #[napi]
  pub async fn add_cookies(&self, cookies: Vec<CookieData>) -> Result<()> {
    let native: Vec<ferridriver::backend::CookieData> =
      cookies.iter().map(ferridriver::backend::CookieData::from).collect();
    self.inner.add_cookies(native).await.into_napi()
  }

  /// Playwright: `context.clearCookies(options?)`. Without options
  /// clears every cookie; with `{ name?, domain?, path? }` only
  /// cookies matching ALL specified filters are cleared. Each filter
  /// is `string | RegExp` — exact match for strings, `.test()` for
  /// regexes (Playwright's `server/browserContext.ts::clearCookies`).
  #[napi]
  pub async fn clear_cookies(&self, options: Option<crate::types::ClearCookieOptions>) -> Result<()> {
    self
      .inner
      .clear_cookies()
      .maybe_options(options.map(Into::into))
      .await
      .into_napi()
  }

  #[napi]
  pub async fn delete_cookie(&self, name: String, domain: Option<String>) -> Result<()> {
    let state = self.inner.state().read().await;
    let ctx = state.context(self.inner.name()).map_err(crate::error::to_napi)?;
    ctx.delete_cookie(&name, domain.as_deref()).await.into_napi()
  }

  // ── Storage state ──

  /// Playwright: `context.storageState(options?: { path?, indexedDB?,
  ///   opfs?, credentials? }): Promise<StorageState>`
  /// (`packages/playwright-core/src/client/browserContext.ts`). Exports
  /// cookies and per-origin `localStorage`, plus `IndexedDB` and the
  /// origin private file system when asked for. `path` writes the JSON
  /// to disk.
  #[napi]
  pub async fn storage_state(&self, options: Option<NapiStorageStateOptions>) -> Result<NapiStorageState> {
    let core_opts = options.map(|o| ferridriver::options::StorageStateOptions {
      path: o.path.map(std::path::PathBuf::from),
      indexed_db: o.indexed_db,
      opfs: o.opfs,
      credentials: o.credentials,
    });
    let state = self.inner.storage_state().maybe_options(core_opts).await.into_napi()?;
    NapiStorageState::try_from(state)
  }

  /// Playwright: `context.setStorageState(storageState: string |
  /// SetStorageState): Promise<void>` (Playwright 1.59). Clears existing
  /// cookies + localStorage, then applies `storageState`. A string is read as
  /// a path to a JSON file written by `storageState({ path })`; an object is
  /// the inline `{ cookies, origins }` shape.
  #[napi(ts_args_type = "storageState: string | { cookies?: any[]; origins?: any[] }")]
  pub async fn set_storage_state(&self, storage_state: serde_json::Value) -> Result<()> {
    let state = resolve_storage_state_input(storage_state).map_err(napi::Error::from_reason)?;
    self.inner.set_storage_state(&state).await.into_napi()
  }

  // ── Timeouts ──

  #[napi]
  pub fn set_default_timeout(&self, ms: f64) {
    self.inner.set_default_timeout(crate::types::f64_to_u64(ms));
  }

  #[napi]
  pub fn set_default_navigation_timeout(&self, ms: f64) {
    self.inner.set_default_navigation_timeout(crate::types::f64_to_u64(ms));
  }

  // ── Permissions ──

  /// Playwright: `browserContext.grantPermissions(permissions, options?: { origin? })`.
  #[napi]
  pub async fn grant_permissions(
    &self,
    permissions: Vec<String>,
    options: Option<crate::types::GrantPermissionsOptions>,
  ) -> Result<()> {
    let origin = options.and_then(|options| options.origin);
    self
      .inner
      .grant_permissions(&permissions, origin.as_deref())
      .await
      .into_napi()
  }

  #[napi]
  pub async fn clear_permissions(&self) -> Result<()> {
    self.inner.clear_permissions().await.into_napi()
  }

  // ── Context-level emulation ──

  /// Playwright: `browserContext.setGeolocation(geolocation | null)` —
  /// `null` clears the override.
  #[napi(ts_args_type = "geolocation: { latitude: number; longitude: number; accuracy?: number } | null")]
  pub async fn set_geolocation(&self, geolocation: Option<crate::types::GeolocationOptions>) -> Result<()> {
    let geo = geolocation.map(|g| ferridriver::options::Geolocation {
      latitude: g.latitude,
      longitude: g.longitude,
      accuracy: g.accuracy.unwrap_or(1.0),
    });
    self.inner.set_geolocation(geo).await.into_napi()
  }

  #[napi(js_name = "setExtraHTTPHeaders")]
  pub async fn set_extra_http_headers(&self, headers: crate::types::HashMap<String, String>) -> Result<()> {
    self.inner.set_extra_http_headers(&headers).await.into_napi()
  }

  #[napi]
  pub async fn set_offline(&self, offline: bool) -> Result<()> {
    self.inner.set_offline(offline).await.into_napi()
  }

  /// ferridriver extension, Chromium only: throttle every page; `null`
  /// lifts it.
  #[napi(
    ts_args_type = "conditions: 'slow-3g' | 'slow-4g' | 'fast-4g' | { latency?: number; downloadThroughput?: number; uploadThroughput?: number } | null"
  )]
  pub async fn set_network_conditions(&self, conditions: Option<serde_json::Value>) -> Result<()> {
    let conditions = conditions
      .filter(|v| !v.is_null())
      .map(serde_json::from_value::<ferridriver::options::NetworkConditions>)
      .transpose()
      .map_err(|e| napi::Error::from_reason(format!("networkConditions: {e}")))?;
    self.inner.set_network_conditions(conditions).await.into_napi()
  }

  /// ferridriver extension, Chromium only: CPU slowdown factor; `null`
  /// lifts it.
  #[napi(js_name = "setCPUThrottling")]
  pub async fn set_cpu_throttling(&self, rate: Option<f64>) -> Result<()> {
    self.inner.set_cpu_throttling(rate).await.into_napi()
  }

  /// Playwright: `browserContext.setHTTPCredentials(httpCredentials:
  /// HttpCredentials | HttpCredentials[] | null)`
  /// (`packages/playwright-core/src/client/browserContext.ts`). `null`,
  /// `undefined` and an empty list clear the stored credentials.
  #[napi(
    js_name = "setHTTPCredentials",
    ts_args_type = "httpCredentials: { username: string; password: string; origin?: string; send?: 'always' | 'unauthorized' } | Array<{ username: string; password: string; origin?: string; send?: 'always' | 'unauthorized' }> | null"
  )]
  pub async fn set_http_credentials(&self, http_credentials: Option<NapiHttpCredentialsInput>) -> Result<()> {
    let creds = http_credentials.and_then(http_credentials_list);
    self.inner.set_http_credentials(creds).await.into_napi()
  }

  /// Playwright: `browserContext.routeFromHAR(har, options?)`. Replays a
  /// `.har` file or `.zip` archive across every page in the context
  /// (current and future). With `update: true`, records the context's
  /// network into the HAR instead — written when the context closes.
  #[napi(
    js_name = "routeFromHAR",
    ts_args_type = "har: string, options?: { url?: string | RegExp, notFound?: 'abort' | 'fallback', update?: boolean, updateContent?: 'attach' | 'embed', updateMode?: 'minimal' | 'full' }"
  )]
  pub async fn route_from_har(&self, har: String, options: Option<crate::page::RouteFromHarOptionsJs>) -> Result<()> {
    let opts = crate::page::parse_har_options(options)?;
    self
      .inner
      .route_from_har(std::path::Path::new(&har))
      .options(opts)
      .await
      .map_err(crate::error::to_napi)
  }

  // ── Context-level routing ──

  /// Playwright: `browserContext.route(url, handler)`. Routes every page
  /// in this context (current and future) — the core `ContextRef::route`
  /// fans the handler out to each page.
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:377`.
  #[napi(
    ts_args_type = "urlOrPredicate: string | RegExp | ((url: URL) => boolean), handler: (route: Route) => void, options?: { times?: number }",
    ts_return_type = "Promise<void>"
  )]
  pub fn route(
    &self,
    env: napi::Env,
    url: napi::bindgen_prelude::Either3<
      String,
      crate::types::JsRegExpLike,
      napi::bindgen_prelude::Function<'_, JsUrl, PredReturn>,
    >,
    handler: napi::bindgen_prelude::Function<'_, crate::route::Route, ()>,
    options: Option<crate::types::RouteOptions>,
  ) -> Result<napi::bindgen_prelude::AsyncBlock<()>> {
    let prepared = self.routes.prepare_route(url, &handler, options)?;
    let inner = self.inner.clone();
    napi::bindgen_prelude::AsyncBlockBuilder::new(async move {
      let matcher = prepared.spec.build()?;
      inner
        .route(matcher, prepared.handler, prepared.times)
        .await
        .map_err(crate::error::to_napi)?;
      Ok(())
    })
    .build(&env)
  }

  /// Playwright: `browserContext.routeWebSocket(url, handler)`. Intercepts
  /// WebSocket connections matching `url` (glob string or `RegExp`) on every
  /// page in this context; the handler receives a live `WebSocketRoute`.
  #[napi(
    ts_args_type = "url: string | RegExp, handler: (ws: WebSocketRoute) => void",
    ts_return_type = "Promise<void>"
  )]
  pub fn route_web_socket(
    &self,
    env: napi::Env,
    url: napi::bindgen_prelude::Either<String, crate::types::JsRegExpLike>,
    handler: napi::bindgen_prelude::Function<'_, crate::web_socket_route::WebSocketRouteArg, ()>,
  ) -> Result<napi::bindgen_prelude::AsyncBlock<()>> {
    let (matcher, rust_handler) = crate::route_registry::RouteRegistry::prepare_ws_route(url, &handler)?;
    let inner = self.inner.clone();
    napi::bindgen_prelude::AsyncBlockBuilder::new(async move {
      inner
        .route_web_socket(matcher, rust_handler)
        .await
        .map_err(crate::error::to_napi)
    })
    .build(&env)
  }

  /// Playwright: `browserContext.unroute(url, handler?)`.
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:411`.
  #[napi(
    ts_args_type = "urlOrPredicate: string | RegExp | ((url: URL) => boolean), handler?: (route: Route) => void",
    ts_return_type = "Promise<void>"
  )]
  pub fn unroute(
    &self,
    env: napi::Env,
    url: napi::bindgen_prelude::Either3<
      String,
      crate::types::JsRegExpLike,
      napi::bindgen_prelude::Function<'_, JsUrl, PredReturn>,
    >,
    handler: Option<napi::bindgen_prelude::Function<'_, crate::route::Route, ()>>,
  ) -> Result<napi::bindgen_prelude::AsyncBlock<()>> {
    let Some(unroute) = self.routes.prepare_unroute(env, url, handler)? else {
      return napi::bindgen_prelude::AsyncBlockBuilder::new(async move { Ok(()) }).build(&env);
    };
    let inner = self.inner.clone();
    napi::bindgen_prelude::AsyncBlockBuilder::new(async move {
      for spec in unroute.specs {
        let m = spec.build()?;
        inner
          .unroute(&m, unroute.handler_id)
          .await
          .map_err(crate::error::to_napi)?;
      }
      Ok(())
    })
    .build(&env)
  }

  /// Playwright:
  /// `browserContext.unrouteAll(options?: { behavior?: 'wait' | 'ignoreErrors' | 'default' })`.
  /// Removes all context-scoped routes; page-scoped routes stay active.
  #[napi]
  pub async fn unroute_all(&self, options: Option<crate::page::UnrouteAllOptions>) -> Result<()> {
    let behavior = options
      .and_then(|o| o.behavior)
      .map(|b| crate::page::parse_unroute_behavior(&b))
      .transpose()?;
    self.inner.unroute_all(behavior).await.map_err(crate::error::to_napi)?;
    self.routes.clear();
    Ok(())
  }

  // ── Context-level init scripts ──

  /// Register a JS snippet to run on every new document on every page in
  /// this context. Mirrors Playwright's
  /// `browserContext.addInitScript(script, arg)` — see
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:356`.
  /// See [`crate::page::Page::add_init_script`] for argument semantics.
  #[napi(
    ts_args_type = "script: Function | string | { path?: string, content?: string }, arg?: any, options?: ExposeFunctionsOptions",
    ts_return_type = "Promise<Disposable>"
  )]
  pub async fn add_init_script(
    &self,
    script: crate::types::NapiInitScript,
    arg: crate::types::NapiInitScriptArg,
    options: Option<crate::types::ExposeFunctionsOptions>,
  ) -> Result<crate::disposable::Disposable> {
    let disposable = if options.and_then(|o| o.expose_functions).unwrap_or(false) {
      self
        .inner
        .add_init_script_exposing_functions(script.into(), arg.into_exposed()?)
        .await
    } else {
      self.inner.add_init_script(script.into(), arg.into_json()).await
    }
    .into_napi()?;
    Ok(crate::disposable::Disposable::wrap(disposable))
  }

  // ── Video recording ──

  /// Enable `recordVideo` for every page opened in this context.
  /// Playwright:
  /// `browser.newContext({ recordVideo: { dir, size? } })` —
  /// `/tmp/playwright/packages/playwright-core/types/types.d.ts:10150`.
  ///
  /// Transitional API: §4.1's `BrowserContextOptions` bag will fold
  /// this into the full context-creation options struct. Until then,
  /// call `context.setRecordVideo({ dir, size })` after
  /// `browser.newContext()` and BEFORE `context.newPage()` — pages
  /// already open do not retroactively record.
  #[napi(ts_args_type = "options: { dir: string, size?: { width: number, height: number } }")]
  pub async fn set_record_video(&self, options: RecordVideoOptionsJs) -> Result<()> {
    let opts = ferridriver::options::RecordVideoOptions {
      dir: std::path::PathBuf::from(options.dir),
      size: options.size.map(|s| ferridriver::options::VideoSize {
        width: crate::types::f64_to_u32(s.width),
        height: crate::types::f64_to_u32(s.height),
      }),
    };
    self.inner.set_record_video(opts).await.into_napi()
  }

  // ── Context-level events ──

  /// Wait for a context-level event. Playwright:
  /// `browserContext.waitForEvent(event, options?)`; resolves with the
  /// matching live class instance.
  #[napi(
    ts_args_type = "event: 'page' | 'weberror' | 'download' | 'frameattached' | 'framedetached' | 'framenavigated' | 'pageclose' | 'pageload' | 'dialog' | 'dialogclosed' | 'console' | 'request' | 'response' | 'requestfinished' | 'requestfailed' | 'close', timeoutMs?: number",
    ts_return_type = "Promise<WebError | Download | Frame | Page | Dialog | ConsoleMessage | Request | Response | BrowserContext>"
  )]
  pub fn wait_for_event(
    &self,
    env: napi::Env,
    event: String,
    timeout_ms: Option<f64>,
  ) -> Result<napi::bindgen_prelude::AsyncBlock<ContextEventArg>> {
    let timeout = crate::types::f64_to_u64(timeout_ms.unwrap_or(30000.0));
    // Listening from the call, before the promise is returned.
    let next = self.inner.wait_for_event(&event, timeout);
    napi::bindgen_prelude::AsyncBlockBuilder::new(
      async move { Ok(ContextEventArg::from_event(next.await.into_napi()?)) },
    )
    .build(&env)
  }

  // ── Exposed bindings / functions ──

  /// Playwright: `browserContext.exposeBinding(name, callback)` —
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:364`.
  ///
  /// Binds `window[name]` on every page in this context (current +
  /// future). A page-side call runs `callback(source, ...args)`, where
  /// `source` is `{ context, page, frame }` (identity strings), and
  /// settles with what it returns or throws. Returns a `Disposable`
  /// whose `dispose()` removes the binding from every page.
  #[napi(
    ts_args_type = "name: string, callback: (source: { context: BrowserContext, page: Page, frame: Frame }, ...args: any[]) => any",
    ts_return_type = "Promise<Disposable>"
  )]
  pub fn expose_binding(
    &self,
    env: napi::Env,
    name: String,
    callback: napi::bindgen_prelude::Unknown<'_>,
  ) -> Result<napi::bindgen_prelude::AsyncBlock<crate::disposable::Disposable>> {
    let binding = crate::exposed::binding(env, callback, true)?;
    let inner = self.inner.clone();
    napi::bindgen_prelude::AsyncBlockBuilder::new(async move {
      let d = inner.expose_binding(&name, binding).await.into_napi()?;
      Ok(crate::disposable::Disposable::wrap(d))
    })
    .build(&env)
  }

  /// Playwright: `browserContext.exposeFunction(name, callback)` —
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:370`.
  ///
  /// `exposeBinding` minus the `source` argument.
  #[napi(
    ts_args_type = "name: string, callback: Function",
    ts_return_type = "Promise<Disposable>"
  )]
  pub fn expose_function(
    &self,
    env: napi::Env,
    name: String,
    callback: napi::bindgen_prelude::Unknown<'_>,
  ) -> Result<napi::bindgen_prelude::AsyncBlock<crate::disposable::Disposable>> {
    let binding = crate::exposed::binding(env, callback, false)?;
    let inner = self.inner.clone();
    napi::bindgen_prelude::AsyncBlockBuilder::new(async move {
      let d = inner.expose_binding(&name, binding).await.into_napi()?;
      Ok(crate::disposable::Disposable::wrap(d))
    })
    .build(&env)
  }

  // ── Lifecycle ──

  /// Playwright: `browserContext.browser(): Browser | null` —
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:290`.
  /// Returns the parent browser, or `null` for a context not created
  /// from a `Browser`.
  #[napi]
  pub fn browser(&self) -> Option<crate::browser::Browser> {
    self.inner.browser().cloned().map(crate::browser::Browser::wrap)
  }

  /// Playwright: `browserContext.isClosed(): boolean` —
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:298`.
  #[napi]
  pub fn is_closed(&self) -> bool {
    self.inner.is_closed()
  }

  #[napi]
  pub async fn close(&self) -> Result<()> {
    self.inner.close().await.into_napi()
  }
}
