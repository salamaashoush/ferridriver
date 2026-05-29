//! `BrowserContext` class -- NAPI binding for `ferridriver::ContextRef`.

use crate::error::IntoNapi;
use crate::page::Page;
use crate::page::{JsUrl, MatcherSpec, PredReturn, PredRoute, resolve_pred};
use crate::types::CookieData;
use napi::Result;
use napi_derive::napi;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Isolated browser context with its own cookies, storage, and permissions.
/// Mirrors Playwright's `BrowserContext`.
#[napi]
pub struct BrowserContext {
  inner: ferridriver::ContextRef,
  /// Predicate-route registry, identical in role to `Page::predicate_routes`:
  /// keeps each `context.route(predicateFn, handler)` JS function so that
  /// `context.unroute(fn)` can match it by `===`.
  predicate_routes: Arc<Mutex<Vec<PredRoute>>>,
}

impl BrowserContext {
  pub(crate) fn wrap(inner: ferridriver::ContextRef) -> Self {
    Self {
      inner,
      predicate_routes: Arc::new(Mutex::new(Vec::new())),
    }
  }
}

#[napi]
impl BrowserContext {
  /// Context name.
  #[napi(getter)]
  pub fn name(&self) -> String {
    self.inner.name().to_string()
  }

  /// Create a new page in this context.
  #[napi]
  pub async fn new_page(&self) -> Result<Page> {
    let page = Box::pin(self.inner.new_page()).await.into_napi()?;
    Ok(Page::wrap(page))
  }

  /// Get all pages in this context.
  #[napi]
  pub async fn pages(&self) -> Result<Vec<Page>> {
    let pages = self.inner.pages().await.into_napi()?;
    Ok(pages.into_iter().map(Page::wrap).collect())
  }

  // ── Cookies ──

  #[napi]
  pub async fn cookies(&self) -> Result<Vec<CookieData>> {
    let cookies = self.inner.cookies().await.into_napi()?;
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
  /// cookies matching ALL specified filters are cleared.
  ///
  /// Filter values are exact-match strings — Playwright's TS API
  /// accepts `string | RegExp` here too; regex filters are tracked
  /// under "Section B" pending a Rust core extension.
  #[napi]
  pub async fn clear_cookies(&self, options: Option<crate::types::ClearCookieOptions>) -> Result<()> {
    match options {
      None => self.inner.clear_cookies().await.into_napi(),
      Some(opts) => {
        let core: ferridriver::backend::ClearCookieOptions = opts.into();
        self.inner.clear_cookies_filtered(&core).await.into_napi()
      },
    }
  }

  #[napi]
  pub async fn delete_cookie(&self, name: String, domain: Option<String>) -> Result<()> {
    let state = self.inner.state().read().await;
    let ctx = state.context(self.inner.name()).map_err(crate::error::to_napi)?;
    ctx.delete_cookie(&name, domain.as_deref()).await.into_napi()
  }

  // ── Storage state ──

  /// Playwright: `context.storageState(options?: { path?, indexedDB? })
  ///   : Promise<{ cookies, origins }>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:460`).
  /// Exports cookies + per-origin localStorage. `path` writes the JSON to disk;
  /// `indexedDB` is accepted for parity but IndexedDB is not yet collected.
  #[napi]
  pub async fn storage_state(&self, options: Option<NapiStorageStateOptions>) -> Result<NapiStorageState> {
    let core_opts = options.map(|o| ferridriver::options::StorageStateOptions {
      path: o.path.map(std::path::PathBuf::from),
      indexed_db: o.indexed_db,
    });
    let state = self.inner.storage_state(core_opts).await.into_napi()?;
    Ok(NapiStorageState::from(state))
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

  #[napi]
  pub async fn grant_permissions(&self, permissions: Vec<String>, origin: Option<String>) -> Result<()> {
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

  #[napi]
  pub async fn set_geolocation(&self, latitude: f64, longitude: f64, accuracy: Option<f64>) -> Result<()> {
    self
      .inner
      .set_geolocation(latitude, longitude, accuracy.unwrap_or(1.0))
      .await
      .into_napi()
  }

  #[napi]
  pub async fn set_extra_http_headers(&self, headers: HashMap<String, String>) -> Result<()> {
    let mut fx = rustc_hash::FxHashMap::default();
    for (k, v) in headers {
      fx.insert(k, v);
    }
    self.inner.set_extra_http_headers(&fx).await.into_napi()
  }

  #[napi]
  pub async fn set_offline(&self, offline: bool) -> Result<()> {
    self.inner.set_offline(offline).await.into_napi()
  }

  /// Playwright: `browserContext.setHTTPCredentials(httpCredentials |
  /// null)` —
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:355`.
  /// Passing `null`/`undefined` clears stored credentials.
  #[napi(
    js_name = "setHTTPCredentials",
    ts_args_type = "httpCredentials: { username: string, password: string, origin?: string, send?: 'always' | 'unauthorized' } | null"
  )]
  pub async fn set_http_credentials(&self, http_credentials: Option<NapiHttpCredentials>) -> Result<()> {
    let creds = http_credentials.map(ferridriver::options::HttpCredentials::from);
    self.inner.set_http_credentials(creds).await.into_napi()
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
  #[allow(clippy::trivially_copy_pass_by_ref)]
  pub fn route(
    &self,
    env: &napi::Env,
    url: napi::bindgen_prelude::Either3<
      String,
      crate::types::JsRegExpLike,
      napi::bindgen_prelude::Function<'_, JsUrl, PredReturn>,
    >,
    handler: napi::threadsafe_function::ThreadsafeFunction<
      crate::route::Route,
      (),
      crate::route::Route,
      napi::Status,
      false,
      true,
      0,
    >,
    options: Option<crate::types::RouteOptions>,
  ) -> Result<napi::bindgen_prelude::AsyncBlock<()>> {
    use napi::bindgen_prelude::Either3;
    let times = options.and_then(|o| o.times_u32());
    let nb = napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking;
    let handler = std::sync::Arc::new(handler);
    let (spec, rust_handler): (MatcherSpec, ferridriver::route::RouteHandler) = match url {
      Either3::A(glob) => (
        MatcherSpec::Glob(glob),
        std::sync::Arc::new(move |route| {
          handler.call(crate::route::Route::wrap(route), nb);
        }),
      ),
      Either3::B(re) => (
        MatcherSpec::Regex {
          source: re.source,
          flags: re.flags,
        },
        std::sync::Arc::new(move |route| {
          handler.call(crate::route::Route::wrap(route), nb);
        }),
      ),
      Either3::C(predicate) => {
        let pred_ref = predicate.create_ref()?;
        let ptsfn = predicate
          .build_threadsafe_function::<JsUrl>()
          .callee_handled::<false>()
          .weak::<false>()
          .max_queue_size::<0>()
          .build()?;
        let ptsfn = std::sync::Arc::new(ptsfn);
        let m = ferridriver::url_matcher::UrlMatcher::predicate(|_| true);
        self
          .predicate_routes
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner)
          .push(PredRoute {
            matcher: m.clone(),
            pred_ref,
          });
        (
          MatcherSpec::Ready(m),
          std::sync::Arc::new(move |route| {
            let ptsfn = std::sync::Arc::clone(&ptsfn);
            let handler = std::sync::Arc::clone(&handler);
            let url = JsUrl::new(route.request().url.clone());
            tokio::spawn(async move {
              let truthy = match ptsfn.call_async(url).await {
                Ok(r) => resolve_pred(r).await,
                Err(_) => false,
              };
              if truthy {
                handler.call(crate::route::Route::wrap(route), nb);
              } else {
                route.continue_route(ferridriver::route::ContinueOverrides::default());
              }
            });
          }),
        )
      },
    };

    let inner = self.inner.clone();
    napi::bindgen_prelude::AsyncBlockBuilder::new(async move {
      let matcher = spec.build()?;
      inner.route(matcher, rust_handler, times).await.map_err(crate::error::to_napi)?;
      Ok(())
    })
    .build(env)
  }

  /// Playwright: `browserContext.unroute(url, handler?)`.
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:411`.
  #[napi(
    ts_args_type = "urlOrPredicate: string | RegExp | ((url: URL) => boolean)",
    ts_return_type = "Promise<void>"
  )]
  #[allow(clippy::trivially_copy_pass_by_ref)]
  pub fn unroute(
    &self,
    env: &napi::Env,
    url: napi::bindgen_prelude::Either3<
      String,
      crate::types::JsRegExpLike,
      napi::bindgen_prelude::Function<'_, JsUrl, PredReturn>,
    >,
  ) -> Result<napi::bindgen_prelude::AsyncBlock<()>> {
    use napi::bindgen_prelude::Either3;
    let specs: Vec<MatcherSpec> = match url {
      Either3::A(glob) => vec![MatcherSpec::Glob(glob)],
      Either3::B(re) => vec![MatcherSpec::Regex {
        source: re.source,
        flags: re.flags,
      }],
      Either3::C(predicate) => {
        let in_ref = predicate.create_ref()?;
        let mut guard = self
          .predicate_routes
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut hit = Vec::new();
        let mut i = 0;
        while i < guard.len() {
          let same = {
            let a = in_ref.borrow_back(env)?;
            let b = guard[i].pred_ref.borrow_back(env)?;
            env.strict_equals(a, b)?
          };
          if same {
            hit.push(MatcherSpec::Ready(guard.remove(i).matcher));
          } else {
            i += 1;
          }
        }
        hit
      },
    };
    let inner = self.inner.clone();
    napi::bindgen_prelude::AsyncBlockBuilder::new(async move {
      for spec in specs {
        let m = spec.build()?;
        inner.unroute(&m).await.map_err(crate::error::to_napi)?;
      }
      Ok(())
    })
    .build(env)
  }

  // ── Context-level init scripts ──

  /// Register a JS snippet to run on every new document on every page in
  /// this context. Mirrors Playwright's
  /// `browserContext.addInitScript(script, arg)` — see
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:356`.
  /// See [`crate::page::Page::add_init_script`] for argument semantics.
  #[napi(
    ts_args_type = "script: Function | string | { path?: string, content?: string }, arg?: any",
    ts_return_type = "Promise<Disposable>"
  )]
  pub async fn add_init_script(
    &self,
    script: crate::types::NapiInitScript,
    arg: crate::types::NapiInitScriptArg,
  ) -> Result<crate::disposable::Disposable> {
    let disposable = self.inner.add_init_script(script.into(), arg.0).await.into_napi()?;
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
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        width: s.width.max(0.0) as u32,
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        height: s.height.max(0.0) as u32,
      }),
    };
    self.inner.set_record_video(opts).await.into_napi()
  }

  // ── Context-level events ──

  /// Register a context-level event listener. Currently supports
  /// `'weberror'` — unhandled errors / rejections from any page in
  /// this context. Playwright:
  /// `browserContext.on('weberror', (webError: WebError) => …)` —
  /// callback receives a live [`crate::web_error::WebError`] class
  /// instance (not a snapshot). Returns a numeric listener id for
  /// removal via [`Self::off`].
  #[napi(ts_args_type = "event: 'weberror', listener: (webError: WebError) => void")]
  pub fn on(
    &self,
    event: String,
    listener: napi::bindgen_prelude::Function<'_, crate::web_error::WebErrorArg, ()>,
  ) -> Result<f64> {
    let callback = build_context_event_callback(listener)?;
    let id = self.inner.on(&event, callback);
    #[allow(clippy::cast_precision_loss)]
    Ok(id.0 as f64)
  }

  /// One-shot variant of [`Self::on`]. Auto-removed after first match.
  #[napi(ts_args_type = "event: 'weberror', listener: (webError: WebError) => void")]
  pub fn once(
    &self,
    event: String,
    listener: napi::bindgen_prelude::Function<'_, crate::web_error::WebErrorArg, ()>,
  ) -> Result<f64> {
    let callback = build_context_event_callback(listener)?;
    let id = self.inner.once(&event, callback);
    #[allow(clippy::cast_precision_loss)]
    Ok(id.0 as f64)
  }

  /// Remove a context-level listener by id.
  #[napi]
  pub fn off(&self, listener_id: f64) {
    self
      .inner
      .off(ferridriver::events::ListenerId(crate::types::f64_to_u64(listener_id)));
  }

  /// Wait for a context-level event. Playwright:
  /// `browserContext.waitForEvent(event, options?)`. Currently
  /// supports `'weberror'` — returns the live [`crate::web_error::WebError`]
  /// handle.
  #[napi(
    ts_args_type = "event: 'weberror', timeoutMs?: number",
    ts_return_type = "Promise<WebError>"
  )]
  pub async fn wait_for_event(&self, event: String, timeout_ms: Option<f64>) -> Result<crate::web_error::WebError> {
    let timeout = crate::types::f64_to_u64(timeout_ms.unwrap_or(30000.0));
    let ev = self.inner.wait_for_event(&event, timeout).await.into_napi()?;
    match ev {
      ferridriver::events::ContextEvent::WebError(err) => Ok(crate::web_error::WebError::from_core(err)),
    }
  }

  // ── Exposed bindings / functions ──

  /// Playwright: `browserContext.exposeBinding(name, callback)` —
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:364`.
  ///
  /// Binds `window[name]` on every page in this context (current +
  /// future). The page-side call routes back into `callback`, invoked
  /// as `callback(source, args)` where `source` is
  /// `{ context, page, frame }` (identity strings) and `args` is the
  /// page-side call argument array.
  ///
  /// NAPI convention (matches `page.exposeFunction`): the callback is
  /// fire-and-forget — the page-side call resolves to `null` while the
  /// JS callback runs. Return-value delivery + arg spreading lives on
  /// the QuickJS/script surface. Returns a `Disposable` whose
  /// `dispose()` removes the binding from every page.
  #[napi(
    ts_args_type = "name: string, callback: (source: { context: string, page: string, frame: string }, args: unknown[]) => void",
    ts_return_type = "Promise<Disposable>"
  )]
  // napi `AsyncBlockBuilder::build` takes `&Env`; matches the sibling `route` binding.
  #[allow(clippy::trivially_copy_pass_by_ref)]
  pub fn expose_binding(
    &self,
    env: &napi::Env,
    name: String,
    callback: napi::bindgen_prelude::Function<
      '_,
      napi::bindgen_prelude::FnArgs<(BindingSourceJs, serde_json::Value)>,
      (),
    >,
  ) -> Result<napi::bindgen_prelude::AsyncBlock<crate::disposable::Disposable>> {
    let tsfn = callback
      .build_threadsafe_function()
      .callee_handled::<false>()
      .weak::<true>()
      .max_queue_size::<0>()
      .build()?;
    let binding: ferridriver::ExposedBinding = std::sync::Arc::new(move |source, args| {
      let arg: napi::bindgen_prelude::FnArgs<(BindingSourceJs, serde_json::Value)> = (
        BindingSourceJs {
          context: source.context,
          page: source.page,
          frame: source.frame,
        },
        serde_json::Value::Array(args),
      )
        .into();
      tsfn.call(arg, napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking);
      Box::pin(async move { serde_json::Value::Null })
    });
    let inner = self.inner.clone();
    napi::bindgen_prelude::AsyncBlockBuilder::new(async move {
      let d = inner.expose_binding(&name, binding).await.into_napi()?;
      Ok(crate::disposable::Disposable::wrap(d))
    })
    .build(env)
  }

  /// Playwright: `browserContext.exposeFunction(name, callback)` —
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:370`.
  ///
  /// `exposeFunction` is `exposeBinding` minus the `source` argument:
  /// the callback receives only the page-side call argument array.
  /// Same fire-and-forget contract as `exposeBinding` on NAPI.
  #[napi(
    ts_args_type = "name: string, callback: (args: unknown[]) => void",
    ts_return_type = "Promise<Disposable>"
  )]
  // napi `AsyncBlockBuilder::build` takes `&Env`; matches the sibling `route` binding.
  #[allow(clippy::trivially_copy_pass_by_ref)]
  pub fn expose_function(
    &self,
    env: &napi::Env,
    name: String,
    callback: napi::bindgen_prelude::Function<'_, serde_json::Value, ()>,
  ) -> Result<napi::bindgen_prelude::AsyncBlock<crate::disposable::Disposable>> {
    let tsfn = callback
      .build_threadsafe_function()
      .callee_handled::<false>()
      .weak::<true>()
      .max_queue_size::<0>()
      .build()?;
    let func: ferridriver::ExposedFn = std::sync::Arc::new(move |args| {
      tsfn.call(
        serde_json::Value::Array(args),
        napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking,
      );
      Box::pin(async move { serde_json::Value::Null })
    });
    let inner = self.inner.clone();
    napi::bindgen_prelude::AsyncBlockBuilder::new(async move {
      let d = inner.expose_function(&name, func).await.into_napi()?;
      Ok(crate::disposable::Disposable::wrap(d))
    })
    .build(env)
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

/// JS-visible shape of Playwright's `BindingSource`
/// (`/tmp/playwright/packages/playwright-core/types/structs.d.ts:45`).
/// ferridriver delivers identity strings (composite context key, page
/// id, frame id) rather than live `BrowserContext`/`Page`/`Frame`
/// handles because the binding dispatch runs outside the handle
/// lifetime.
#[napi(object)]
pub struct BindingSourceJs {
  pub context: String,
  pub page: String,
  pub frame: String,
}

/// Lower a JS listener `Function<'_>` (which is `!Send` because it
/// holds a raw NAPI value pointer) into a pure-Send
/// [`ContextEventCallback`]. Kept in a separate sync function so the
/// async `BrowserContext::on` / `once` generators don't capture the
/// `!Send` `Function<'_>` across their await points.
///
/// The threadsafe function's arg type is [`crate::web_error::WebErrorArg`],
/// which [`napi::bindgen_prelude::ToNapiValue`]-converts (inside the
/// JS thread) into a live NAPI [`crate::web_error::WebError`] class
/// instance — matching Playwright's
/// `browserContext.on('weberror', (webError: WebError) => any)` byte
/// for byte.
/// NAPI shape for Playwright's
/// `recordVideo?: { dir: string, size?: { width, height } }` option —
/// `/tmp/playwright/packages/playwright-core/types/types.d.ts:10150`.
#[napi(object)]
pub struct RecordVideoOptionsJs {
  pub dir: String,
  pub size: Option<VideoSizeJs>,
}

/// NAPI shape for Playwright's `recordVideo.size: { width, height }`.
#[napi(object)]
pub struct VideoSizeJs {
  pub width: f64,
  pub height: f64,
}

/// NAPI shape for Playwright's
/// `BrowserContextOptions` —
/// `/tmp/playwright/packages/playwright-core/types/types.d.ts:22229`.
/// Every field is optional. Fields that must mirror Playwright's
/// string unions (e.g. `colorScheme: null | "light" | "dark" |
/// "no-preference"`) use string passthrough here with the exact union
/// rendered via `#[napi(ts_args_type = ...)]` on the consuming
/// `browser.newContext(options)` method.
#[napi(object)]
pub struct NapiBrowserContextOptions {
  pub accept_downloads: Option<bool>,
  pub base_url: Option<String>,
  pub bypass_csp: Option<bool>,
  pub color_scheme: Option<String>,
  pub contrast: Option<String>,
  pub device_scale_factor: Option<f64>,
  pub extra_http_headers: Option<HashMap<String, String>>,
  pub forced_colors: Option<String>,
  pub geolocation: Option<NapiGeolocation>,
  pub has_touch: Option<bool>,
  pub http_credentials: Option<NapiHttpCredentials>,
  pub ignore_https_errors: Option<bool>,
  pub is_mobile: Option<bool>,
  pub java_script_enabled: Option<bool>,
  pub locale: Option<String>,
  pub offline: Option<bool>,
  pub permissions: Option<Vec<String>>,
  pub proxy: Option<NapiProxyConfig>,
  pub record_video: Option<RecordVideoOptionsJs>,
  pub reduced_motion: Option<String>,
  pub screen: Option<NapiScreenSize>,
  pub service_workers: Option<String>,
  pub strict_selectors: Option<bool>,
  pub timezone_id: Option<String>,
  pub user_agent: Option<String>,
  /// Playwright allows `viewport: null` to opt out of viewport
  /// emulation. NAPI inbound deserialisation treats `null` and
  /// `undefined` identically, so we expose an explicit boolean
  /// `disable_viewport` for the `null` case alongside `viewport` for
  /// a concrete size. Callers pass `{ width, height }` to set, or
  /// `{ disableViewport: true }` to opt out. Absent fields =
  /// `undefined` = "browser default".
  pub viewport: Option<NapiViewportSize>,
  pub disable_viewport: Option<bool>,
}

#[napi(object)]
pub struct NapiGeolocation {
  pub latitude: f64,
  pub longitude: f64,
  pub accuracy: Option<f64>,
}

#[napi(object)]
pub struct NapiHttpCredentials {
  pub username: String,
  pub password: String,
  pub origin: Option<String>,
  pub send: Option<String>,
}

impl From<NapiHttpCredentials> for ferridriver::options::HttpCredentials {
  fn from(c: NapiHttpCredentials) -> Self {
    use ferridriver::options as fo;
    fo::HttpCredentials {
      username: c.username,
      password: c.password,
      origin: c.origin,
      send: c.send.and_then(|s| match s.as_str() {
        "always" => Some(fo::HttpCredentialsSend::Always),
        "unauthorized" => Some(fo::HttpCredentialsSend::Unauthorized),
        _ => None,
      }),
    }
  }
}

#[napi(object)]
pub struct NapiProxyConfig {
  pub server: String,
  pub bypass: Option<String>,
  pub username: Option<String>,
  pub password: Option<String>,
}

#[napi(object)]
pub struct NapiScreenSize {
  pub width: f64,
  pub height: f64,
}

#[napi(object)]
pub struct NapiViewportSize {
  pub width: f64,
  pub height: f64,
}

/// NAPI shape for Playwright's `storageState(options?)` —
/// `{ path?: string, indexedDB?: boolean }`
/// (`/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:460`).
#[napi(object)]
pub struct NapiStorageStateOptions {
  pub path: Option<String>,
  pub indexed_db: Option<bool>,
}

/// A single `localStorage` entry — Playwright `NameValue`.
#[napi(object)]
pub struct NapiNameValue {
  pub name: String,
  pub value: String,
}

/// Per-origin storage snapshot — Playwright `OriginStorage` (minus indexedDB).
#[napi(object)]
pub struct NapiOriginState {
  pub origin: String,
  pub local_storage: Vec<NapiNameValue>,
}

/// Result of `context.storageState()` — Playwright `StorageState`.
#[napi(object)]
pub struct NapiStorageState {
  pub cookies: Vec<CookieData>,
  pub origins: Vec<NapiOriginState>,
}

impl From<ferridriver::options::StorageState> for NapiStorageState {
  fn from(s: ferridriver::options::StorageState) -> Self {
    Self {
      cookies: s.cookies.iter().map(CookieData::from).collect(),
      origins: s
        .origins
        .into_iter()
        .map(|o| NapiOriginState {
          origin: o.origin,
          local_storage: o
            .local_storage
            .into_iter()
            .map(|nv| NapiNameValue {
              name: nv.name,
              value: nv.value,
            })
            .collect(),
        })
        .collect(),
    }
  }
}

impl NapiBrowserContextOptions {
  /// Lower into the core [`ferridriver::options::BrowserContextOptions`]
  /// bag. Unknown string values for enum-typed fields fall back to
  /// `None` (same-as-absent), matching Playwright's lenient client-side
  /// parsing.
  #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
  #[must_use]
  pub fn into_core(self) -> ferridriver::options::BrowserContextOptions {
    use ferridriver::options as fo;
    let color_scheme = self
      .color_scheme
      .map_or(fo::MediaOverride::Unchanged, |s| match s.as_str() {
        "null" => fo::MediaOverride::Disabled,
        other => fo::MediaOverride::Set(other.to_string()),
      });
    let contrast = self
      .contrast
      .map_or(fo::MediaOverride::Unchanged, |s| match s.as_str() {
        "null" => fo::MediaOverride::Disabled,
        other => fo::MediaOverride::Set(other.to_string()),
      });
    let forced_colors = self
      .forced_colors
      .map_or(fo::MediaOverride::Unchanged, |s| match s.as_str() {
        "null" => fo::MediaOverride::Disabled,
        other => fo::MediaOverride::Set(other.to_string()),
      });
    let reduced_motion = self
      .reduced_motion
      .map_or(fo::MediaOverride::Unchanged, |s| match s.as_str() {
        "null" => fo::MediaOverride::Disabled,
        other => fo::MediaOverride::Set(other.to_string()),
      });
    let viewport = if self.disable_viewport == Some(true) {
      fo::ViewportOption::Null
    } else if let Some(vp) = self.viewport {
      fo::ViewportOption::Size {
        width: vp.width.max(0.0) as i64,
        height: vp.height.max(0.0) as i64,
      }
    } else {
      fo::ViewportOption::Default
    };
    let extra_http_headers = self.extra_http_headers.map(|h| {
      let mut fx = rustc_hash::FxHashMap::default();
      for (k, v) in h {
        fx.insert(k, v);
      }
      fx
    });
    let http_credentials = self.http_credentials.map(fo::HttpCredentials::from);
    let proxy = self.proxy.map(|p| fo::ProxyConfig {
      server: p.server,
      bypass: p.bypass,
      username: p.username,
      password: p.password,
    });
    let record_video = self.record_video.map(|rv| fo::RecordVideoOptions {
      dir: std::path::PathBuf::from(rv.dir),
      size: rv.size.map(|s| fo::VideoSize {
        width: s.width.max(0.0) as u32,
        height: s.height.max(0.0) as u32,
      }),
    });
    let screen = self.screen.map(|s| fo::ScreenSize {
      width: s.width.max(0.0) as i64,
      height: s.height.max(0.0) as i64,
    });
    let service_workers = self.service_workers.and_then(|s| match s.as_str() {
      "allow" => Some(fo::ServiceWorkerPolicy::Allow),
      "block" => Some(fo::ServiceWorkerPolicy::Block),
      _ => None,
    });
    fo::BrowserContextOptions {
      accept_downloads: self.accept_downloads,
      base_url: self.base_url,
      bypass_csp: self.bypass_csp,
      color_scheme,
      contrast,
      device_scale_factor: self.device_scale_factor,
      extra_http_headers,
      forced_colors,
      geolocation: self.geolocation.map(|g| fo::Geolocation {
        latitude: g.latitude,
        longitude: g.longitude,
        accuracy: g.accuracy.unwrap_or(0.0),
      }),
      has_touch: self.has_touch,
      http_credentials,
      ignore_https_errors: self.ignore_https_errors,
      is_mobile: self.is_mobile,
      java_script_enabled: self.java_script_enabled,
      locale: self.locale,
      offline: self.offline,
      permissions: self.permissions,
      proxy,
      record_har: None,
      record_video,
      reduced_motion,
      screen,
      service_workers,
      storage_state: None,
      strict_selectors: self.strict_selectors,
      timezone_id: self.timezone_id,
      user_agent: self.user_agent,
      viewport,
    }
  }
}

fn build_context_event_callback(
  listener: napi::bindgen_prelude::Function<'_, crate::web_error::WebErrorArg, ()>,
) -> Result<ferridriver::events::ContextEventCallback> {
  let tsfn = listener
    .build_threadsafe_function()
    .callee_handled::<false>()
    .weak::<true>()
    .max_queue_size::<0>()
    .build()?;
  Ok(std::sync::Arc::new(move |ev| match ev {
    ferridriver::events::ContextEvent::WebError(err) => {
      tsfn.call(
        crate::web_error::WebErrorArg(err),
        napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking,
      );
    },
  }))
}
