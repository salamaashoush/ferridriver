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
/// Matches Playwright's viewport options.
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

/// PDF page-size dimension as accepted by Playwright's `PDFOptions.width`,
/// `PDFOptions.height`, and `PDFOptions.margin.*` fields.
///
/// Playwright TS accepts `string | number`. A bare number is interpreted as
/// CSS pixels; a string must end with one of the unit suffixes `px`, `in`,
/// `cm`, `mm`. Conversion rules match
/// `/tmp/playwright/packages/playwright-core/src/server/chromium/crPdf.ts::convertPrintParameterToInches`.
#[derive(Debug, Clone, PartialEq)]
pub enum PdfSize {
  /// Pixels — either from a bare numeric input or from a `"Npx"` string.
  Pixels(f64),
  /// `"Nin"` — inches.
  Inches(f64),
  /// `"Ncm"` — centimeters.
  Centimeters(f64),
  /// `"Nmm"` — millimeters.
  Millimeters(f64),
}

impl PdfSize {
  /// Convert to inches. Playwright's CDP `Page.printToPDF` expects inches
  /// for `paperWidth` / `paperHeight` / `marginTop` / etc.
  ///
  /// Conversion constants mirror crPdf.ts exactly: `px÷96`, `in`, `cm·37.8/96`, `mm·3.78/96`.
  #[must_use]
  pub fn to_inches(&self) -> f64 {
    match *self {
      Self::Pixels(v) => v / 96.0,
      Self::Inches(v) => v,
      Self::Centimeters(v) => v * 37.8 / 96.0,
      Self::Millimeters(v) => v * 3.78 / 96.0,
    }
  }

  /// Parse a Playwright-style size string (`"10px"`, `"2in"`, `"5cm"`,
  /// `"15mm"`). Unknown suffix — or no suffix — is treated as bare pixels,
  /// matching Playwright's fallback for Phantom-compatibility.
  ///
  /// # Errors
  ///
  /// Returns [`crate::error::FerriError::InvalidArgument`] if the numeric
  /// portion cannot be parsed.
  pub fn parse(text: &str) -> crate::error::Result<Self> {
    let trimmed = text.trim();
    let (num_str, unit) = if trimmed.len() >= 2 {
      let (head, tail) = trimmed.split_at(trimmed.len() - 2);
      match tail.to_ascii_lowercase().as_str() {
        "px" => (head, "px"),
        "in" => (head, "in"),
        "cm" => (head, "cm"),
        "mm" => (head, "mm"),
        _ => (trimmed, "px"),
      }
    } else {
      (trimmed, "px")
    };
    let value: f64 = num_str.trim().parse().map_err(|_| {
      crate::error::FerriError::invalid_argument("pdf size", format!("cannot parse numeric portion of {text:?}"))
    })?;
    Ok(match unit {
      "px" => Self::Pixels(value),
      "in" => Self::Inches(value),
      "cm" => Self::Centimeters(value),
      "mm" => Self::Millimeters(value),
      _ => unreachable!("unit matched above"),
    })
  }
}

/// Per-side PDF margins. Each side may be `Some(PdfSize)` or `None` (zero).
#[derive(Debug, Clone, Default)]
pub struct PdfMargin {
  pub top: Option<PdfSize>,
  pub right: Option<PdfSize>,
  pub bottom: Option<PdfSize>,
  pub left: Option<PdfSize>,
}

/// Full Playwright `PDFOptions` surface (15 fields). Mirrors
/// `/tmp/playwright/packages/playwright-core/src/client/page.ts::PDFOptions`
/// and the CDP `Page.printToPDF` plumbing in
/// `/tmp/playwright/packages/playwright-core/src/server/chromium/crPdf.ts`.
///
/// Defaults: every field is `None`/empty; the CDP layer applies Playwright's
/// own defaults (`scale = 1`, `landscape = false`, `pageRanges = ""`, ...).
/// Only `path` is Rust-side: if set, the generated PDF bytes are written
/// there by `Page::pdf` (the bytes are also returned).
#[derive(Debug, Clone, Default)]
pub struct PdfOptions {
  /// Paper format keyword. Case-insensitive match against
  /// [`pdf_paper_format_size`]: `Letter`, `Legal`, `Tabloid`, `Ledger`,
  /// `A0`..`A6`. When set, overrides `width`/`height`.
  pub format: Option<String>,
  /// Filesystem path to additionally write the generated PDF to.
  pub path: Option<std::path::PathBuf>,
  /// Scale factor. Playwright's default is `1.0` (applied by CDP backend
  /// when `None`). Valid range per Chrome: `0.1..=2.0`.
  pub scale: Option<f64>,
  /// Render header/footer.
  pub display_header_footer: Option<bool>,
  /// HTML template for the header (uses CSS print media).
  pub header_template: Option<String>,
  /// HTML template for the footer.
  pub footer_template: Option<String>,
  /// Include CSS `background`s in the rendering.
  pub print_background: Option<bool>,
  /// Rotate the page 90° for landscape orientation.
  pub landscape: Option<bool>,
  /// Page-range filter, e.g. `"1-5, 8, 11-13"`. Empty string = all pages.
  pub page_ranges: Option<String>,
  /// Page width (ignored if `format` is set).
  pub width: Option<PdfSize>,
  /// Page height (ignored if `format` is set).
  pub height: Option<PdfSize>,
  /// Per-side margins.
  pub margin: Option<PdfMargin>,
  /// Prefer the CSS `@page` size declared in the document over `format` /
  /// `width` / `height`.
  pub prefer_css_page_size: Option<bool>,
  /// Embed a document outline (Chrome's `generateDocumentOutline`).
  pub outline: Option<bool>,
  /// Emit a tagged (structured / accessible) PDF.
  pub tagged: Option<bool>,
}

/// Paper-format size lookup. Case-insensitive. Sizes are in inches, matching
/// the canonical table at
/// `/tmp/playwright/packages/playwright-core/src/server/chromium/crPdf.ts::PagePaperFormats`.
///
/// Returns `(width, height)` in inches, or `None` if the format is unknown.
#[must_use]
pub fn pdf_paper_format_size(format: &str) -> Option<(f64, f64)> {
  match format.to_ascii_lowercase().as_str() {
    "letter" => Some((8.5, 11.0)),
    "legal" => Some((8.5, 14.0)),
    "tabloid" => Some((11.0, 17.0)),
    "ledger" => Some((17.0, 11.0)),
    "a0" => Some((33.1, 46.8)),
    "a1" => Some((23.4, 33.1)),
    "a2" => Some((16.54, 23.4)),
    "a3" => Some((11.7, 16.54)),
    "a4" => Some((8.27, 11.7)),
    "a5" => Some((5.83, 8.27)),
    "a6" => Some((4.13, 5.83)),
    _ => None,
  }
}

/// Options for [`crate::page::Page::close`]. Mirrors Playwright's
/// `page.close({ runBeforeUnload, reason })`.
#[derive(Debug, Clone, Default)]
pub struct PageCloseOptions {
  /// When `true`, the page's `beforeunload` handlers fire before close.
  /// Chromium mapping: switches the CDP method from `Target.closeTarget`
  /// (force-close) to `Page.close` (fires `beforeunload`).
  pub run_before_unload: Option<bool>,
  /// Human-readable reason attached to the resulting `TargetClosed` error
  /// that any in-flight operation on this page will receive.
  pub reason: Option<String>,
}

/// Options for [`crate::browser::Browser::close`]. Mirrors Playwright's
/// `browser.close({ reason })`.
#[derive(Debug, Clone, Default)]
pub struct BrowserCloseOptions {
  /// Human-readable reason surfaced to `TargetClosed` errors emitted by any
  /// in-flight operation on pages/contexts from this browser.
  pub reason: Option<String>,
}

/// Navigation options for goto/reload/goBack/goForward.
#[derive(Debug, Clone, Default)]
pub struct GotoOptions {
  /// When to consider navigation complete:
  /// "load" (default), "domcontentloaded", "networkidle", "commit"
  pub wait_until: Option<String>,
  /// Maximum navigation timeout in milliseconds.
  pub timeout: Option<u64>,
  /// HTTP `Referer` header to send with the navigation request. Mirrors
  /// Playwright's `page.goto(url, { referer })`. If both this and
  /// `extraHTTPHeaders.referer` are set, this wins.
  pub referer: Option<String>,
}

/// Which browser to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserType {
  /// Google Chrome / Chromium
  Chromium,
  /// Mozilla Firefox
  Firefox,
  /// Apple `WebKit` (macOS only)
  WebKit,
}

/// Launch options for the browser -- matches Playwright's `browserType.launch()`.
#[derive(Debug, Clone)]
pub struct LaunchOptions {
  /// Backend protocol: `CdpPipe`, `CdpRaw`, `WebKit`, `Bidi`.
  pub backend: crate::backend::BackendKind,
  /// Which browser to launch. Inferred from backend if not set.
  /// `Chromium` for CDP backends, `Firefox` for `BiDi`, `WebKit` for `WebKit`.
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

#[cfg(test)]
mod pdf_option_tests {
  use super::*;

  // ── PdfSize parsing ──────────────────────────────────────────────────

  #[test]
  fn parses_pixel_suffix() {
    assert_eq!(PdfSize::parse("100px").unwrap(), PdfSize::Pixels(100.0));
  }

  #[test]
  fn parses_inch_suffix() {
    assert_eq!(PdfSize::parse("8.5in").unwrap(), PdfSize::Inches(8.5));
  }

  #[test]
  fn parses_cm_and_mm_suffixes() {
    assert_eq!(PdfSize::parse("10cm").unwrap(), PdfSize::Centimeters(10.0));
    assert_eq!(PdfSize::parse("5.5mm").unwrap(), PdfSize::Millimeters(5.5));
  }

  #[test]
  fn parses_suffix_case_insensitively() {
    assert_eq!(PdfSize::parse("8.5IN").unwrap(), PdfSize::Inches(8.5));
    assert_eq!(PdfSize::parse("100Px").unwrap(), PdfSize::Pixels(100.0));
  }

  #[test]
  fn bare_number_falls_back_to_pixels() {
    // Playwright parity: Phantom-compatible fallback to px if no known unit.
    assert_eq!(PdfSize::parse("42").unwrap(), PdfSize::Pixels(42.0));
  }

  #[test]
  fn unknown_suffix_falls_back_to_pixels() {
    // `em` is not in the table — Playwright treats the whole string as px.
    // The numeric value here is "42" (with "em" treated as suffix but then
    // falling through to the default "px" branch). Match Playwright: it
    // slices the last 2 chars, sees "em" (unknown), then parses the WHOLE
    // original string as a number. "42em" isn't a number → error.
    // We mirror that: unknown suffix + non-numeric body ⇒ InvalidArgument.
    assert!(PdfSize::parse("42em").is_err());
  }

  #[test]
  fn invalid_number_is_rejected() {
    assert!(PdfSize::parse("abcpx").is_err());
  }

  #[test]
  fn short_input_takes_pixel_fallback() {
    // "5" is shorter than 2 chars, so the parser skips suffix detection
    // and uses the bare-pixels path.
    assert_eq!(PdfSize::parse("5").unwrap(), PdfSize::Pixels(5.0));
  }

  // ── PdfSize::to_inches conversion constants ──────────────────────────

  #[test]
  fn pixels_convert_using_96_dpi() {
    assert!((PdfSize::Pixels(96.0).to_inches() - 1.0).abs() < 1e-9);
  }

  #[test]
  fn inches_are_identity() {
    assert!((PdfSize::Inches(2.5).to_inches() - 2.5).abs() < 1e-9);
  }

  #[test]
  fn centimeters_convert_per_playwright_constants() {
    // 37.8 / 96 (Playwright's exact constant).
    let expected = 10.0 * 37.8 / 96.0;
    assert!((PdfSize::Centimeters(10.0).to_inches() - expected).abs() < 1e-9);
  }

  #[test]
  fn millimeters_convert_per_playwright_constants() {
    let expected = 25.0 * 3.78 / 96.0;
    assert!((PdfSize::Millimeters(25.0).to_inches() - expected).abs() < 1e-9);
  }

  // ── Paper format lookup ──────────────────────────────────────────────

  #[test]
  fn paper_formats_return_canonical_sizes() {
    assert_eq!(pdf_paper_format_size("Letter"), Some((8.5, 11.0)));
    assert_eq!(pdf_paper_format_size("A4"), Some((8.27, 11.7)));
    assert_eq!(pdf_paper_format_size("tabloid"), Some((11.0, 17.0)));
    assert_eq!(pdf_paper_format_size("LEDGER"), Some((17.0, 11.0)));
  }

  #[test]
  fn unknown_paper_format_returns_none() {
    assert_eq!(pdf_paper_format_size("A99"), None);
    assert_eq!(pdf_paper_format_size(""), None);
  }

  // ── PdfOptions default is fully empty (CDP-defaults-apply) ───────────

  #[test]
  fn default_pdf_options_has_no_overrides() {
    let opts = PdfOptions::default();
    assert!(opts.format.is_none());
    assert!(opts.path.is_none());
    assert!(opts.scale.is_none());
    assert!(opts.display_header_footer.is_none());
    assert!(opts.header_template.is_none());
    assert!(opts.footer_template.is_none());
    assert!(opts.print_background.is_none());
    assert!(opts.landscape.is_none());
    assert!(opts.page_ranges.is_none());
    assert!(opts.width.is_none());
    assert!(opts.height.is_none());
    assert!(opts.margin.is_none());
    assert!(opts.prefer_css_page_size.is_none());
    assert!(opts.outline.is_none());
    assert!(opts.tagged.is_none());
  }
}
