//! MCP tool parameter types. Each struct maps to one tool's input schema.
//!
//! All tool params include a `session` field via `#[serde(flatten)]` on `SessionParam`.

use serde::Deserialize;

/// Shared session parameter flattened into every tool's input schema.
/// Defines the `instance:context` key format in one place.
#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct SessionParam {
  #[schemars(description = "Session key, format '<instance>:<context>' (e.g. 'staging:admin'). \
    The INSTANCE (before ':') selects the browser process and its DNS/proxy/flags; in environment \
    setups it is the environment name (dev|staging|prod) and decides which env's DNS the browser uses. \
    The CONTEXT (after ':') isolates cookies/storage within that browser (use one per user). \
    IMPORTANT: a value with NO ':' is treated as a context on the 'default' instance, NOT as an \
    instance -- so to act on an environment always pass '<env>:<context>' (e.g. 'staging:admin'), \
    never a bare 'staging'. Omit entirely only for the plain 'default:default' session.")]
  pub session: Option<String>,
}

impl SessionParam {
  /// Get the session string as `Option<&String>` for backward compat with `sess()`.
  #[must_use]
  pub fn as_opt(&self) -> Option<&String> {
    self.session.as_ref()
  }
}

/// How far a navigation should be awaited before the tool returns.
///
/// A closed set rather than a free `String` so the milestones reach the caller
/// through the tool's JSON schema instead of only its prose description.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum WaitUntil {
  /// Return as soon as the navigation commits (default).
  #[default]
  Commit,
  /// Wait for the `load` event.
  Load,
  /// Wait for `DOMContentLoaded`.
  DomContentLoaded,
  /// Wait until the network has been idle.
  NetworkIdle,
  /// Do not wait for a lifecycle milestone — an alias for `commit`.
  None,
}

impl From<WaitUntil> for ferridriver::options::LoadState {
  fn from(value: WaitUntil) -> Self {
    match value {
      // `none` promises "don't wait", and commit is the earliest point a
      // navigation can return from.
      WaitUntil::Commit | WaitUntil::None => Self::Commit,
      WaitUntil::Load => Self::Load,
      WaitUntil::DomContentLoaded => Self::DomContentLoaded,
      WaitUntil::NetworkIdle => Self::NetworkIdle,
    }
  }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NavigateParams {
  #[schemars(description = "Target URL.")]
  pub url: String,
  #[schemars(
    description = "Navigation wait: `commit` (default, earliest navigation commit), `load`, `domcontentloaded`, `networkidle`, or `none` (same as commit)."
  )]
  pub wait_until: Option<WaitUntil>,
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

/// Encoding for a captured screenshot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ImageFormat {
  /// Lossless, the default.
  #[default]
  Png,
  /// Smaller, lossy; honors `quality`.
  #[serde(alias = "jpg")]
  Jpeg,
  /// WebP; honors `quality`.
  Webp,
}

impl ImageFormat {
  /// The MIME type this format is served as.
  #[must_use]
  pub fn mime(self) -> &'static str {
    match self {
      Self::Png => "image/png",
      Self::Jpeg => "image/jpeg",
      Self::Webp => "image/webp",
    }
  }

  /// The file extension to persist a capture under.
  #[must_use]
  pub fn extension(self) -> &'static str {
    match self {
      Self::Png => "png",
      Self::Jpeg => "jpg",
      Self::Webp => "webp",
    }
  }
}

impl From<ImageFormat> for ferridriver::options::ScreenshotFormat {
  fn from(value: ImageFormat) -> Self {
    match value {
      ImageFormat::Png => Self::Png,
      ImageFormat::Jpeg => Self::Jpeg,
      ImageFormat::Webp => Self::Webp,
    }
  }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ScreenshotParams_ {
  #[schemars(description = "Image format: 'png' (default, lossless), 'jpeg' (smaller, lossy), or 'webp'.")]
  pub format: Option<ImageFormat>,
  #[schemars(
    description = "Image quality for jpeg/webp. Ignored for png. Default: 80.",
    range(min = 0, max = 100)
  )]
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

/// What `page` should do to the session's tabs, context, or browser.
///
/// Closed set so the ten actions — and which of them tear a browser down —
/// live in the tool's JSON schema rather than only in its description.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PageAction {
  /// Go back in the active tab's history.
  Back,
  /// Go forward in the active tab's history.
  Forward,
  /// Reload the active tab.
  Reload,
  /// Open a tab, optionally at `url`.
  New,
  /// Close the tab at `page_index`.
  Close,
  /// Switch to the tab at `page_index`; invalidates old refs.
  Select,
  /// List every session and its tabs.
  List,
  /// Close one session's context: its tabs, cookies and storage.
  CloseContext,
  /// Close one browser process and its contexts, leaving other instances up.
  CloseInstance,
  /// Close every browser this server launched.
  CloseBrowser,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PageParams {
  #[schemars(
    description = "Action: back, forward, reload, new, close, select, list, close_context, close_instance, close_browser."
  )]
  pub action: PageAction,
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

/// What `diagnostics` should report.
///
/// A closed set rather than a free `String` so the accepted values live in the
/// tool's JSON schema: with a `String`, omitting the field produced a bare
/// `missing field 'type'` and a wrong value was only caught by a hand-written
/// match, neither of which told the caller what was legal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticsKind {
  /// Console messages (log/warn/error).
  Console,
  /// HTTP requests seen since load.
  Network,
  /// Begin performance tracing.
  TraceStart,
  /// End tracing and report metrics.
  TraceStop,
}

/// Severity filter for the console feed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ConsoleLevel {
  /// `console.log`.
  Log,
  /// `console.warn`.
  Warn,
  /// `console.error`.
  Error,
  /// `console.info`.
  Info,
  /// `console.debug`.
  Debug,
  /// Every severity (the default).
  #[default]
  All,
}

impl ConsoleLevel {
  /// Whether a message of this `console.*` type passes the filter.
  #[must_use]
  pub fn accepts(self, type_str: &str) -> bool {
    match self {
      Self::All => true,
      Self::Log => type_str == "log",
      Self::Warn => type_str == "warn",
      Self::Error => type_str == "error",
      Self::Info => type_str == "info",
      Self::Debug => type_str == "debug",
    }
  }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DiagnosticsParams {
  #[schemars(description = "REQUIRED. One of: console, network, trace_start, trace_stop.")]
  pub r#type: DiagnosticsKind,
  #[schemars(description = "Filter level for console: log, warn, error, info, debug, all (default).")]
  pub level: Option<ConsoleLevel>,
  #[schemars(description = "Max entries to return. Defaults to 50.")]
  pub limit: Option<usize>,
  #[schemars(
    description = "Case-insensitive substring match. For network, matches the request URL; for console, matches the \
                   message text. Applied before `limit`, so the newest N *matching* entries come back."
  )]
  pub filter: Option<String>,
  #[schemars(
    description = "For network: return only { method, status, url } per request instead of the full record. Full \
                   records include every request and response header and can run to hundreds of KB on a real page, so \
                   prefer this unless you specifically need headers."
  )]
  pub summary: Option<bool>,
  #[serde(flatten)]
  pub session: SessionParam,
}

/// Which installed Chrome an auto-discovering `connect` should look for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ChromeChannel {
  /// Google Chrome (the default).
  #[default]
  Stable,
  /// Google Chrome Beta.
  Beta,
  /// Google Chrome Canary.
  Canary,
}

impl ChromeChannel {
  /// The channel name the browser launcher expects.
  #[must_use]
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Stable => "stable",
      Self::Beta => "beta",
      Self::Canary => "canary",
    }
  }
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
  pub channel: Option<ChromeChannel>,
  #[schemars(description = "Custom Chrome user data directory for auto-discovery.")]
  pub user_data_dir: Option<String>,
  #[serde(flatten)]
  pub session: SessionParam,
}

#[cfg(test)]
mod typed_vocabulary_tests {
  use super::{ChromeChannel, ConsoleLevel, ImageFormat, PageAction, PageParams, ScreenshotParams_, WaitUntil};

  fn schema_of<T: schemars::JsonSchema>() -> String {
    serde_json::to_value(schemars::schema_for!(T))
      .expect("schema")
      .to_string()
  }

  // The point of each enum: the legal values reach the caller through the tool's
  // JSON schema, so a wrong one is rejected by the client before the call.
  #[test]
  fn page_actions_are_enumerated_in_the_schema() {
    let rendered = schema_of::<PageParams>();
    for expected in [
      "back",
      "forward",
      "reload",
      "new",
      "close",
      "select",
      "list",
      "close_context",
      "close_instance",
      "close_browser",
    ] {
      assert!(rendered.contains(expected), "schema must list {expected}: {rendered}");
    }
  }

  #[test]
  fn an_unknown_page_action_is_rejected_by_name() {
    let err = serde_json::from_value::<PageAction>(serde_json::json!("close_tab")).expect_err("must reject");
    assert!(err.to_string().contains("close_context"), "{err}");
  }

  #[test]
  fn screenshot_schema_carries_the_formats_and_the_quality_bounds() {
    let rendered = schema_of::<ScreenshotParams_>();
    for expected in ["png", "jpeg", "webp", "maximum", "minimum"] {
      assert!(rendered.contains(expected), "schema must carry {expected}: {rendered}");
    }
  }

  // `jpg` stays accepted as it was when the field was a free string, without
  // advertising a second spelling in the schema.
  #[test]
  fn jpg_remains_an_accepted_spelling_of_jpeg() {
    let parsed: ImageFormat = serde_json::from_value(serde_json::json!("jpg")).expect("jpg must parse");
    assert_eq!(parsed, ImageFormat::Jpeg);
    assert_eq!(parsed.mime(), "image/jpeg");
    assert_eq!(parsed.extension(), "jpg");
  }

  // `none` used to fall through to `Load` — the strongest wait — despite the
  // description promising the opposite.
  #[test]
  fn wait_until_none_does_not_wait() {
    let parsed: WaitUntil = serde_json::from_value(serde_json::json!("none")).expect("none must parse");
    assert_eq!(
      ferridriver::options::LoadState::from(parsed),
      ferridriver::options::LoadState::Commit
    );
  }

  #[test]
  fn wait_until_maps_each_milestone_to_its_load_state() {
    use ferridriver::options::LoadState;
    for (value, expected) in [
      (WaitUntil::Commit, LoadState::Commit),
      (WaitUntil::Load, LoadState::Load),
      (WaitUntil::DomContentLoaded, LoadState::DomContentLoaded),
      (WaitUntil::NetworkIdle, LoadState::NetworkIdle),
    ] {
      assert_eq!(LoadState::from(value), expected);
    }
  }

  #[test]
  fn console_level_all_accepts_every_severity_and_the_rest_filter() {
    assert!(ConsoleLevel::default().accepts("error"));
    assert!(ConsoleLevel::Warn.accepts("warn"));
    assert!(!ConsoleLevel::Warn.accepts("log"));
  }

  #[test]
  fn chrome_channel_defaults_to_stable() {
    assert_eq!(ChromeChannel::default().as_str(), "stable");
    let parsed: ChromeChannel = serde_json::from_value(serde_json::json!("canary")).expect("canary must parse");
    assert_eq!(parsed.as_str(), "canary");
  }
}

#[cfg(test)]
mod diagnostics_kind_tests {
  use super::{DiagnosticsKind, DiagnosticsParams};

  // The point of the enum: the accepted values reach the caller through the JSON
  // schema, instead of only existing in a hand-written match arm.
  #[test]
  fn schema_enumerates_the_accepted_types() {
    let schema = serde_json::to_value(schemars::schema_for!(DiagnosticsParams)).expect("schema");
    let rendered = schema.to_string();
    for expected in ["console", "network", "trace_start", "trace_stop"] {
      assert!(rendered.contains(expected), "schema must list {expected}: {rendered}");
    }
  }

  #[test]
  fn accepts_the_snake_case_wire_names() {
    for (wire, expected) in [
      ("console", DiagnosticsKind::Console),
      ("network", DiagnosticsKind::Network),
      ("trace_start", DiagnosticsKind::TraceStart),
      ("trace_stop", DiagnosticsKind::TraceStop),
    ] {
      let parsed: DiagnosticsKind =
        serde_json::from_value(serde_json::json!(wire)).unwrap_or_else(|e| panic!("{wire} must parse: {e}"));
      assert_eq!(parsed, expected);
    }
  }

  // A bad value now names the legal set instead of being caught by a match arm.
  #[test]
  fn a_bad_type_reports_the_legal_values() {
    let err = serde_json::from_value::<DiagnosticsKind>(serde_json::json!("netwrok")).expect_err("must reject");
    let message = err.to_string();
    for expected in ["console", "network", "trace_start", "trace_stop"] {
      assert!(message.contains(expected), "error must list {expected}: {message}");
    }
  }

  #[test]
  fn a_missing_type_is_rejected() {
    let err = serde_json::from_value::<DiagnosticsParams>(serde_json::json!({})).expect_err("type is required");
    assert!(err.to_string().contains("type"), "{err}");
  }
}
