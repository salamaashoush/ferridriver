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

/// Media emulation options -- matches Playwright's page.emulateMedia().
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
