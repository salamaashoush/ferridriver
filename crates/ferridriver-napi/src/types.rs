//! Shared NAPI object types for ferridriver bindings.

use napi_derive::napi;

/// Convert a JS `number` (f64) to u64 for millisecond timeouts and similar values.
/// Negative values are clamped to 0; fractional parts are truncated.
/// This is the correct semantic for the NAPI boundary where JS has only f64 numbers.
pub(crate) fn f64_to_u64(v: f64) -> u64 {
  if v < 0.0 {
    0
  } else {
    // After the negative check above, v is guaranteed non-negative.
    // Truncation of the fractional part is intentional for ms timeouts.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
      v as u64
    }
  }
}

/// Options for role-based locators (getByRole).
#[napi(object)]
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
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct TextOptions {
  pub exact: Option<bool>,
}

/// Options for filtering locators.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct FilterOptions {
  pub has_text: Option<String>,
  pub has_not_text: Option<String>,
  pub has: Option<String>,
  pub has_not: Option<String>,
}

/// Options for waiting operations.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct WaitOptions {
  /// "visible", "hidden", "attached", "stable"
  pub state: Option<String>,
  pub timeout: Option<f64>,
}

/// Options for screenshots.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct ScreenshotOptions {
  pub full_page: Option<bool>,
  /// "png", "jpeg", "webp"
  pub format: Option<String>,
  pub quality: Option<i32>,
}

/// Viewport configuration.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct ViewportConfig {
  pub width: i32,
  pub height: i32,
  pub device_scale_factor: Option<f64>,
  pub is_mobile: Option<bool>,
  pub has_touch: Option<bool>,
  pub is_landscape: Option<bool>,
}

/// Cookie data.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct CookieData {
  pub name: String,
  pub value: String,
  pub domain: String,
  pub path: String,
  pub secure: bool,
  pub http_only: bool,
  pub expires: Option<f64>,
}

/// Performance metric.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct MetricData {
  pub name: String,
  pub value: f64,
}

/// Element bounding box in viewport coordinates.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct BoundingBox {
  pub x: f64,
  pub y: f64,
  pub width: f64,
  pub height: f64,
}

/// Navigation options (waitUntil, timeout).
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct GotoOptions {
  /// When to consider navigation complete: "load", "domcontentloaded", "networkidle", "commit"
  pub wait_until: Option<String>,
  /// Maximum navigation timeout in milliseconds.
  pub timeout: Option<f64>,
}

impl From<&GotoOptions> for ferridriver::options::GotoOptions {
  fn from(o: &GotoOptions) -> Self {
    Self {
      wait_until: o.wait_until.clone(),
      timeout: o.timeout.map(f64_to_u64),
    }
  }
}

/// Emulate media options.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct EmulateMediaOptions {
  pub media: Option<String>,
  pub color_scheme: Option<String>,
  pub reduced_motion: Option<String>,
  pub forced_colors: Option<String>,
  pub contrast: Option<String>,
}

impl From<&EmulateMediaOptions> for ferridriver::options::EmulateMediaOptions {
  fn from(o: &EmulateMediaOptions) -> Self {
    Self {
      media: o.media.clone(),
      color_scheme: o.color_scheme.clone(),
      reduced_motion: o.reduced_motion.clone(),
      forced_colors: o.forced_colors.clone(),
      contrast: o.contrast.clone(),
    }
  }
}

/// Launch options for the browser.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct LaunchOptions {
  /// Backend to use: "cdp-pipe" (default), "cdp-raw", "webkit"
  pub backend: Option<String>,
  /// WebSocket URL to connect to (instead of launching)
  pub ws_endpoint: Option<String>,
}

// ── Conversion helpers ────────────────────────────────────────────────────

impl From<&RoleOptions> for ferridriver::options::RoleOptions {
  fn from(o: &RoleOptions) -> Self {
    Self {
      name: o.name.clone(),
      exact: o.exact,
      checked: o.checked,
      disabled: o.disabled,
      expanded: o.expanded,
      level: o.level,
      pressed: o.pressed,
      selected: o.selected,
      include_hidden: o.include_hidden,
    }
  }
}

impl From<&TextOptions> for ferridriver::options::TextOptions {
  fn from(o: &TextOptions) -> Self {
    Self { exact: o.exact }
  }
}

impl From<&FilterOptions> for ferridriver::options::FilterOptions {
  fn from(o: &FilterOptions) -> Self {
    Self {
      has_text: o.has_text.clone(),
      has_not_text: o.has_not_text.clone(),
      has: o.has.clone(),
      has_not: o.has_not.clone(),
    }
  }
}

impl From<&WaitOptions> for ferridriver::options::WaitOptions {
  fn from(o: &WaitOptions) -> Self {
    Self {
      state: o.state.clone(),
      timeout: o.timeout.map(f64_to_u64),
    }
  }
}

impl From<&ScreenshotOptions> for ferridriver::options::ScreenshotOptions {
  fn from(o: &ScreenshotOptions) -> Self {
    Self {
      full_page: o.full_page,
      format: o.format.clone(),
      quality: o.quality.map(i64::from),
    }
  }
}

impl From<&ViewportConfig> for ferridriver::options::ViewportConfig {
  fn from(o: &ViewportConfig) -> Self {
    Self {
      width: i64::from(o.width),
      height: i64::from(o.height),
      device_scale_factor: o.device_scale_factor.unwrap_or(1.0),
      is_mobile: o.is_mobile.unwrap_or(false),
      has_touch: o.has_touch.unwrap_or(false),
      is_landscape: o.is_landscape.unwrap_or(false),
    }
  }
}

impl From<&CookieData> for ferridriver::backend::CookieData {
  fn from(o: &CookieData) -> Self {
    Self {
      name: o.name.clone(),
      value: o.value.clone(),
      domain: o.domain.clone(),
      path: o.path.clone(),
      secure: o.secure,
      http_only: o.http_only,
      expires: o.expires,
    }
  }
}

impl From<&ferridriver::backend::CookieData> for CookieData {
  fn from(o: &ferridriver::backend::CookieData) -> Self {
    Self {
      name: o.name.clone(),
      value: o.value.clone(),
      domain: o.domain.clone(),
      path: o.path.clone(),
      secure: o.secure,
      http_only: o.http_only,
      expires: o.expires,
    }
  }
}

impl From<&ferridriver::backend::MetricData> for MetricData {
  fn from(o: &ferridriver::backend::MetricData) -> Self {
    Self {
      name: o.name.clone(),
      value: o.value,
    }
  }
}

// ── Event data types (Playwright-compatible) ─────────────────────────────

/// Network response data. Matches Playwright's Response interface (subset).
#[napi(object)]
#[derive(Debug, Clone)]
pub struct ResponseData {
  pub url: String,
  pub status: i32,
  pub status_text: String,
}
