//! Backend abstraction layer for browser automation.
//!
//! Provides a unified API across multiple browser backends:
//! - `CdpWs`: Chrome DevTools Protocol over WebSocket (via chromiumoxide) — default
//! - `CdpPipe`: Chrome DevTools Protocol over pipes (--remote-debugging-pipe, fd 3/4)
//! - `WebKit`: Native WKWebView on macOS (subprocess model)
//!
//! Uses enum dispatch (not trait objects) for zero-cost abstraction and Clone support.

pub mod cdp_ws;
pub mod cdp_pipe;
pub mod cdp_raw;
#[cfg(target_os = "macos")]
pub mod webkit;

/// Empty JSON object `{}` — avoids `serde_json::json!({})` heap allocation per call.
#[inline]
pub(crate) fn empty_params() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

use crate::state::{ConsoleMsg, NetRequest};
use std::sync::Arc;
use tokio::sync::RwLock;

// ─── Backend-agnostic types ─────────────────────────────────────────────────

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

/// Which backend to use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendKind {
    /// Chrome DevTools Protocol over WebSocket (default, via chromiumoxide)
    CdpWs,
    /// Chrome DevTools Protocol over pipes (--remote-debugging-pipe)
    CdpPipe,
    /// Chrome DevTools Protocol over WebSocket (our own, fully parallel)
    CdpRaw,
    /// Native WebKit/WKWebView (macOS only)
    #[cfg(target_os = "macos")]
    WebKit,
}

// ─── AnyBrowser ─────────────────────────────────────────────────────────────

/// Browser instance — enum dispatch across backends.
pub enum AnyBrowser {
    CdpWs(cdp_ws::CdpWsBrowser),
    CdpPipe(cdp_pipe::CdpPipeBrowser),
    CdpRaw(cdp_raw::CdpRawBrowser),
    #[cfg(target_os = "macos")]
    WebKit(webkit::WebKitBrowser),
}

impl AnyBrowser {
    pub async fn pages(&self) -> Result<Vec<AnyPage>, String> {
        match self {
            Self::CdpWs(b) => b.pages().await,
            Self::CdpPipe(b) => b.pages().await,
            Self::CdpRaw(b) => b.pages().await,
            #[cfg(target_os = "macos")]
            Self::WebKit(b) => b.pages().await,
        }
    }

    pub async fn new_page(&self, url: &str) -> Result<AnyPage, String> {
        match self {
            Self::CdpWs(b) => b.new_page(url).await,
            Self::CdpPipe(b) => b.new_page(url).await,
            Self::CdpRaw(b) => b.new_page(url).await,
            #[cfg(target_os = "macos")]
            Self::WebKit(b) => b.new_page(url).await,
        }
    }

    pub async fn new_page_isolated(&self, url: &str) -> Result<AnyPage, String> {
        match self {
            Self::CdpWs(b) => b.new_page_isolated(url).await,
            Self::CdpPipe(b) => b.new_page_isolated(url).await,
            Self::CdpRaw(b) => b.new_page_isolated(url).await,
            #[cfg(target_os = "macos")]
            Self::WebKit(b) => b.new_page_isolated(url).await,
        }
    }

    pub async fn close(&mut self) -> Result<(), String> {
        match self {
            Self::CdpWs(b) => b.close().await,
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
    CdpWs(cdp_ws::CdpWsPage),
    CdpPipe(cdp_pipe::CdpPipePage),
    CdpRaw(cdp_raw::CdpRawPage),
    #[cfg(target_os = "macos")]
    WebKit(webkit::WebKitPage),
}

/// Macro to dispatch a method call across all AnyPage variants.
macro_rules! page_dispatch {
    ($self:expr, $method:ident ( $($arg:expr),* $(,)? )) => {
        match $self {
            AnyPage::CdpWs(p) => p.$method($($arg),*).await,
            AnyPage::CdpPipe(p) => p.$method($($arg),*).await,
            AnyPage::CdpRaw(p) => p.$method($($arg),*).await,
            #[cfg(target_os = "macos")]
            AnyPage::WebKit(p) => p.$method($($arg),*).await,
        }
    };
}

impl AnyPage {
    // ── Navigation ──

    pub async fn goto(&self, url: &str) -> Result<(), String> {
        page_dispatch!(self, goto(url))
    }

    pub async fn wait_for_navigation(&self) -> Result<(), String> {
        page_dispatch!(self, wait_for_navigation())
    }

    pub async fn reload(&self) -> Result<(), String> {
        page_dispatch!(self, reload())
    }

    pub async fn go_back(&self) -> Result<(), String> {
        page_dispatch!(self, go_back())
    }

    pub async fn go_forward(&self) -> Result<(), String> {
        page_dispatch!(self, go_forward())
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

    /// Evaluate JS that returns a DOM element and wrap it as AnyElement.
    /// The JS must return a single DOM element (not a value).
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

    // ── Input ──

    pub async fn click_at(&self, x: f64, y: f64) -> Result<(), String> {
        page_dispatch!(self, click_at(x, y))
    }

    /// Click at coordinates with specific button and click count.
    /// button: "left", "right", "middle", "back", "forward"
    /// click_count: 1 for single, 2 for double, 3 for triple
    pub async fn click_at_opts(&self, x: f64, y: f64, button: &str, click_count: u32) -> Result<(), String> {
        page_dispatch!(self, click_at_opts(x, y, button, click_count))
    }

    /// Move mouse to coordinates without clicking.
    pub async fn move_mouse(&self, x: f64, y: f64) -> Result<(), String> {
        page_dispatch!(self, move_mouse(x, y))
    }

    /// Move mouse smoothly with bezier easing over N steps.
    pub async fn move_mouse_smooth(&self, from_x: f64, from_y: f64, to_x: f64, to_y: f64, steps: u32) -> Result<(), String> {
        page_dispatch!(self, move_mouse_smooth(from_x, from_y, to_x, to_y, steps))
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

    // ── Emulation ──

    pub async fn emulate_viewport(&self, config: &crate::options::ViewportConfig) -> Result<(), String> {
        page_dispatch!(self, emulate_viewport(config))
    }

    pub async fn set_user_agent(&self, ua: &str) -> Result<(), String> {
        page_dispatch!(self, set_user_agent(ua))
    }

    pub async fn set_geolocation(
        &self, lat: f64, lng: f64, accuracy: f64,
    ) -> Result<(), String> {
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

    pub async fn set_network_state(
        &self, offline: bool, latency: f64, download: f64, upload: f64,
    ) -> Result<(), String> {
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

    /// Resolve a backend node ID (from a11y snapshot) to an element.
    /// `ref_id` is the ref label (e.g., "e5") used to tag the element for later CSS lookup.
    pub async fn resolve_backend_node(
        &self, backend_node_id: i64, ref_id: &str,
    ) -> Result<AnyElement, String> {
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
            Self::CdpWs(p) => p.attach_listeners(console_log, network_log, dialog_log),
            Self::CdpPipe(p) => p.attach_listeners(console_log, network_log, dialog_log),
            Self::CdpRaw(p) => p.attach_listeners(console_log, network_log, dialog_log),
            #[cfg(target_os = "macos")]
            Self::WebKit(p) => p.attach_listeners(console_log, network_log, dialog_log),
        }
    }

    // ── Element screenshot (by selector) ──

    pub async fn screenshot_element(
        &self, selector: &str, format: ImageFormat,
    ) -> Result<Vec<u8>, String> {
        page_dispatch!(self, screenshot_element(selector, format))
    }

    // ── PDF generation ──

    pub async fn pdf(&self, landscape: bool, print_background: bool) -> Result<Vec<u8>, String> {
        page_dispatch!(self, pdf(landscape, print_background))
    }

    // ── File upload ──

    /// Set files on a file input element found by CSS selector.
    /// Uses CDP DOM.setFileInputFiles with backendNodeId.
    pub async fn set_file_input(
        &self, selector: &str, paths: &[String],
    ) -> Result<(), String> {
        page_dispatch!(self, set_file_input(selector, paths))
    }
}

// ─── AnyElement ─────────────────────────────────────────────────────────────

/// Element handle — enum dispatch across backends.
pub enum AnyElement {
    CdpWs(cdp_ws::CdpWsElement),
    CdpPipe(cdp_pipe::CdpPipeElement),
    CdpRaw(cdp_raw::CdpRawElement),
    #[cfg(target_os = "macos")]
    WebKit(webkit::WebKitElement),
}

macro_rules! element_dispatch {
    ($self:expr, $method:ident ( $($arg:expr),* $(,)? )) => {
        match $self {
            AnyElement::CdpWs(e) => e.$method($($arg),*).await,
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

    pub async fn hover(&self) -> Result<(), String> {
        element_dispatch!(self, hover())
    }

    pub async fn type_str(&self, text: &str) -> Result<(), String> {
        element_dispatch!(self, type_str(text))
    }

    /// Call a JavaScript function on this element (e.g., "function() { this.value = ''; }").
    pub async fn call_js_fn(&self, function: &str) -> Result<(), String> {
        element_dispatch!(self, call_js_fn(function))
    }

    /// Call a JS function on this element and return the value directly.
    /// Single CDP round-trip with returnByValue: true.
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
