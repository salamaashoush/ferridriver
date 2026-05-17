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
}
