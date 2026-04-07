#![allow(clippy::missing_errors_doc)]
//! Backend abstraction layer for browser automation.
//!
//! Provides a unified API across multiple browser backends:
//! - `CdpPipe`: Chrome `DevTools` Protocol over pipes (--remote-debugging-pipe, fd 3/4)
//! - `CdpRaw`: Chrome `DevTools` Protocol over WebSocket (our own, fully parallel)
//! - `WebKit`: Native `WKWebView` on macOS (subprocess model)
//!
//! Uses enum dispatch (not trait objects) for zero-cost abstraction and Clone support.

pub mod cdp;
pub(crate) mod json_scan;
#[cfg(target_os = "macos")]
pub mod webkit;

/// Empty JSON object `{}` — avoids `serde_json::json!({})` heap allocation per call.
#[inline]
pub(crate) fn empty_params() -> serde_json::Value {
  serde_json::Value::Object(serde_json::Map::new())
}

use crate::events::EventEmitter;
use crate::state::{ConsoleMsg, NetRequest};
use std::sync::Arc;
use tokio::sync::RwLock;

// ─── Backend-agnostic types ─────────────────────────────────────────────────

/// Frame metadata (backend-agnostic).
#[derive(Debug, Clone)]
pub struct FrameInfo {
  pub frame_id: String,
  pub parent_frame_id: Option<String>,
  pub name: String,
  pub url: String,
}

/// Accessibility tree node (backend-agnostic).
#[derive(Debug, Clone)]
pub struct AxNodeData {
  pub node_id: String,
  pub parent_id: Option<String>,
  pub backend_dom_node_id: Option<i64>,
  pub ignored: bool,
  pub role: Option<String>,
  pub name: Option<String>,
  pub description: Option<String>,
  pub properties: Vec<AxProperty>,
}

#[derive(Debug, Clone)]
pub struct AxProperty {
  pub name: String,
  pub value: Option<serde_json::Value>,
}

/// Cookie `SameSite` attribute (matches Playwright's `Strict | Lax | None`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SameSite {
  Strict,
  Lax,
  None,
}

impl SameSite {
  /// Convert to a CDP/WebKit string.
  #[must_use]
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Strict => "Strict",
      Self::Lax => "Lax",
      Self::None => "None",
    }
  }
}

impl std::str::FromStr for SameSite {
  type Err = ();

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "Strict" => Ok(Self::Strict),
      "Lax" => Ok(Self::Lax),
      "None" => Ok(Self::None),
      _ => Err(()),
    }
  }
}

/// Cookie data (backend-agnostic, matches Playwright's `NetworkCookie`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CookieData {
  pub name: String,
  pub value: String,
  pub domain: String,
  pub path: String,
  pub secure: bool,
  pub http_only: bool,
  pub expires: Option<f64>,
  /// `SameSite` attribute (`Strict`, `Lax`, or `None`).
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub same_site: Option<SameSite>,
}

/// Options for setting a cookie (matches Playwright's `SetNetworkCookieParam`).
/// Use `url` to derive domain/path automatically, or set `domain`/`path` directly.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SetCookieParams {
  pub name: String,
  pub value: String,
  /// URL to derive domain/path from. Mutually exclusive with domain/path.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub url: Option<String>,
  #[serde(default)]
  pub domain: String,
  #[serde(default)]
  pub path: String,
  #[serde(default)]
  pub secure: bool,
  #[serde(default)]
  pub http_only: bool,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub expires: Option<f64>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub same_site: Option<SameSite>,
}

impl From<SetCookieParams> for CookieData {
  fn from(p: SetCookieParams) -> Self {
    Self {
      name: p.name,
      value: p.value,
      domain: p.domain,
      path: if p.path.is_empty() { "/".to_string() } else { p.path },
      secure: p.secure,
      http_only: p.http_only,
      expires: p.expires,
      same_site: p.same_site,
    }
  }
}

/// Options for clearing cookies (matches Playwright's `ClearNetworkCookieOptions`).
/// All fields are optional filters -- only cookies matching ALL specified filters are cleared.
/// If no filters are specified, all cookies are cleared.
#[derive(Debug, Clone, Default)]
pub struct ClearCookieOptions {
  /// Filter by cookie name (exact match).
  pub name: Option<String>,
  /// Filter by domain (exact match).
  pub domain: Option<String>,
  /// Filter by path (exact match).
  pub path: Option<String>,
}

/// Screenshot options.
#[derive(Debug, Clone)]
pub struct ScreenshotOpts {
  pub format: ImageFormat,
  pub quality: Option<i64>,
  pub full_page: bool,
}

impl Default for ScreenshotOpts {
  fn default() -> Self {
    Self {
      format: ImageFormat::Png,
      quality: None,
      full_page: false,
    }
  }
}

/// Image format for screenshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
  Png,
  Jpeg,
  Webp,
}

/// Performance metric (backend-agnostic).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MetricData {
  pub name: String,
  pub value: f64,
}

/// Navigation lifecycle target — which CDP event to wait for after Page.navigate.
/// Matches Playwright's `waitUntil` semantics exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavLifecycle {
  /// `Page.frameNavigated` — response committed, new document started.
  Commit,
  /// `Page.lifecycleEvent` name="`DOMContentLoaded`" — HTML parsed, DOM ready.
  DomContentLoaded,
  /// `Page.lifecycleEvent` name="load" — all resources loaded.
  Load,
}

impl NavLifecycle {
  /// Parse from a `waitUntil` string (Playwright / MCP convention).
  /// Unknown values default to `Load`.
  #[must_use]
  pub fn parse_lifecycle(s: &str) -> Self {
    match s {
      "commit" => Self::Commit,
      "domcontentloaded" => Self::DomContentLoaded,
      _ => Self::Load,
    }
  }
}

/// Which backend to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
  /// Chrome `DevTools` Protocol over pipes (--remote-debugging-pipe)
  CdpPipe,
  /// Chrome `DevTools` Protocol over WebSocket (our own, fully parallel)
  CdpRaw,
  /// Native WebKit/WKWebView (macOS only)
  #[cfg(target_os = "macos")]
  WebKit,
}

// ─── AnyBrowser ─────────────────────────────────────────────────────────────

/// Browser instance — enum dispatch across backends.
pub enum AnyBrowser {
  CdpPipe(cdp::CdpBrowser<cdp::pipe::PipeTransport>),
  CdpRaw(cdp::CdpBrowser<cdp::ws::WsTransport>),
  #[cfg(target_os = "macos")]
  WebKit(webkit::WebKitBrowser),
}

impl AnyBrowser {
  /// List all open pages in this browser.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to enumerate targets or pages.
  pub async fn pages(&self) -> Result<Vec<AnyPage>, String> {
    match self {
      Self::CdpPipe(b) => b.pages().await,
      Self::CdpRaw(b) => b.pages().await,
      #[cfg(target_os = "macos")]
      Self::WebKit(b) => b.pages().await,
    }
  }

  /// Open a new page and navigate to the given URL.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to create a new target or navigate to the URL.
  pub async fn new_page(&self, url: &str) -> Result<AnyPage, String> {
    match self {
      Self::CdpPipe(b) => b.new_page(url).await,
      Self::CdpRaw(b) => b.new_page(url).await,
      #[cfg(target_os = "macos")]
      Self::WebKit(b) => b.new_page(url).await,
    }
  }

  /// Open a new page in an isolated browser context and navigate to the given URL.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to create an isolated context or navigate.
  pub async fn new_page_isolated(
    &self,
    url: &str,
    viewport: Option<&crate::options::ViewportConfig>,
  ) -> Result<AnyPage, String> {
    match self {
      Self::CdpPipe(b) => b.new_page_isolated(url, viewport).await,
      Self::CdpRaw(b) => b.new_page_isolated(url, viewport).await,
      #[cfg(target_os = "macos")]
      Self::WebKit(b) => b.new_page_isolated(url, viewport).await,
    }
  }

  /// Close the browser and all its pages.
  ///
  /// # Errors
  ///
  /// Returns an error if the browser process fails to shut down cleanly.
  pub async fn close(&mut self) -> Result<(), String> {
    match self {
      Self::CdpPipe(b) => b.close().await,
      Self::CdpRaw(b) => b.close().await,
      #[cfg(target_os = "macos")]
      Self::WebKit(b) => b.close().await,
    }
  }
}

// ─── AnyPage ────────────────────────────────────────────────────────────────

/// Page handle — enum dispatch across backends. Cheaply cloneable (Arc-based).
#[derive(Clone)]
pub enum AnyPage {
  CdpPipe(cdp::CdpPage<cdp::pipe::PipeTransport>),
  CdpRaw(cdp::CdpPage<cdp::ws::WsTransport>),
  #[cfg(target_os = "macos")]
  WebKit(webkit::WebKitPage),
}

/// Macro to dispatch a method call across all `AnyPage` variants.
macro_rules! page_dispatch {
    ($self:expr, $method:ident ( $($arg:expr),* $(,)? )) => {
        match $self {
            AnyPage::CdpPipe(p) => p.$method($($arg),*).await,
            AnyPage::CdpRaw(p) => p.$method($($arg),*).await,
            #[cfg(target_os = "macos")]
            AnyPage::WebKit(p) => p.$method($($arg),*).await,
        }
    };
}

impl AnyPage {
  // ── Events ──

  /// Get the event emitter for this page.
  #[must_use]
  pub fn events(&self) -> &EventEmitter {
    match self {
      AnyPage::CdpPipe(p) => &p.events,
      AnyPage::CdpRaw(p) => &p.events,
      #[cfg(target_os = "macos")]
      AnyPage::WebKit(p) => &p.events,
    }
  }

  // ── Frames ──

  pub async fn get_frame_tree(&self) -> Result<Vec<FrameInfo>, String> {
    page_dispatch!(self, get_frame_tree())
  }

  pub async fn evaluate_in_frame(&self, expression: &str, frame_id: &str) -> Result<Option<serde_json::Value>, String> {
    page_dispatch!(self, evaluate_in_frame(expression, frame_id))
  }

  // ── Navigation ──

  pub async fn goto(
    &self, url: &str, lifecycle: NavLifecycle, timeout_ms: u64,
  ) -> Result<(), String> {
    page_dispatch!(self, goto(url, lifecycle, timeout_ms))
  }

  pub async fn wait_for_navigation(&self) -> Result<(), String> {
    page_dispatch!(self, wait_for_navigation())
  }

  pub async fn reload(
    &self, lifecycle: NavLifecycle, timeout_ms: u64,
  ) -> Result<(), String> {
    page_dispatch!(self, reload(lifecycle, timeout_ms))
  }

  pub async fn go_back(
    &self, lifecycle: NavLifecycle, timeout_ms: u64,
  ) -> Result<(), String> {
    page_dispatch!(self, go_back(lifecycle, timeout_ms))
  }

  pub async fn go_forward(
    &self, lifecycle: NavLifecycle, timeout_ms: u64,
  ) -> Result<(), String> {
    page_dispatch!(self, go_forward(lifecycle, timeout_ms))
  }

  pub async fn url(&self) -> Result<Option<String>, String> {
    page_dispatch!(self, url())
  }

  pub async fn title(&self) -> Result<Option<String>, String> {
    page_dispatch!(self, title())
  }

  // ── JavaScript ──

  pub async fn evaluate(&self, expression: &str) -> Result<Option<serde_json::Value>, String> {
    page_dispatch!(self, evaluate(expression))
  }

  // ── Elements ──

  pub async fn find_element(&self, selector: &str) -> Result<AnyElement, String> {
    page_dispatch!(self, find_element(selector))
  }

  pub async fn evaluate_to_element(&self, js: &str) -> Result<AnyElement, String> {
    page_dispatch!(self, evaluate_to_element(js))
  }

  // ── Content ──

  pub async fn content(&self) -> Result<String, String> {
    page_dispatch!(self, content())
  }

  pub async fn set_content(&self, html: &str) -> Result<(), String> {
    page_dispatch!(self, set_content(html))
  }

  // ── Screenshots ──

  pub async fn screenshot(&self, opts: ScreenshotOpts) -> Result<Vec<u8>, String> {
    page_dispatch!(self, screenshot(opts))
  }

  // ── Accessibility ──

  pub async fn accessibility_tree(&self) -> Result<Vec<AxNodeData>, String> {
    page_dispatch!(self, accessibility_tree())
  }

  pub async fn accessibility_tree_with_depth(&self, depth: i32) -> Result<Vec<AxNodeData>, String> {
    page_dispatch!(self, accessibility_tree_with_depth(depth))
  }

  // ── Input ──

  pub async fn click_at(&self, x: f64, y: f64) -> Result<(), String> {
    page_dispatch!(self, click_at(x, y))
  }

  pub async fn click_at_opts(&self, x: f64, y: f64, button: &str, click_count: u32) -> Result<(), String> {
    page_dispatch!(self, click_at_opts(x, y, button, click_count))
  }

  pub async fn move_mouse(&self, x: f64, y: f64) -> Result<(), String> {
    page_dispatch!(self, move_mouse(x, y))
  }

  pub async fn move_mouse_smooth(
    &self,
    from_x: f64,
    from_y: f64,
    to_x: f64,
    to_y: f64,
    steps: u32,
  ) -> Result<(), String> {
    page_dispatch!(self, move_mouse_smooth(from_x, from_y, to_x, to_y, steps))
  }

  pub async fn mouse_wheel(&self, delta_x: f64, delta_y: f64) -> Result<(), String> {
    page_dispatch!(self, mouse_wheel(delta_x, delta_y))
  }

  pub async fn mouse_down(&self, x: f64, y: f64, button: &str) -> Result<(), String> {
    page_dispatch!(self, mouse_down(x, y, button))
  }

  pub async fn mouse_up(&self, x: f64, y: f64, button: &str) -> Result<(), String> {
    page_dispatch!(self, mouse_up(x, y, button))
  }

  pub async fn click_and_drag(&self, from: (f64, f64), to: (f64, f64)) -> Result<(), String> {
    page_dispatch!(self, click_and_drag(from, to))
  }

  pub async fn type_str(&self, text: &str) -> Result<(), String> {
    page_dispatch!(self, type_str(text))
  }

  pub async fn press_key(&self, key: &str) -> Result<(), String> {
    page_dispatch!(self, press_key(key))
  }

  // ── Cookies ──

  pub async fn get_cookies(&self) -> Result<Vec<CookieData>, String> {
    page_dispatch!(self, get_cookies())
  }

  pub async fn set_cookie(&self, cookie: CookieData) -> Result<(), String> {
    page_dispatch!(self, set_cookie(cookie))
  }

  pub async fn delete_cookie(&self, name: &str, domain: Option<&str>) -> Result<(), String> {
    page_dispatch!(self, delete_cookie(name, domain))
  }

  pub async fn clear_cookies(&self) -> Result<(), String> {
    page_dispatch!(self, clear_cookies())
  }

  /// Clear cookies matching the given filters. If no filters, clears all.
  pub async fn clear_cookies_filtered(&self, options: &ClearCookieOptions) -> Result<(), String> {
    if options.name.is_none() && options.domain.is_none() && options.path.is_none() {
      return self.clear_cookies().await;
    }
    // Get all cookies, delete the ones that match the filters.
    let cookies = self.get_cookies().await?;
    for c in &cookies {
      let name_match = options.name.as_ref().is_none_or(|n| &c.name == n);
      let domain_match = options.domain.as_ref().is_none_or(|d| &c.domain == d);
      let path_match = options.path.as_ref().is_none_or(|p| &c.path == p);
      if name_match && domain_match && path_match {
        self.delete_cookie(&c.name, Some(&c.domain)).await?;
      }
    }
    Ok(())
  }

  // ── Emulation ──

  pub async fn emulate_viewport(&self, config: &crate::options::ViewportConfig) -> Result<(), String> {
    page_dispatch!(self, emulate_viewport(config))
  }

  pub async fn set_user_agent(&self, ua: &str) -> Result<(), String> {
    page_dispatch!(self, set_user_agent(ua))
  }

  pub async fn set_geolocation(&self, lat: f64, lng: f64, accuracy: f64) -> Result<(), String> {
    page_dispatch!(self, set_geolocation(lat, lng, accuracy))
  }

  pub async fn set_locale(&self, locale: &str) -> Result<(), String> {
    page_dispatch!(self, set_locale(locale))
  }

  pub async fn set_timezone(&self, timezone_id: &str) -> Result<(), String> {
    page_dispatch!(self, set_timezone(timezone_id))
  }

  pub async fn emulate_media(&self, opts: &crate::options::EmulateMediaOptions) -> Result<(), String> {
    page_dispatch!(self, emulate_media(opts))
  }

  pub async fn set_javascript_enabled(&self, enabled: bool) -> Result<(), String> {
    page_dispatch!(self, set_javascript_enabled(enabled))
  }

  pub async fn set_extra_http_headers(&self, headers: &rustc_hash::FxHashMap<String, String>) -> Result<(), String> {
    page_dispatch!(self, set_extra_http_headers(headers))
  }

  pub async fn grant_permissions(&self, permissions: &[String], origin: Option<&str>) -> Result<(), String> {
    page_dispatch!(self, grant_permissions(permissions, origin))
  }

  pub async fn reset_permissions(&self) -> Result<(), String> {
    page_dispatch!(self, reset_permissions())
  }

  pub async fn set_focus_emulation_enabled(&self, enabled: bool) -> Result<(), String> {
    page_dispatch!(self, set_focus_emulation_enabled(enabled))
  }

  // ── Network ──

  pub async fn set_network_state(&self, offline: bool, latency: f64, download: f64, upload: f64) -> Result<(), String> {
    page_dispatch!(self, set_network_state(offline, latency, download, upload))
  }

  // ── Tracing ──

  pub async fn start_tracing(&self) -> Result<(), String> {
    page_dispatch!(self, start_tracing())
  }

  pub async fn stop_tracing(&self) -> Result<(), String> {
    page_dispatch!(self, stop_tracing())
  }

  pub async fn metrics(&self) -> Result<Vec<MetricData>, String> {
    page_dispatch!(self, metrics())
  }

  // ── Ref resolution ──

  pub async fn resolve_backend_node(&self, backend_node_id: i64, ref_id: &str) -> Result<AnyElement, String> {
    page_dispatch!(self, resolve_backend_node(backend_node_id, ref_id))
  }

  // ── Event listeners ──

  pub fn attach_listeners(
    &self,
    console_log: Arc<RwLock<Vec<ConsoleMsg>>>,
    network_log: Arc<RwLock<Vec<NetRequest>>>,
    dialog_log: Arc<RwLock<Vec<crate::state::DialogEvent>>>,
  ) {
    match self {
      Self::CdpPipe(p) => p.attach_listeners(console_log, network_log, dialog_log),
      Self::CdpRaw(p) => p.attach_listeners(console_log, network_log, dialog_log),
      #[cfg(target_os = "macos")]
      Self::WebKit(p) => p.attach_listeners(console_log, network_log, dialog_log),
    }
  }

  // ── Element screenshot (by selector) ──

  pub async fn screenshot_element(&self, selector: &str, format: ImageFormat) -> Result<Vec<u8>, String> {
    page_dispatch!(self, screenshot_element(selector, format))
  }

  // ── PDF generation ──

  pub async fn pdf(&self, landscape: bool, print_background: bool) -> Result<Vec<u8>, String> {
    page_dispatch!(self, pdf(landscape, print_background))
  }

  // ── Screencast (video recording) ──

  pub async fn start_screencast(
    &self,
    quality: u8,
    max_width: u32,
    max_height: u32,
  ) -> Result<tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>, String> {
    match self {
      AnyPage::CdpPipe(p) => p.start_screencast(quality, max_width, max_height).await,
      AnyPage::CdpRaw(p) => p.start_screencast(quality, max_width, max_height).await,
      #[cfg(target_os = "macos")]
      AnyPage::WebKit(_) => Err("Video recording is not supported on WebKit backend".into()),
    }
  }

  pub async fn stop_screencast(&self) -> Result<(), String> {
    match self {
      AnyPage::CdpPipe(p) => p.stop_screencast().await,
      AnyPage::CdpRaw(p) => p.stop_screencast().await,
      #[cfg(target_os = "macos")]
      AnyPage::WebKit(_) => Ok(()), // No-op if never started.
    }
  }

  // ── File upload ──

  pub async fn set_file_input(&self, selector: &str, paths: &[String]) -> Result<(), String> {
    page_dispatch!(self, set_file_input(selector, paths))
  }

  // ── Dialog handling ──

  pub async fn set_dialog_handler(&self, handler: crate::events::DialogHandler) {
    match self {
      Self::CdpPipe(p) => *p.dialog_handler.write().await = handler,
      Self::CdpRaw(p) => *p.dialog_handler.write().await = handler,
      #[cfg(target_os = "macos")]
      Self::WebKit(_) => {
        // WebKit dialog handling is in the ObjC subprocess via WKUIDelegate.
        // Custom handlers would need a new IPC op. For now, auto-behavior only.
      },
    }
  }

  // ── Network Interception ──

  pub async fn route(&self, pattern: &str, handler: crate::route::RouteHandler) -> Result<(), String> {
    page_dispatch!(self, route(pattern, handler))
  }

  pub async fn unroute(&self, pattern: &str) -> Result<(), String> {
    page_dispatch!(self, unroute(pattern))
  }

  // ── Lifecycle ──

  pub async fn close_page(&self) -> Result<(), String> {
    page_dispatch!(self, close_page())
  }

  #[must_use]
  pub fn is_closed(&self) -> bool {
    match self {
      Self::CdpPipe(p) => p.is_closed(),
      Self::CdpRaw(p) => p.is_closed(),
      #[cfg(target_os = "macos")]
      Self::WebKit(p) => p.is_closed(),
    }
  }

  // ── Exposed Functions ──

  pub async fn expose_function(&self, name: &str, func: crate::events::ExposedFn) -> Result<(), String> {
    page_dispatch!(self, expose_function(name, func))
  }

  pub async fn remove_exposed_function(&self, name: &str) -> Result<(), String> {
    page_dispatch!(self, remove_exposed_function(name))
  }

  // ── Init Scripts ──

  pub async fn add_init_script(&self, source: &str) -> Result<String, String> {
    page_dispatch!(self, add_init_script(source))
  }

  pub async fn remove_init_script(&self, identifier: &str) -> Result<(), String> {
    page_dispatch!(self, remove_init_script(identifier))
  }
}

// ─── AnyElement ─────────────────────────────────────────────────────────────

/// Element handle — enum dispatch across backends.
pub enum AnyElement {
  CdpPipe(cdp::CdpElement<cdp::pipe::PipeTransport>),
  CdpRaw(cdp::CdpElement<cdp::ws::WsTransport>),
  #[cfg(target_os = "macos")]
  WebKit(webkit::WebKitElement),
}

macro_rules! element_dispatch {
    ($self:expr, $method:ident ( $($arg:expr),* $(,)? )) => {
        match $self {
            AnyElement::CdpPipe(e) => e.$method($($arg),*).await,
            AnyElement::CdpRaw(e) => e.$method($($arg),*).await,
            #[cfg(target_os = "macos")]
            AnyElement::WebKit(e) => e.$method($($arg),*).await,
        }
    };
}

impl AnyElement {
  pub async fn click(&self) -> Result<(), String> {
    element_dispatch!(self, click())
  }

  pub async fn dblclick(&self) -> Result<(), String> {
    element_dispatch!(self, dblclick())
  }

  pub async fn hover(&self) -> Result<(), String> {
    element_dispatch!(self, hover())
  }

  pub async fn type_str(&self, text: &str) -> Result<(), String> {
    element_dispatch!(self, type_str(text))
  }

  pub async fn call_js_fn(&self, function: &str) -> Result<(), String> {
    element_dispatch!(self, call_js_fn(function))
  }

  pub async fn call_js_fn_value(&self, function: &str) -> Result<Option<serde_json::Value>, String> {
    element_dispatch!(self, call_js_fn_value(function))
  }

  pub async fn scroll_into_view(&self) -> Result<(), String> {
    element_dispatch!(self, scroll_into_view())
  }

  pub async fn screenshot(&self, format: ImageFormat) -> Result<Vec<u8>, String> {
    element_dispatch!(self, screenshot(format))
  }
}
