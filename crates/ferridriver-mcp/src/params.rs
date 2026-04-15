//! MCP tool parameter types. Each struct maps to one tool's input schema.
//!
//! All tool params include a `session` field via `#[serde(flatten)]` on `SessionParam`.

use serde::Deserialize;

/// Shared session parameter flattened into every tool's input schema.
/// Defines the `instance:context` key format in one place.
#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct SessionParam {
  #[schemars(description = "Session key. Format: 'instance:context' (e.g. 'staging:admin'). \
    Instance selects the browser process (each can have its own DNS/proxy/flags). \
    Context isolates cookies/storage within that browser. \
    Plain name without ':' uses the default instance. Omit for 'default:default'.")]
  pub session: Option<String>,
}

impl SessionParam {
  /// Get the session string as `Option<&String>` for backward compat with `sess()`.
  #[must_use]
  pub fn as_opt(&self) -> Option<&String> {
    self.session.as_ref()
  }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NavigateParams {
  #[schemars(description = "Target URL.")]
  pub url: String,
  #[schemars(
    description = "Navigation wait: `commit` (default, earliest navigation commit), `load`, `domcontentloaded`, `networkidle`, or `none`."
  )]
  pub wait_until: Option<String>,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NewPageParams {
  #[schemars(description = "URL to open.")]
  pub url: Option<String>,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ClosePageParams {
  #[schemars(description = "Page index to close.")]
  pub page_index: usize,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SelectPageParams {
  #[schemars(description = "Page index.")]
  pub page_index: usize,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ClickParams {
  #[schemars(description = "Element ref from snapshot.")]
  pub r#ref: Option<String>,
  #[schemars(description = "CSS selector fallback.")]
  pub selector: Option<String>,
  #[schemars(description = "Double click.")]
  pub double_click: Option<bool>,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ClickAtParams {
  #[schemars(description = "X coordinate in viewport pixels.")]
  pub x: f64,
  #[schemars(description = "Y coordinate in viewport pixels.")]
  pub y: f64,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct HoverParams {
  #[schemars(description = "Element ref.")]
  pub r#ref: Option<String>,
  #[schemars(description = "CSS selector.")]
  pub selector: Option<String>,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FillParams {
  #[schemars(description = "Element ref.")]
  pub r#ref: Option<String>,
  #[schemars(description = "CSS selector.")]
  pub selector: Option<String>,
  #[schemars(description = "Value to fill.")]
  pub value: String,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TypeTextParams {
  #[schemars(
    description = "Text to send as keyboard input. Types into whichever element is focused—use click(ref=...) on the field first."
  )]
  pub text: String,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PressKeyParams {
  #[schemars(
    description = "Key or shortcut. Examples: Enter, Tab, ArrowDown, Escape, Control+a, Meta+v, Control+Shift+t (Playwright-style)."
  )]
  pub key: String,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DragParams {
  #[schemars(description = "Start X coordinate in viewport pixels.")]
  pub from_x: f64,
  #[schemars(description = "Start Y coordinate in viewport pixels.")]
  pub from_y: f64,
  #[schemars(description = "End X coordinate in viewport pixels.")]
  pub to_x: f64,
  #[schemars(description = "End Y coordinate in viewport pixels.")]
  pub to_y: f64,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ScrollParams {
  #[schemars(description = "Horizontal scroll amount in pixels. Positive = right, negative = left.")]
  pub delta_x: Option<f64>,
  #[schemars(
    description = "Vertical scroll amount in pixels. Positive = down, negative = up. Common values: 300 (one scroll), -300 (scroll up)."
  )]
  pub delta_y: Option<f64>,
  #[schemars(
    description = "CSS selector to scroll into view. When provided, delta_x/delta_y are ignored and the element is scrolled into the viewport."
  )]
  pub selector: Option<String>,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ScreenshotParams_ {
  #[schemars(description = "Image format: 'png' (default, lossless), 'jpeg' (smaller, lossy), or 'webp'.")]
  pub format: Option<String>,
  #[schemars(description = "Image quality 0-100 for jpeg/webp. Ignored for png. Default: 80.")]
  pub quality: Option<i64>,
  #[schemars(description = "Capture the full scrollable page, not just the viewport. Default: false.")]
  pub full_page: Option<bool>,
  #[schemars(description = "CSS selector to screenshot a specific element instead of the full page.")]
  pub selector: Option<String>,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EvaluateParams {
  #[schemars(description = "JS expression.")]
  pub expression: String,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SnapshotParams {
  #[serde(flatten)]
  pub session: SessionParam,
  #[schemars(description = "Accessibility tree depth limit. -1 or omit for unlimited. 0 = root only.")]
  pub depth: Option<i32>,
  #[schemars(
    description = "Track key for incremental snapshots. When set, subsequent calls with the same key return only changed/new nodes."
  )]
  pub track: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WaitForParams {
  #[schemars(
    description = "CSS selector to wait for. Resolves when at least one matching element appears in the DOM."
  )]
  pub selector: Option<String>,
  #[schemars(description = "Text substring to wait for in the page body. Case-sensitive.")]
  pub text: Option<String>,
  #[schemars(description = "Timeout ms.")]
  pub timeout: Option<u64>,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SessionOnlyParams {
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetCookieParams {
  pub name: String,
  pub value: String,
  pub domain: Option<String>,
  pub path: Option<String>,
  pub secure: Option<bool>,
  pub http_only: Option<bool>,
  pub expires: Option<f64>,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DeleteCookieParams_ {
  pub name: String,
  pub domain: Option<String>,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EmulateDeviceParams {
  pub width: Option<i64>,
  pub height: Option<i64>,
  pub device_scale_factor: Option<f64>,
  pub mobile: Option<bool>,
  pub user_agent: Option<String>,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetGeolocationParams {
  pub latitude: f64,
  pub longitude: f64,
  pub accuracy: Option<f64>,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetNetworkStateParams {
  #[schemars(description = "offline or online.")]
  pub state: String,
  pub download_throughput: Option<f64>,
  pub upload_throughput: Option<f64>,
  pub latency: Option<f64>,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct LocalStorageKeyParams {
  pub key: String,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct LocalStorageSetParams {
  pub key: String,
  pub value: String,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetContentParams {
  pub html: String,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ConsoleMessagesParams {
  #[schemars(description = "Filter: log, warn, error, info, debug, or all.")]
  pub level: Option<String>,
  #[schemars(description = "Max messages to return.")]
  pub limit: Option<usize>,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NetworkRequestsParams {
  #[schemars(description = "Max requests to return.")]
  pub limit: Option<usize>,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FormField {
  #[schemars(description = "Element ref.")]
  pub r#ref: Option<String>,
  #[schemars(description = "CSS selector.")]
  pub selector: Option<String>,
  #[schemars(description = "Value to fill.")]
  pub value: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FillFormParams {
  #[schemars(description = "Array of {ref, selector, value} fields.")]
  pub fields: Vec<FormField>,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchPageParams {
  #[schemars(description = "Text or regex pattern to search for in page content.")]
  pub pattern: String,
  #[schemars(description = "Treat pattern as regex. Default: false.")]
  pub regex: Option<bool>,
  #[schemars(description = "Case-sensitive search. Default: false.")]
  pub case_sensitive: Option<bool>,
  #[schemars(description = "Characters of surrounding context per match. Default: 150.")]
  pub context_chars: Option<usize>,
  #[schemars(description = "CSS selector to limit search scope.")]
  pub selector: Option<String>,
  #[schemars(description = "Maximum matches to return. Default: 25.")]
  pub max_results: Option<usize>,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FindElementsParams {
  #[schemars(description = "CSS selector to query elements.")]
  pub selector: String,
  #[schemars(description = "Specific attributes to extract (e.g. [\"href\", \"src\"]).")]
  pub attributes: Option<Vec<String>>,
  #[schemars(description = "Maximum elements to return. Default: 50.")]
  pub max_results: Option<usize>,
  #[schemars(description = "Include text content of each element. Default: true.")]
  pub include_text: Option<bool>,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SelectOptionParams {
  #[schemars(description = "Element ref from snapshot.")]
  pub r#ref: Option<String>,
  #[schemars(description = "CSS selector.")]
  pub selector: Option<String>,
  #[schemars(description = "Option value to select.")]
  pub value: Option<String>,
  #[schemars(description = "Option text/label to select.")]
  pub label: Option<String>,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetDropdownOptionsParams {
  #[schemars(description = "Element ref from snapshot.")]
  pub r#ref: Option<String>,
  #[schemars(description = "CSS selector.")]
  pub selector: Option<String>,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct UploadFileParams {
  #[schemars(description = "Element ref from snapshot (preferred for file inputs).")]
  pub r#ref: Option<String>,
  #[schemars(description = "CSS selector for the file input when `ref` is not used.")]
  pub selector: Option<String>,
  #[schemars(description = "Absolute path to the file to upload.")]
  pub path: String,
  #[serde(flatten)]
  pub session: SessionParam,
}

// ── Consolidated param types (used by refactored tool modules) ─────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PageParams {
  #[schemars(description = "Action: back, forward, reload, new, close, select, list, close_browser.")]
  pub action: String,
  #[schemars(description = "URL for 'new' action.")]
  pub url: Option<String>,
  #[schemars(description = "Page index for close/select actions.")]
  pub page_index: Option<usize>,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CookiesParams {
  #[schemars(description = "Action: get, set, delete, clear.")]
  pub action: String,
  #[schemars(description = "Cookie name (required for set/delete).")]
  pub name: Option<String>,
  #[schemars(description = "Cookie value (required for set).")]
  pub value: Option<String>,
  #[schemars(description = "Cookie domain (e.g. '.example.com'). Required for set. Used to scope delete.")]
  pub domain: Option<String>,
  #[schemars(description = "Cookie path. Defaults to '/'.")]
  pub path: Option<String>,
  #[schemars(description = "Restrict cookie to HTTPS only. Default: false.")]
  pub secure: Option<bool>,
  #[schemars(description = "Prevent JavaScript access to cookie. Default: false.")]
  pub http_only: Option<bool>,
  #[schemars(description = "Cookie expiry as Unix timestamp in seconds. Omit for session cookie.")]
  pub expires: Option<f64>,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct StorageParams {
  #[schemars(description = "Action: get, set, list, clear.")]
  pub action: String,
  #[schemars(description = "Storage key (required for get/set).")]
  pub key: Option<String>,
  #[schemars(description = "Storage value (required for set).")]
  pub value: Option<String>,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EmulateParams {
  #[schemars(description = "Viewport width in pixels. Common: 375 (iPhone), 768 (tablet), 1280 (desktop).")]
  pub width: Option<i64>,
  #[schemars(description = "Viewport height in pixels.")]
  pub height: Option<i64>,
  #[schemars(description = "Device pixel ratio. 1.0 = standard, 2.0 = retina/HiDPI, 3.0 = ultra-high density.")]
  pub device_scale_factor: Option<f64>,
  #[schemars(description = "Enable mobile mode (touch events, mobile viewport behavior). Default: false.")]
  pub mobile: Option<bool>,
  #[schemars(description = "Custom User-Agent string to override the browser default.")]
  pub user_agent: Option<String>,
  #[schemars(description = "Latitude for geolocation override (-90 to 90).")]
  pub latitude: Option<f64>,
  #[schemars(description = "Longitude for geolocation override (-180 to 180).")]
  pub longitude: Option<f64>,
  #[schemars(description = "Geolocation accuracy in meters. Default: 1.0.")]
  pub accuracy: Option<f64>,
  #[schemars(description = "Network state: 'offline' (disable network) or 'online' (restore network).")]
  pub network: Option<String>,
  #[schemars(description = "Network latency in milliseconds. Simulates slow connections.")]
  pub latency: Option<f64>,
  #[schemars(description = "Download speed limit in bytes/sec. -1 = unlimited. Example: 50000 = ~50KB/s (slow 3G).")]
  pub download_throughput: Option<f64>,
  #[schemars(description = "Upload speed limit in bytes/sec. -1 = unlimited.")]
  pub upload_throughput: Option<f64>,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DiagnosticsParams {
  #[schemars(description = "Type: console, network, trace_start, trace_stop.")]
  pub r#type: String,
  #[schemars(description = "Filter level for console: log, warn, error, info, debug, all.")]
  pub level: Option<String>,
  #[schemars(description = "Max entries to return.")]
  pub limit: Option<usize>,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ConnectParams {
  #[schemars(
    description = "WebSocket URL (ws://...) or HTTP debugger URL (http://...) to connect to a running Chrome instance. Omit for auto-discovery."
  )]
  pub url: Option<String>,
  #[schemars(
    description = "Auto-discover a running Chrome instance by reading DevToolsActivePort file. Ignored if url is provided."
  )]
  pub auto_discover: Option<bool>,
  #[schemars(description = "Chrome channel for auto-discovery: 'stable' (default), 'beta', 'canary'.")]
  pub channel: Option<String>,
  #[schemars(description = "Custom Chrome user data directory for auto-discovery.")]
  pub user_data_dir: Option<String>,
  #[serde(flatten)]
  pub session: SessionParam,
}
