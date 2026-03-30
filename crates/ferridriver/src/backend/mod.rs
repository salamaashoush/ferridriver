//! Backend abstraction layer for browser automation.
//!
//! Provides a unified API across multiple browser backends:
//! - `CdpRaw`: Chrome `DevTools` Protocol over WebSocket (our own, fully parallel)
//! - `CdpPipe`: Chrome `DevTools` Protocol over pipes (--remote-debugging-pipe, fd 3/4)
//! - `WebKit`: Native `WKWebView` on macOS (subprocess model)
//!
//! Uses enum dispatch (not trait objects) for zero-cost abstraction and Clone support.

pub mod cdp_pipe;
pub mod cdp_raw;
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

/// Cookie data (backend-agnostic).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CookieData {
  pub name: String,
  pub value: String,
  pub domain: String,
  pub path: String,
  pub secure: bool,
  pub http_only: bool,
  pub expires: Option<f64>,
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
  #[must_use]
  pub fn from_str(s: &str) -> Self {
    match s {
      "commit" => Self::Commit,
      "domcontentloaded" => Self::DomContentLoaded,
      "load" => Self::Load,
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
  CdpPipe(cdp_pipe::CdpPipeBrowser),
  CdpRaw(cdp_raw::CdpRawBrowser),
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
      Self::WebKit(b) => b.new_page_isolated(url).await,
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
  CdpPipe(cdp_pipe::CdpPipePage),
  CdpRaw(cdp_raw::CdpRawPage),
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

  /// Get the frame tree for this page.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to retrieve frame tree information.
  pub async fn get_frame_tree(&self) -> Result<Vec<FrameInfo>, String> {
    page_dispatch!(self, get_frame_tree())
  }

  /// Evaluate JS in a specific frame.
  ///
  /// # Errors
  ///
  /// Returns an error if the frame ID is invalid or JS evaluation fails.
  pub async fn evaluate_in_frame(&self, expression: &str, frame_id: &str) -> Result<Option<serde_json::Value>, String> {
    page_dispatch!(self, evaluate_in_frame(expression, frame_id))
  }

  // ── Navigation ──

  /// Navigate the page to the given URL.
  ///
  /// # Errors
  ///
  /// Returns an error if navigation fails (e.g., invalid URL, network error, or timeout).
  pub async fn goto(
    &self, url: &str, lifecycle: NavLifecycle, timeout_ms: u64,
  ) -> Result<(), String> {
    page_dispatch!(self, goto(url, lifecycle, timeout_ms))
  }

  /// Wait until the current navigation completes.
  ///
  /// # Errors
  ///
  /// Returns an error if the navigation times out or the page encounters a load error.
  pub async fn wait_for_navigation(&self) -> Result<(), String> {
    page_dispatch!(self, wait_for_navigation())
  }

  /// Reload the current page.
  ///
  /// # Errors
  ///
  /// Returns an error if the reload fails or the backend connection is lost.
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

  /// Get the current page URL.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to retrieve the URL.
  pub async fn url(&self) -> Result<Option<String>, String> {
    page_dispatch!(self, url())
  }

  /// Get the current page title.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to retrieve the title.
  pub async fn title(&self) -> Result<Option<String>, String> {
    page_dispatch!(self, title())
  }

  // ── JavaScript ──

  /// Evaluate a JavaScript expression in the page context.
  ///
  /// # Errors
  ///
  /// Returns an error if JS evaluation throws an exception or the backend fails.
  pub async fn evaluate(&self, expression: &str) -> Result<Option<serde_json::Value>, String> {
    page_dispatch!(self, evaluate(expression))
  }

  // ── Elements ──

  /// Find a DOM element by CSS selector.
  ///
  /// # Errors
  ///
  /// Returns an error if no element matches the selector or the backend fails.
  pub async fn find_element(&self, selector: &str) -> Result<AnyElement, String> {
    page_dispatch!(self, find_element(selector))
  }

  /// Evaluate JS that returns a DOM element and wrap it as `AnyElement`.
  /// The JS must return a single DOM element (not a value).
  ///
  /// # Errors
  ///
  /// Returns an error if the JS does not return a DOM element or evaluation fails.
  pub async fn evaluate_to_element(&self, js: &str) -> Result<AnyElement, String> {
    page_dispatch!(self, evaluate_to_element(js))
  }

  // ── Content ──

  /// Get the full HTML content of the page.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to retrieve page content.
  pub async fn content(&self) -> Result<String, String> {
    page_dispatch!(self, content())
  }

  /// Replace the entire page content with the given HTML.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to set the page content.
  pub async fn set_content(&self, html: &str) -> Result<(), String> {
    page_dispatch!(self, set_content(html))
  }

  // ── Screenshots ──

  /// Capture a screenshot of the page.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to capture or encode the screenshot.
  pub async fn screenshot(&self, opts: ScreenshotOpts) -> Result<Vec<u8>, String> {
    page_dispatch!(self, screenshot(opts))
  }

  // ── Accessibility ──

  /// Get the full accessibility tree for this page.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to retrieve the accessibility tree.
  pub async fn accessibility_tree(&self) -> Result<Vec<AxNodeData>, String> {
    page_dispatch!(self, accessibility_tree())
  }

  /// Get the accessibility tree with a depth limit.
  /// depth=-1 means unlimited, depth=0 means root only, etc.
  /// Uses native CDP depth parameter on Chrome backends, native `NSAccessibility`
  /// depth limiting on `WebKit`.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to retrieve the accessibility tree.
  pub async fn accessibility_tree_with_depth(&self, depth: i32) -> Result<Vec<AxNodeData>, String> {
    page_dispatch!(self, accessibility_tree_with_depth(depth))
  }

  // ── Input ──

  /// Simulate a mouse click at the given page coordinates.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to dispatch the click event.
  pub async fn click_at(&self, x: f64, y: f64) -> Result<(), String> {
    page_dispatch!(self, click_at(x, y))
  }

  /// Click at coordinates with specific button and click count.
  /// button: "left", "right", "middle", "back", "forward"
  /// `click_count`: 1 for single, 2 for double, 3 for triple
  ///
  /// # Errors
  ///
  /// Returns an error if the button name is invalid or the backend fails to dispatch the event.
  pub async fn click_at_opts(&self, x: f64, y: f64, button: &str, click_count: u32) -> Result<(), String> {
    page_dispatch!(self, click_at_opts(x, y, button, click_count))
  }

  /// Move mouse to coordinates without clicking.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to dispatch the mouse move event.
  pub async fn move_mouse(&self, x: f64, y: f64) -> Result<(), String> {
    page_dispatch!(self, move_mouse(x, y))
  }

  /// Move mouse smoothly with bezier easing over N steps.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to dispatch any of the intermediate mouse events.
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

  /// Mouse wheel scroll at current position.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to dispatch the wheel event.
  pub async fn mouse_wheel(&self, delta_x: f64, delta_y: f64) -> Result<(), String> {
    page_dispatch!(self, mouse_wheel(delta_x, delta_y))
  }

  /// Mouse button down (without up). For custom drag sequences.
  ///
  /// # Errors
  ///
  /// Returns an error if the button name is invalid or the backend fails to dispatch the event.
  pub async fn mouse_down(&self, x: f64, y: f64, button: &str) -> Result<(), String> {
    page_dispatch!(self, mouse_down(x, y, button))
  }

  /// Mouse button up.
  ///
  /// # Errors
  ///
  /// Returns an error if the button name is invalid or the backend fails to dispatch the event.
  pub async fn mouse_up(&self, x: f64, y: f64, button: &str) -> Result<(), String> {
    page_dispatch!(self, mouse_up(x, y, button))
  }

  /// Click and drag from one point to another.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to dispatch the mouse down, move, or up events.
  pub async fn click_and_drag(&self, from: (f64, f64), to: (f64, f64)) -> Result<(), String> {
    page_dispatch!(self, click_and_drag(from, to))
  }

  /// Type text by dispatching key events for each character.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to dispatch any key event.
  pub async fn type_str(&self, text: &str) -> Result<(), String> {
    page_dispatch!(self, type_str(text))
  }

  /// Press a keyboard key (e.g., "Enter", "Tab", "`ArrowDown`").
  ///
  /// # Errors
  ///
  /// Returns an error if the key name is unrecognized or the backend fails.
  pub async fn press_key(&self, key: &str) -> Result<(), String> {
    page_dispatch!(self, press_key(key))
  }

  // ── Cookies ──

  /// Get all cookies for this page.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to retrieve cookies.
  pub async fn get_cookies(&self) -> Result<Vec<CookieData>, String> {
    page_dispatch!(self, get_cookies())
  }

  /// Set a cookie on this page.
  ///
  /// # Errors
  ///
  /// Returns an error if the cookie data is invalid or the backend fails to set it.
  pub async fn set_cookie(&self, cookie: CookieData) -> Result<(), String> {
    page_dispatch!(self, set_cookie(cookie))
  }

  /// Delete a cookie by name and optional domain.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to delete the cookie.
  pub async fn delete_cookie(&self, name: &str, domain: Option<&str>) -> Result<(), String> {
    page_dispatch!(self, delete_cookie(name, domain))
  }

  /// Clear all cookies for this page.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to clear cookies.
  pub async fn clear_cookies(&self) -> Result<(), String> {
    page_dispatch!(self, clear_cookies())
  }

  // ── Emulation ──

  /// Set the viewport size and device scale factor.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to apply the viewport configuration.
  pub async fn emulate_viewport(&self, config: &crate::options::ViewportConfig) -> Result<(), String> {
    page_dispatch!(self, emulate_viewport(config))
  }

  /// Override the browser user-agent string.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to set the user agent.
  pub async fn set_user_agent(&self, ua: &str) -> Result<(), String> {
    page_dispatch!(self, set_user_agent(ua))
  }

  /// Override the geolocation reported to the page.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to apply the geolocation override.
  pub async fn set_geolocation(&self, lat: f64, lng: f64, accuracy: f64) -> Result<(), String> {
    page_dispatch!(self, set_geolocation(lat, lng, accuracy))
  }

  /// Override the browser locale (e.g., "en-US").
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to apply the locale override.
  pub async fn set_locale(&self, locale: &str) -> Result<(), String> {
    page_dispatch!(self, set_locale(locale))
  }

  /// Override the browser timezone (e.g., "America/`New_York`").
  ///
  /// # Errors
  ///
  /// Returns an error if the timezone ID is invalid or the backend fails.
  pub async fn set_timezone(&self, timezone_id: &str) -> Result<(), String> {
    page_dispatch!(self, set_timezone(timezone_id))
  }

  /// Emulate media features (e.g., color scheme, reduced motion).
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to apply the media emulation.
  pub async fn emulate_media(&self, opts: &crate::options::EmulateMediaOptions) -> Result<(), String> {
    page_dispatch!(self, emulate_media(opts))
  }

  /// Enable or disable JavaScript execution on this page.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to toggle the JS execution state.
  pub async fn set_javascript_enabled(&self, enabled: bool) -> Result<(), String> {
    page_dispatch!(self, set_javascript_enabled(enabled))
  }

  /// Set extra HTTP headers sent with every request from this page.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to apply the headers.
  pub async fn set_extra_http_headers(&self, headers: &rustc_hash::FxHashMap<String, String>) -> Result<(), String> {
    page_dispatch!(self, set_extra_http_headers(headers))
  }

  /// Grant browser permissions (e.g., "geolocation", "notifications") for an origin.
  ///
  /// # Errors
  ///
  /// Returns an error if a permission name is invalid or the backend fails.
  pub async fn grant_permissions(&self, permissions: &[String], origin: Option<&str>) -> Result<(), String> {
    page_dispatch!(self, grant_permissions(permissions, origin))
  }

  /// Reset all granted permissions to defaults.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to reset permissions.
  pub async fn reset_permissions(&self) -> Result<(), String> {
    page_dispatch!(self, reset_permissions())
  }

  /// Enable or disable focus emulation (page always appears focused).
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to toggle focus emulation.
  pub async fn set_focus_emulation_enabled(&self, enabled: bool) -> Result<(), String> {
    page_dispatch!(self, set_focus_emulation_enabled(enabled))
  }

  // ── Network ──

  /// Configure network throttling (offline mode, latency, download/upload throughput).
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to apply network conditions.
  pub async fn set_network_state(&self, offline: bool, latency: f64, download: f64, upload: f64) -> Result<(), String> {
    page_dispatch!(self, set_network_state(offline, latency, download, upload))
  }

  // ── Tracing ──

  /// Start performance tracing on this page.
  ///
  /// # Errors
  ///
  /// Returns an error if tracing is already active or the backend fails to start it.
  pub async fn start_tracing(&self) -> Result<(), String> {
    page_dispatch!(self, start_tracing())
  }

  /// Stop performance tracing on this page.
  ///
  /// # Errors
  ///
  /// Returns an error if tracing was not started or the backend fails to stop it.
  pub async fn stop_tracing(&self) -> Result<(), String> {
    page_dispatch!(self, stop_tracing())
  }

  /// Retrieve performance metrics from the page.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to collect metrics.
  pub async fn metrics(&self) -> Result<Vec<MetricData>, String> {
    page_dispatch!(self, metrics())
  }

  // ── Ref resolution ──

  /// Resolve a backend node ID (from a11y snapshot) to an element.
  /// `ref_id` is the ref label (e.g., "e5") used to tag the element for later CSS lookup.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend node ID is invalid or the element cannot be resolved.
  pub async fn resolve_backend_node(&self, backend_node_id: i64, ref_id: &str) -> Result<AnyElement, String> {
    page_dispatch!(self, resolve_backend_node(backend_node_id, ref_id))
  }

  // ── Event listeners ──

  /// Attach console, network, and dialog event listeners that push into the provided logs.
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

  /// Capture a screenshot of a specific element found by CSS selector.
  ///
  /// # Errors
  ///
  /// Returns an error if no element matches the selector or screenshot capture fails.
  pub async fn screenshot_element(&self, selector: &str, format: ImageFormat) -> Result<Vec<u8>, String> {
    page_dispatch!(self, screenshot_element(selector, format))
  }

  // ── PDF generation ──

  /// Generate a PDF of the page.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend does not support PDF generation or the operation fails.
  pub async fn pdf(&self, landscape: bool, print_background: bool) -> Result<Vec<u8>, String> {
    page_dispatch!(self, pdf(landscape, print_background))
  }

  // ── File upload ──

  /// Set files on a file input element found by CSS selector.
  /// Uses CDP DOM.setFileInputFiles with backendNodeId.
  ///
  /// # Errors
  ///
  /// Returns an error if the selector does not match a file input or the file paths are invalid.
  pub async fn set_file_input(&self, selector: &str, paths: &[String]) -> Result<(), String> {
    page_dispatch!(self, set_file_input(selector, paths))
  }

  // ── Init Scripts ──

  // ── Dialog handling ──

  /// Set a custom dialog handler. Called synchronously when a JS dialog appears.
  /// Default: auto-accept alerts/confirms, accept prompts with default value.
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

  /// Register a route handler for URLs matching the glob pattern.
  ///
  /// # Errors
  ///
  /// Returns an error if the pattern is invalid or the backend fails to enable interception.
  pub async fn route(&self, pattern: &str, handler: crate::route::RouteHandler) -> Result<(), String> {
    page_dispatch!(self, route(pattern, handler))
  }

  /// Remove all route handlers matching the glob pattern.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to remove the route or disable interception.
  pub async fn unroute(&self, pattern: &str) -> Result<(), String> {
    page_dispatch!(self, unroute(pattern))
  }

  // ── Lifecycle ──

  /// Close this page/target.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to close the page or target.
  pub async fn close_page(&self) -> Result<(), String> {
    page_dispatch!(self, close_page())
  }

  /// Check if this page has been closed.
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

  /// Expose a Rust function to the page as `window.<name>(...)`.
  /// The function receives JSON arguments and returns a JSON value.
  /// Persists across navigations.
  ///
  /// # Errors
  ///
  /// Returns an error if the function name conflicts or the backend fails to inject the binding.
  pub async fn expose_function(&self, name: &str, func: crate::events::ExposedFn) -> Result<(), String> {
    page_dispatch!(self, expose_function(name, func))
  }

  /// Remove a previously exposed function.
  ///
  /// # Errors
  ///
  /// Returns an error if the function was not previously exposed or the backend fails.
  pub async fn remove_exposed_function(&self, name: &str) -> Result<(), String> {
    page_dispatch!(self, remove_exposed_function(name))
  }

  // ── Init Scripts ──

  /// Inject a script to run before any page JS on every navigation.
  /// Returns an identifier for later removal.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to register the init script.
  pub async fn add_init_script(&self, source: &str) -> Result<String, String> {
    page_dispatch!(self, add_init_script(source))
  }

  /// Remove a previously injected init script by identifier.
  ///
  /// # Errors
  ///
  /// Returns an error if the identifier is invalid or the backend fails to remove the script.
  pub async fn remove_init_script(&self, identifier: &str) -> Result<(), String> {
    page_dispatch!(self, remove_init_script(identifier))
  }
}

// ─── AnyElement ─────────────────────────────────────────────────────────────

/// Element handle — enum dispatch across backends.
pub enum AnyElement {
  CdpPipe(cdp_pipe::CdpPipeElement),
  CdpRaw(cdp_raw::CdpRawElement),
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
  /// Click this element.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not visible, not interactable, or the backend fails.
  pub async fn click(&self) -> Result<(), String> {
    element_dispatch!(self, click())
  }

  /// Double-click with proper clickCount sequence so the browser fires
  /// both `click` and `dblclick` DOM events.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not interactable or the backend fails.
  pub async fn dblclick(&self) -> Result<(), String> {
    element_dispatch!(self, dblclick())
  }

  /// Hover over this element.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not visible or the backend fails to move the mouse.
  pub async fn hover(&self) -> Result<(), String> {
    element_dispatch!(self, hover())
  }

  /// Type text into this element by dispatching key events.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not focusable or the backend fails.
  pub async fn type_str(&self, text: &str) -> Result<(), String> {
    element_dispatch!(self, type_str(text))
  }

  /// Call a JavaScript function on this element (e.g., "`function()` { this.value = ''; }").
  ///
  /// # Errors
  ///
  /// Returns an error if the JS function throws an exception or the backend fails.
  pub async fn call_js_fn(&self, function: &str) -> Result<(), String> {
    element_dispatch!(self, call_js_fn(function))
  }

  /// Call a JS function on this element and return the value directly.
  /// Single CDP round-trip with returnByValue: true.
  ///
  /// # Errors
  ///
  /// Returns an error if the JS function throws an exception or the backend fails.
  pub async fn call_js_fn_value(&self, function: &str) -> Result<Option<serde_json::Value>, String> {
    element_dispatch!(self, call_js_fn_value(function))
  }

  /// Scroll this element into view.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is detached from the DOM or the backend fails.
  pub async fn scroll_into_view(&self) -> Result<(), String> {
    element_dispatch!(self, scroll_into_view())
  }

  /// Capture a screenshot of this element.
  ///
  /// # Errors
  ///
  /// Returns an error if the element has zero dimensions or screenshot capture fails.
  pub async fn screenshot(&self, format: ImageFormat) -> Result<Vec<u8>, String> {
    element_dispatch!(self, screenshot(format))
  }
}
