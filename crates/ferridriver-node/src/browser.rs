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

#[napi]
impl Browser {
  /// Create a new page (tab).
  #[napi]
  pub async fn new_page(&self) -> Result<Page> {
    let page = Box::pin(self.inner.new_page()).await.into_napi()?;
    Ok(Page::wrap(page))
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
  ) -> crate::context::BrowserContext {
    let core = options.map(crate::context::NapiBrowserContextOptions::into_core);
    crate::context::BrowserContext::wrap(self.inner.new_context(core))
  }

  /// Get the default browser context.
  #[napi]
  pub fn default_context(&self) -> crate::context::BrowserContext {
    crate::context::BrowserContext::wrap(self.inner.default_context())
  }

  /// Close the browser. Accepts Playwright's `{ reason? }` options shape;
  /// the reason is surfaced on `TargetClosed` errors emitted to
  /// in-flight operations on this browser's pages/contexts.
  #[napi]
  pub async fn close(&self, options: Option<crate::types::BrowserCloseOptions>) -> Result<()> {
    let opts: Option<ferridriver::options::BrowserCloseOptions> = options.map(Into::into);
    self.inner.close(opts).await.into_napi()
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
