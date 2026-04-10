//! Option structs for the Playwright-compatible Page and Locator API.

/// Options for role-based locators (getByRole).
#[derive(Debug, Clone, Default)]
pub struct RoleOptions {
  pub name: Option<String>,
  pub exact: Option<bool>,
  pub checked: Option<bool>,
  pub disabled: Option<bool>,
  pub expanded: Option<bool>,
  pub level: Option<i32>,
  pub pressed: Option<bool>,
  pub selected: Option<bool>,
  pub include_hidden: Option<bool>,
}

/// Options for text-based locators (getByText, getByLabel, etc.).
#[derive(Debug, Clone, Default)]
pub struct TextOptions {
  pub exact: Option<bool>,
}

/// Options for filtering locators.
#[derive(Debug, Clone, Default)]
pub struct FilterOptions {
  pub has_text: Option<String>,
  pub has_not_text: Option<String>,
  pub has: Option<String>,
  pub has_not: Option<String>,
}

/// Options for waiting operations.
#[derive(Debug, Clone, Default)]
pub struct WaitOptions {
  /// "visible", "hidden", "attached", "stable"
  pub state: Option<String>,
  pub timeout: Option<u64>,
}

/// Options for screenshots.
#[derive(Debug, Clone, Default)]
pub struct ScreenshotOptions {
  pub full_page: Option<bool>,
  /// "png", "jpeg", "webp"
  pub format: Option<String>,
  pub quality: Option<i64>,
}

/// Element bounding box in viewport coordinates.
#[derive(Debug, Clone, Copy)]
pub struct BoundingBox {
  pub x: f64,
  pub y: f64,
  pub width: f64,
  pub height: f64,
}

/// Viewport configuration -- consistent across all backends.
/// Matches Playwright's viewport options and chromiumoxide's Viewport struct.
#[derive(Debug, Clone)]
pub struct ViewportConfig {
  /// CSS pixel width of the viewport.
  pub width: i64,
  /// CSS pixel height of the viewport.
  pub height: i64,
  /// Device scale factor (DPR). 1 for standard, 2 for Retina.
  pub device_scale_factor: f64,
  /// Simulate mobile device.
  pub is_mobile: bool,
  /// Enable touch events.
  pub has_touch: bool,
  /// Landscape orientation.
  pub is_landscape: bool,
}

/// Media emulation options -- matches Playwright's `page.emulateMedia()`.
#[derive(Debug, Clone, Default)]
pub struct EmulateMediaOptions {
  /// "screen", "print", or null to reset
  pub media: Option<String>,
  /// "light", "dark", "no-preference"
  pub color_scheme: Option<String>,
  /// "reduce", "no-preference"
  pub reduced_motion: Option<String>,
  /// "active", "none"
  pub forced_colors: Option<String>,
  /// "more", "less", "no-preference"
  pub contrast: Option<String>,
}

/// Navigation options for goto/reload/goBack/goForward.
#[derive(Debug, Clone, Default)]
pub struct GotoOptions {
  /// When to consider navigation complete:
  /// "load" (default), "domcontentloaded", "networkidle", "commit"
  pub wait_until: Option<String>,
  /// Maximum navigation timeout in milliseconds.
  pub timeout: Option<u64>,
}

/// Which browser to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserType {
  /// Google Chrome / Chromium
  Chromium,
  /// Mozilla Firefox
  Firefox,
  /// Apple WebKit (macOS only)
  WebKit,
}

/// Launch options for the browser -- matches Playwright's `browserType.launch()`.
#[derive(Debug, Clone)]
pub struct LaunchOptions {
  /// Backend protocol: `CdpPipe`, `CdpRaw`, `WebKit`, `Bidi`.
  pub backend: crate::backend::BackendKind,
  /// Which browser to launch. Inferred from backend if not set.
  /// `Chromium` for CDP backends, `Firefox` for BiDi, `WebKit` for WebKit.
  pub browser: Option<BrowserType>,
  /// Run in headful mode (show browser window). Default: false (headless).
  pub headless: bool,
  /// Path to browser executable. Default: auto-detect based on `browser` type.
  pub executable_path: Option<String>,
  /// Extra command-line arguments to pass to the browser.
  pub args: Vec<String>,
  /// User data directory. Default: temp dir per launch.
  pub user_data_dir: Option<String>,
  /// WebSocket URL to connect to instead of launching.
  pub ws_endpoint: Option<String>,
  /// Auto-connect to running Chrome instance.
  pub auto_connect: Option<AutoConnectOptions>,
  /// Default viewport for new pages. None = use browser defaults.
  pub viewport: Option<ViewportConfig>,
  /// Slow down operations by this many ms (for debugging).
  pub slow_mo: Option<u64>,
  /// Default navigation timeout in ms.
  pub timeout: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct AutoConnectOptions {
  pub channel: String,
  pub user_data_dir: Option<String>,
}

impl Default for LaunchOptions {
  fn default() -> Self {
    Self {
      backend: crate::backend::BackendKind::CdpPipe,
      browser: None,
      headless: true,
      executable_path: None,
      args: Vec::new(),
      user_data_dir: None,
      ws_endpoint: None,
      auto_connect: None,
      viewport: Some(ViewportConfig::default()),
      slow_mo: None,
      timeout: None,
    }
  }
}

impl Default for ViewportConfig {
  fn default() -> Self {
    Self {
      width: 1280,
      height: 720,
      device_scale_factor: 1.0,
      is_mobile: false,
      has_touch: false,
      is_landscape: false,
    }
  }
}
