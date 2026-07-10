//! `Browser` -- NAPI binding for `ferridriver::Browser`.
//!
//! `Browser` instances are produced exclusively by the
//! [`crate::browser_type::BrowserType`] factory (`chromium()` /
//! `firefox()` / `webkit()` top-level functions). There is no
//! `Browser.launch` / `Browser.connect` static — that mirrors
//! Playwright's `chromium.launch()` / `firefox.launch()` /
//! `webkit.launch()` entry points.

use crate::error::IntoNapi;
use crate::page::Page;
use napi::Result;
use napi_derive::napi;

/// Browser instance. Manages contexts, pages, and browser lifecycle.
#[napi]
pub struct Browser {
  inner: ferridriver::Browser,
}

impl Browser {
  /// Wrap a core Browser into a NAPI Browser.
  pub(crate) fn wrap(inner: ferridriver::Browser) -> Self {
    Self { inner }
  }
}

/// Cross-thread dispatch arg for `browser.on('context')` — carries the
/// live [`ferridriver::ContextRef`] across the tokio→napi boundary; the
/// `ToNapiValue` conversion (run on the JS thread) wraps it into the
/// [`crate::context::BrowserContext`] class instance.
pub struct BrowserContextArg(ferridriver::ContextRef);

impl napi::bindgen_prelude::ToNapiValue for BrowserContextArg {
  unsafe fn to_napi_value(env: napi::sys::napi_env, val: Self) -> napi::Result<napi::sys::napi_value> {
    let wrapper = crate::context::BrowserContext::wrap(val.0);
    unsafe { crate::context::BrowserContext::to_napi_value(env, wrapper) }
  }
}

fn build_browser_event_callback(
  listener: napi::bindgen_prelude::Function<'_, BrowserContextArg, ()>,
) -> Result<ferridriver::events::BrowserEventCallback> {
  let tsfn = listener
    .build_threadsafe_function()
    .callee_handled::<false>()
    .weak::<true>()
    .max_queue_size::<0>()
    .build()?;
  Ok(std::sync::Arc::new(move |ev| match ev {
    ferridriver::events::BrowserEvent::Context(ctx) => {
      tsfn.call(
        BrowserContextArg(ctx),
        napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking,
      );
    },
  }))
}

#[napi]
impl Browser {
  /// Create a new page (tab).
  #[napi]
  pub async fn new_page(&self) -> Result<Page> {
    let page = Box::pin(self.inner.new_page()).await.into_napi()?;
    Ok(Page::wrap(page))
  }

  /// Playwright: `browser.newBrowserCDPSession()`. Attaches a raw CDP
  /// session to the browser target. Chromium-only.
  #[napi(js_name = "newBrowserCDPSession")]
  pub async fn new_browser_cdp_session(&self) -> Result<crate::cdp_session::CDPSession> {
    let session = self.inner.new_browser_cdp_session().await.into_napi()?;
    Ok(crate::cdp_session::CDPSession::wrap(session))
  }

  /// Create a new page and navigate to URL.
  #[napi]
  pub async fn new_page_with_url(&self, url: String) -> Result<Page> {
    let page = Box::pin(self.inner.new_page_with_url(&url)).await.into_napi()?;
    Ok(Page::wrap(page))
  }

  /// Get the active page for the default context.
  #[napi]
  pub async fn page(&self) -> Result<Page> {
    let page = Box::pin(self.inner.page()).await.into_napi()?;
    Ok(Page::wrap(page))
  }

  /// Create a new isolated browser context.
  /// Mirrors Playwright's `browser.newContext(options?)` —
  /// `/tmp/playwright/packages/playwright-core/types/types.d.ts:22229`.
  /// Every option field is optional; pass `undefined` or `{}` for
  /// no-options.
  ///
  /// The `ts_args_type` below forces the generated `.d.ts` to carry
  /// Playwright's exact string-literal unions (e.g. `colorScheme:
  /// 'light' | 'dark' | 'no-preference' | null`) — napi-rs's default
  /// inference would widen them to `string`.
  #[napi(ts_args_type = "options?: {
    acceptDownloads?: boolean;
    baseURL?: string;
    bypassCSP?: boolean;
    colorScheme?: 'light' | 'dark' | 'no-preference' | null;
    contrast?: 'no-preference' | 'more' | null;
    deviceScaleFactor?: number;
    extraHTTPHeaders?: Record<string, string>;
    forcedColors?: 'active' | 'none' | null;
    geolocation?: { latitude: number; longitude: number; accuracy?: number };
    hasTouch?: boolean;
    httpCredentials?: { username: string; password: string; origin?: string; send?: 'always' | 'unauthorized' };
    ignoreHTTPSErrors?: boolean;
    isMobile?: boolean;
    javaScriptEnabled?: boolean;
    locale?: string;
    offline?: boolean;
    permissions?: string[];
    proxy?: { server: string; bypass?: string; username?: string; password?: string };
    recordVideo?: { dir: string; size?: { width: number; height: number } };
    reducedMotion?: 'reduce' | 'no-preference' | null;
    screen?: { width: number; height: number };
    serviceWorkers?: 'allow' | 'block';
    strictSelectors?: boolean;
    timezoneId?: string;
    userAgent?: string;
    viewport?: { width: number; height: number };
    /**
     * Set to `true` to opt out of viewport emulation entirely —
     * equivalent to Playwright's `viewport: null`. napi-rs cannot
     * distinguish JS `null` from `undefined`, so the opt-out is
     * exposed as this explicit boolean. Defaults to `false`.
     */
    disableViewport?: boolean;
  }")]
  pub fn new_context(
    &self,
    options: Option<crate::context::NapiBrowserContextOptions>,
  ) -> Result<crate::context::BrowserContext> {
    let core = options.map(crate::context::NapiBrowserContextOptions::into_core);
    // The core builder resolves synchronously (context registration is pure
    // bookkeeping); block_on keeps this method's sync JS shape.
    let ctx = napi::bindgen_prelude::block_on(std::future::IntoFuture::into_future(
      self.inner.new_context().maybe_options(core),
    ))
    .into_napi()?;
    Ok(crate::context::BrowserContext::wrap(ctx))
  }

  /// Get the default browser context.
  #[napi]
  pub fn default_context(&self) -> crate::context::BrowserContext {
    crate::context::BrowserContext::wrap(self.inner.default_context())
  }

  /// Register a browser-level event listener. Supports `'context'` —
  /// fired when a new context is created via [`Self::new_context`].
  /// Playwright: `browser.on('context', (context: BrowserContext) => …)`.
  /// Returns a numeric listener id for [`Self::off`].
  #[napi(ts_args_type = "event: 'context', listener: (context: BrowserContext) => void")]
  pub fn on(&self, event: String, listener: napi::bindgen_prelude::Function<'_, BrowserContextArg, ()>) -> Result<f64> {
    let callback = build_browser_event_callback(listener)?;
    let id = self.inner.on(&event, callback);
    #[allow(clippy::cast_precision_loss)]
    Ok(id.0 as f64)
  }

  /// One-shot variant of [`Self::on`].
  #[napi(ts_args_type = "event: 'context', listener: (context: BrowserContext) => void")]
  pub fn once(
    &self,
    event: String,
    listener: napi::bindgen_prelude::Function<'_, BrowserContextArg, ()>,
  ) -> Result<f64> {
    let callback = build_browser_event_callback(listener)?;
    let id = self.inner.once(&event, callback);
    #[allow(clippy::cast_precision_loss)]
    Ok(id.0 as f64)
  }

  /// Remove a browser-level listener by id.
  #[napi]
  pub fn off(&self, listener_id: f64) {
    self
      .inner
      .off(ferridriver::events::ListenerId(crate::types::f64_to_u64(listener_id)));
  }

  /// Wait for a browser-level event. Playwright:
  /// `browser.waitForEvent(event, options?)`. Supports `'context'`.
  #[napi(
    ts_args_type = "event: 'context', timeoutMs?: number",
    ts_return_type = "Promise<BrowserContext>"
  )]
  pub async fn wait_for_event(&self, event: String, timeout_ms: Option<f64>) -> Result<crate::context::BrowserContext> {
    let timeout = crate::types::f64_to_u64(timeout_ms.unwrap_or(30000.0));
    let ev = self.inner.wait_for_event(&event, timeout).await.into_napi()?;
    match ev {
      ferridriver::events::BrowserEvent::Context(ctx) => Ok(crate::context::BrowserContext::wrap(ctx)),
    }
  }

  /// Close the browser. Accepts Playwright's `{ reason? }` options shape;
  /// the reason is surfaced on `TargetClosed` errors emitted to
  /// in-flight operations on this browser's pages/contexts.
  #[napi]
  pub async fn close(&self, options: Option<crate::types::BrowserCloseOptions>) -> Result<()> {
    let opts: Option<ferridriver::options::BrowserCloseOptions> = options.map(Into::into);
    self.inner.close().maybe_options(opts).await.into_napi()
  }

  /// List all browser contexts. Sync — mirrors Playwright's
  /// `browser.contexts(): BrowserContext[]`.
  #[napi]
  pub fn contexts(&self) -> Vec<crate::context::BrowserContext> {
    self
      .inner
      .contexts()
      .into_iter()
      .map(crate::context::BrowserContext::wrap)
      .collect()
  }

  /// Real product version string (e.g. `"HeadlessChrome/120.0.6099.109"`).
  #[napi(getter)]
  pub fn version(&self) -> String {
    self.inner.version().to_string()
  }

  /// Whether the browser is connected. Sync — mirrors Playwright's
  /// `browser.isConnected(): boolean`.
  #[napi]
  pub fn is_connected(&self) -> bool {
    self.inner.is_connected()
  }

  /// Publish this browser under a named session so other processes (the
  /// `ferridriver` CLI, another agent) can attach to it. Mirrors Playwright's
  /// `browser.bind(title, options): Promise<{ endpoint }>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/browser.ts:132`).
  ///
  /// `title` is the session id. `host`/`port` bind over TCP (`ws://`
  /// endpoint); otherwise a Unix-domain socket is used. Returns the resolved
  /// endpoint clients connect to.
  #[napi(
    ts_args_type = "title: string, options?: {
    workspaceDir?: string;
    metadata?: Record<string, any>;
    host?: string;
    port?: number;
  }",
    ts_return_type = "Promise<{ endpoint: string }>"
  )]
  pub async fn bind(&self, title: String, options: Option<NapiBindOptions>) -> Result<BindResult> {
    let opts = options.unwrap_or_default();
    let endpoint = ferridriver_session::bind_global(&self.inner, &title, opts.into_core(), None)
      .await
      .map_err(|e| napi::Error::from_reason(e.to_string()))?;
    Ok(BindResult { endpoint })
  }

  /// Stop accepting new connections for the bound session and remove its
  /// registry entry. Mirrors Playwright's `browser.unbind(): Promise<void>`.
  /// A no-op if the browser was never bound.
  #[napi]
  #[allow(clippy::unused_async, clippy::unused_async_trait_impl)] // NAPI requires async to surface a JS Promise (Playwright parity)
  pub async fn unbind(&self) -> Result<()> {
    ferridriver_session::unbind_browser(&self.inner).map_err(|e| napi::Error::from_reason(e.to_string()))
  }
}

/// Options for [`Browser::bind`]. Field names mirror Playwright's option bag.
#[napi(object)]
#[derive(Default)]
pub struct NapiBindOptions {
  pub workspace_dir: Option<String>,
  pub metadata: Option<serde_json::Value>,
  pub host: Option<String>,
  pub port: Option<u32>,
}

impl NapiBindOptions {
  fn into_core(self) -> ferridriver_session::BindOptions {
    ferridriver_session::BindOptions {
      workspace_dir: self.workspace_dir,
      metadata: self.metadata,
      host: self.host,
      port: self.port.and_then(|p| u16::try_from(p).ok()),
    }
  }
}

/// The `{ endpoint }` object returned by [`Browser::bind`].
#[napi(object)]
pub struct BindResult {
  pub endpoint: String,
}
