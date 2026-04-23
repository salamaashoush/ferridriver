//! `BrowserContext` class -- NAPI binding for `ferridriver::ContextRef`.

use crate::error::IntoNapi;
use crate::page::Page;
use crate::types::CookieData;
use napi::Result;
use napi_derive::napi;
use std::collections::HashMap;

/// Isolated browser context with its own cookies, storage, and permissions.
/// Mirrors Playwright's `BrowserContext`.
#[napi]
pub struct BrowserContext {
  inner: ferridriver::ContextRef,
}

impl BrowserContext {
  pub(crate) fn wrap(inner: ferridriver::ContextRef) -> Self {
    Self { inner }
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
    let ctx = state.context(self.inner.name()).map_err(napi::Error::from_reason)?;
    ctx.delete_cookie(&name, domain.as_deref()).await.into_napi()
  }

  // ── Timeouts ──

  #[napi]
  pub fn set_default_timeout(&mut self, ms: f64) {
    self.inner.set_default_timeout(crate::types::f64_to_u64(ms));
  }

  #[napi]
  pub fn set_default_navigation_timeout(&mut self, ms: f64) {
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

  // ── Context-level init scripts ──

  /// Register a JS snippet to run on every new document on every page in
  /// this context. Mirrors Playwright's
  /// `browserContext.addInitScript(script, arg)` — see
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:356`.
  /// See [`crate::page::Page::add_init_script`] for argument semantics.
  #[napi(ts_args_type = "script: Function | string | { path?: string, content?: string }, arg?: any")]
  pub async fn add_init_script(
    &self,
    script: crate::types::NapiInitScript,
    arg: crate::types::NapiInitScriptArg,
  ) -> Result<Vec<String>> {
    self.inner.add_init_script(script.into(), arg.0).await.into_napi()
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

  // ── Lifecycle ──

  #[napi]
  pub async fn close(&self) -> Result<()> {
    self.inner.close().await.into_napi()
  }
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
    let http_credentials = self.http_credentials.map(|c| fo::HttpCredentials {
      username: c.username,
      password: c.password,
      origin: c.origin,
      send: c.send.and_then(|s| match s.as_str() {
        "always" => Some(fo::HttpCredentialsSend::Always),
        "unauthorized" => Some(fo::HttpCredentialsSend::Unauthorized),
        _ => None,
      }),
    });
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
