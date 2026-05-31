//! `BrowserType` -- NAPI binding for `ferridriver::BrowserType`.
//!
//! Mirrors Playwright's `BrowserType` interface
//! (`/tmp/playwright/packages/playwright-core/types/types.d.ts:15046`)
//! and the three top-level `chromium` / `firefox` / `webkit`
//! singletons exposed by `import { chromium } from 'playwright'`.

// `#[napi]` exports these to JS but clippy's reachability check only
// follows Rust call graphs, so it flags the entry points as dead.
// Disable the lint at the module level — every public item here is
// part of the NAPI surface, not internal.
#![allow(dead_code)]

use crate::browser::Browser;
use crate::error::IntoNapi;
use crate::types::{BrowserTypeOptions, ConnectOptions, ConnectOverCdpOptions, LaunchOptions};
use ferridriver::options as core_opts;
use napi::Result;
use napi_derive::napi;

/// Playwright `BrowserType`. Construct via top-level [`chromium`],
/// [`firefox`], or [`webkit`].
#[napi]
pub struct BrowserType {
  inner: ferridriver::BrowserType,
}

impl BrowserType {
  pub(crate) fn wrap(inner: ferridriver::BrowserType) -> Self {
    Self { inner }
  }
}

#[napi]
impl BrowserType {
  /// Playwright `BrowserType.name()` — `"chromium"` / `"firefox"` / `"webkit"`.
  #[napi]
  pub fn name(&self) -> String {
    self.inner.name().to_string()
  }

  /// Playwright `BrowserType.executablePath()` — path to the bundled
  /// browser binary, or `null` if no bundled binary is available on
  /// this platform.
  #[napi]
  pub fn executable_path(&self) -> Option<String> {
    self.inner.executable_path().map(|p| p.to_string_lossy().into_owned())
  }

  /// Playwright `browserType.launch(options?) -> Promise<Browser>`.
  #[napi]
  pub async fn launch(&self, options: Option<LaunchOptions>) -> Result<Browser> {
    let core = lower_launch_options(options.unwrap_or_default());
    let inner = Box::pin(self.inner.launch(core)).await.into_napi()?;
    Ok(Browser::wrap(inner))
  }

  /// Playwright `browserType.connect(wsEndpoint, options?) -> Promise<Browser>`.
  #[napi]
  pub async fn connect(&self, ws_endpoint: String, options: Option<ConnectOptions>) -> Result<Browser> {
    let core = lower_connect_options(options.unwrap_or_default());
    let inner = Box::pin(self.inner.connect(&ws_endpoint, core)).await.into_napi()?;
    Ok(Browser::wrap(inner))
  }

  /// Playwright `browserType.connectOverCDP(endpointURL, options?) -> Promise<Browser>`.
  /// Chromium-only.
  #[napi]
  pub async fn connect_over_cdp(
    &self,
    endpoint_url: String,
    options: Option<ConnectOverCdpOptions>,
  ) -> Result<Browser> {
    let core = lower_connect_over_cdp_options(options.unwrap_or_default());
    let inner = Box::pin(self.inner.connect_over_cdp(&endpoint_url, core))
      .await
      .into_napi()?;
    Ok(Browser::wrap(inner))
  }

  /// Playwright
  /// `browserType.launchPersistentContext(userDataDir, options?) -> Promise<BrowserContext>`.
  /// Returns the persistent default context. Closing it (or the
  /// underlying browser) terminates the launch.
  ///
  /// `options` accepts the union of [`LaunchOptions`] and the
  /// [`crate::context::NapiBrowserContextOptions`] context-options
  /// bag. The `ts_args_type` below forces the generated `.d.ts` to
  /// carry Playwright's exact merged shape.
  #[napi(ts_args_type = "userDataDir: string, options?: {
    headless?: boolean;
    executablePath?: string;
    args?: string[];
    channel?: string;
    slowMo?: number;
    timeout?: number;
    downloadsPath?: string;
    tracesDir?: string;
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
    disableViewport?: boolean;
  }")]
  pub async fn launch_persistent_context(
    &self,
    user_data_dir: String,
    options: Option<PersistentContextOptions>,
  ) -> Result<crate::context::BrowserContext> {
    let opts = options.unwrap_or_default();
    let launch = lower_launch_options(opts.launch);
    let context = opts
      .context
      .map(crate::context::NapiBrowserContextOptions::into_core)
      .unwrap_or_default();
    let core = core_opts::LaunchPersistentContextOptions { launch, context };
    let ctx = Box::pin(
      self
        .inner
        .launch_persistent_context(std::path::Path::new(&user_data_dir), core),
    )
    .await
    .into_napi()?;
    Ok(crate::context::BrowserContext::wrap(ctx))
  }
}

/// Internal helper: split the merged persistent-context options bag
/// into `LaunchOptions` + context options. `napi(object)` cannot
/// nest `Option<NapiBrowserContextOptions>` field inside a flat
/// signature without losing the merged TS shape, so we rebuild the
/// internal split from the `ts_args_type` declaration above by
/// having napi-rs hand the JS object to a single intermediate struct.
#[derive(Default)]
pub struct PersistentContextOptions {
  pub launch: LaunchOptions,
  pub context: Option<crate::context::NapiBrowserContextOptions>,
}

impl napi::bindgen_prelude::FromNapiValue for PersistentContextOptions {
  unsafe fn from_napi_value(env: napi::sys::napi_env, value: napi::sys::napi_value) -> napi::Result<Self> {
    Ok(Self {
      launch: unsafe { LaunchOptions::from_napi_value(env, value)? },
      context: Some(unsafe { crate::context::NapiBrowserContextOptions::from_napi_value(env, value)? }),
    })
  }
}

fn lower_launch_options(opts: LaunchOptions) -> core_opts::LaunchOptions {
  core_opts::LaunchOptions {
    headless: opts.headless,
    executable_path: opts.executable_path,
    args: opts.args.unwrap_or_default(),
    channel: opts.channel,
    env: None,
    slow_mo: opts.slow_mo.map(u64::from),
    timeout: opts.timeout.map(u64::from),
    downloads_path: opts.downloads_path.map(std::path::PathBuf::from),
    ignore_default_args: None,
    handle_sighup: None,
    handle_sigint: None,
    handle_sigterm: None,
    chromium_sandbox: None,
    firefox_user_prefs: None,
    proxy: None,
    traces_dir: opts.traces_dir.map(std::path::PathBuf::from),
  }
}

fn lower_connect_options(opts: ConnectOptions) -> core_opts::ConnectOptions {
  core_opts::ConnectOptions {
    headers: opts.headers.map(|h| h.into_iter().collect()),
    slow_mo: opts.slow_mo.map(u64::from),
    timeout: opts.timeout.map(u64::from),
    expose_network: opts.expose_network,
  }
}

fn lower_connect_over_cdp_options(opts: ConnectOverCdpOptions) -> core_opts::ConnectOverCdpOptions {
  core_opts::ConnectOverCdpOptions {
    headers: opts.headers.map(|h| h.into_iter().collect()),
    slow_mo: opts.slow_mo.map(u64::from),
    timeout: opts.timeout.map(u64::from),
  }
}

/// Top-level Playwright `chromium` accessor.
///
/// `chromium()` returns a Chromium `BrowserType` configured with the
/// CDP-pipe transport. Pass `{ transport: 'ws' }` to drive CDP over
/// WebSocket (CdpRaw backend) — a ferridriver extension over
/// Playwright's pipe-only `chromium`.
#[napi(ts_args_type = "options?: { transport?: 'pipe' | 'ws' }")]
pub fn chromium(options: Option<BrowserTypeOptions>) -> BrowserType {
  let transport = options.and_then(|o| o.transport).and_then(|t| match t.as_str() {
    "ws" => Some(core_opts::ChromiumTransport::Ws),
    "pipe" => Some(core_opts::ChromiumTransport::Pipe),
    _ => None,
  });
  BrowserType::wrap(ferridriver::BrowserType::chromium_with(
    &core_opts::BrowserTypeOptions { transport },
  ))
}

/// Top-level Playwright `firefox` accessor.
#[napi]
pub fn firefox() -> BrowserType {
  BrowserType::wrap(ferridriver::BrowserType::firefox())
}

/// Top-level Playwright `webkit` accessor. Drives Playwright's
/// cross-platform WebKit build via `pw_run.sh`, so `launch()` works on
/// Unix (Linux + macOS); on unsupported platforms (Windows) it returns
/// a typed error.
#[napi]
pub fn webkit() -> BrowserType {
  BrowserType::wrap(ferridriver::BrowserType::webkit())
}
