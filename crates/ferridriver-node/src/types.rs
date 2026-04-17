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
  /// SameSite attribute: "Strict", "Lax", or "None".
  pub same_site: Option<String>,
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

/// Navigation options (waitUntil, timeout, referer).
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct GotoOptions {
  /// When to consider navigation complete: "load", "domcontentloaded", "networkidle", "commit"
  pub wait_until: Option<String>,
  /// Maximum navigation timeout in milliseconds.
  pub timeout: Option<f64>,
  /// HTTP `Referer` header to send with the navigation request. Mirrors
  /// Playwright's `page.goto(url, { referer })`.
  pub referer: Option<String>,
}

impl From<GotoOptions> for ferridriver::options::GotoOptions {
  fn from(o: GotoOptions) -> Self {
    Self {
      wait_until: o.wait_until,
      timeout: o.timeout.map(f64_to_u64),
      referer: o.referer,
    }
  }
}

/// Options for `page.close({ runBeforeUnload?, reason? })`. Playwright parity.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct PageCloseOptions {
  /// When `true`, fires the page's `beforeunload` handlers before
  /// unloading. CDP switches from `Target.closeTarget` (force) to
  /// `Page.close`; BiDi passes `promptUnload: true`; WebKit dispatches
  /// a synthetic `BeforeUnloadEvent` on the window.
  pub run_before_unload: Option<bool>,
  /// Human-readable reason surfaced on subsequent `TargetClosed` errors
  /// emitted to in-flight operations on this page.
  pub reason: Option<String>,
}

impl From<PageCloseOptions> for ferridriver::options::PageCloseOptions {
  fn from(o: PageCloseOptions) -> Self {
    Self {
      run_before_unload: o.run_before_unload,
      reason: o.reason,
    }
  }
}

/// Options for `browser.close({ reason? })`. Playwright parity.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct BrowserCloseOptions {
  /// Human-readable reason surfaced on `TargetClosed` errors emitted to
  /// in-flight operations on this browser's pages/contexts.
  pub reason: Option<String>,
}

impl From<BrowserCloseOptions> for ferridriver::options::BrowserCloseOptions {
  fn from(o: BrowserCloseOptions) -> Self {
    Self { reason: o.reason }
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

impl From<EmulateMediaOptions> for ferridriver::options::EmulateMediaOptions {
  fn from(o: EmulateMediaOptions) -> Self {
    Self {
      media: o.media,
      color_scheme: o.color_scheme,
      reduced_motion: o.reduced_motion,
      forced_colors: o.forced_colors,
      contrast: o.contrast,
    }
  }
}

/// Launch options for the browser.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct LaunchOptions {
  /// Backend protocol: "cdp-pipe" (default), "cdp-raw", "webkit", "bidi".
  /// Inferred from `browser` if not set.
  pub backend: Option<String>,
  /// Browser product to launch: "chromium" (default), "firefox", "webkit".
  /// Determines the default backend and executable detection:
  /// - "chromium" -> cdp-pipe backend, detects Chrome/Chromium
  /// - "firefox"  -> bidi backend, detects Firefox
  /// - "webkit"   -> webkit backend (macOS only)
  pub browser: Option<String>,
  /// WebSocket URL to connect to (instead of launching)
  pub ws_endpoint: Option<String>,
  /// Run in headless mode (default: true)
  pub headless: Option<bool>,
  /// Path to the browser executable
  pub executable_path: Option<String>,
  /// Additional browser arguments
  pub args: Option<Vec<String>>,
}

// ── Conversion helpers ────────────────────────────────────────────────────

impl From<RoleOptions> for ferridriver::options::RoleOptions {
  fn from(o: RoleOptions) -> Self {
    Self {
      name: o.name,
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

impl From<TextOptions> for ferridriver::options::TextOptions {
  fn from(o: TextOptions) -> Self {
    Self { exact: o.exact }
  }
}

impl From<FilterOptions> for ferridriver::options::FilterOptions {
  fn from(o: FilterOptions) -> Self {
    Self {
      has_text: o.has_text,
      has_not_text: o.has_not_text,
      has: o.has,
      has_not: o.has_not,
    }
  }
}

impl From<WaitOptions> for ferridriver::options::WaitOptions {
  fn from(o: WaitOptions) -> Self {
    Self {
      state: o.state,
      timeout: o.timeout.map(f64_to_u64),
    }
  }
}

impl From<ScreenshotOptions> for ferridriver::options::ScreenshotOptions {
  fn from(o: ScreenshotOptions) -> Self {
    Self {
      full_page: o.full_page,
      format: o.format,
      quality: o.quality.map(i64::from),
    }
  }
}

impl From<ViewportConfig> for ferridriver::options::ViewportConfig {
  fn from(o: ViewportConfig) -> Self {
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
      same_site: o
        .same_site
        .as_deref()
        .and_then(|v| v.parse::<ferridriver::backend::SameSite>().ok()),
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
      same_site: o.same_site.map(|ss| ss.as_str().to_string()),
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

/// Per-side PDF margin. Each side is a `string | number`. Playwright parity:
/// bare number → CSS pixels, string takes a unit suffix (`"10cm"`, `"2in"`,
/// `"5mm"`, `"100px"`).
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct PdfMarginOptions {
  pub top: Option<napi::Either<f64, String>>,
  pub right: Option<napi::Either<f64, String>>,
  pub bottom: Option<napi::Either<f64, String>>,
  pub left: Option<napi::Either<f64, String>>,
}

/// Full Playwright `PDFOptions` surface (all 15 fields). Mirrors
/// `/tmp/playwright/packages/playwright-core/src/client/page.ts::PDFOptions`.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct PdfOptions {
  pub scale: Option<f64>,
  pub display_header_footer: Option<bool>,
  pub header_template: Option<String>,
  pub footer_template: Option<String>,
  pub print_background: Option<bool>,
  pub landscape: Option<bool>,
  pub page_ranges: Option<String>,
  /// Paper format keyword: `Letter`, `Legal`, `Tabloid`, `Ledger`,
  /// `A0`..`A6`. Case-insensitive. When set, overrides `width`/`height`.
  pub format: Option<String>,
  pub width: Option<napi::Either<f64, String>>,
  pub height: Option<napi::Either<f64, String>>,
  pub margin: Option<PdfMarginOptions>,
  pub path: Option<String>,
  /// Playwright capitalizes this as `preferCSSPageSize` (CSS upper-case).
  /// napi-rs would auto-lowercase to `preferCssPageSize`; override the
  /// emitted JS name explicitly so the TS surface matches Playwright.
  #[napi(js_name = "preferCSSPageSize")]
  pub prefer_css_page_size: Option<bool>,
  pub tagged: Option<bool>,
  pub outline: Option<bool>,
}

fn js_size_to_rust(v: napi::Either<f64, String>) -> napi::Result<ferridriver::options::PdfSize> {
  match v {
    napi::Either::A(px) => Ok(ferridriver::options::PdfSize::Pixels(px)),
    napi::Either::B(s) => ferridriver::options::PdfSize::parse(&s).map_err(|e| napi::Error::from_reason(e.to_string())),
  }
}

impl TryFrom<PdfOptions> for ferridriver::options::PdfOptions {
  type Error = napi::Error;

  fn try_from(o: PdfOptions) -> napi::Result<Self> {
    let width = o.width.map(js_size_to_rust).transpose()?;
    let height = o.height.map(js_size_to_rust).transpose()?;
    let margin = o
      .margin
      .map(|m| -> napi::Result<ferridriver::options::PdfMargin> {
        Ok(ferridriver::options::PdfMargin {
          top: m.top.map(js_size_to_rust).transpose()?,
          right: m.right.map(js_size_to_rust).transpose()?,
          bottom: m.bottom.map(js_size_to_rust).transpose()?,
          left: m.left.map(js_size_to_rust).transpose()?,
        })
      })
      .transpose()?;
    Ok(Self {
      format: o.format,
      path: o.path.map(std::path::PathBuf::from),
      scale: o.scale,
      display_header_footer: o.display_header_footer,
      header_template: o.header_template,
      footer_template: o.footer_template,
      print_background: o.print_background,
      landscape: o.landscape,
      page_ranges: o.page_ranges,
      width,
      height,
      margin,
      prefer_css_page_size: o.prefer_css_page_size,
      outline: o.outline,
      tagged: o.tagged,
    })
  }
}

/// Shape of a JS `RegExp` as seen across NAPI.
///
/// `RegExp.prototype.source` and `RegExp.prototype.flags` are accessor
/// properties on the prototype chain, but `napi_get_named_property`
/// (which napi-rs's `#[napi(object)]` deserializer uses per field) calls
/// V8's `[[Get]]` operation — that walks the prototype chain and invokes
/// accessors. So `source` is available on any real `RegExp` instance the
/// caller passes in, and this struct binds to a bare `/pattern/flags`
/// literal without any client-side serialization step.
///
/// Plain user objects that happen to carry a `source` string also match
/// (intentional — Playwright itself treats any object with these fields
/// as regex-shaped; see `isRegExp` in
/// `/tmp/playwright/packages/isomorphic/urlMatch.ts`).
#[napi(object)]
#[derive(Debug, Clone)]
pub struct JsRegExpLike {
  /// `RegExp.prototype.source` — the pattern between the slashes, without
  /// the enclosing slashes or flags.
  pub source: String,
  /// `RegExp.prototype.flags` — the flag letters (e.g. `"i"`, `"gs"`).
  /// Absent on bare regex literals with no flags, which expose `flags` as
  /// an empty string; `Option` tolerates both shapes.
  pub flags: Option<String>,
}

/// Lower a user-passed URL matcher to a Rust [`ferridriver::UrlMatcher`].
///
/// NAPI accepts `string | RegExp` directly — exactly the Playwright
/// surface `URLMatch = string | RegExp | ((url) => boolean) | URLPattern`
/// minus the predicate/URLPattern branches, which ride on separate NAPI
/// methods (predicate needs a `ThreadsafeFunction`; URLPattern is Node
/// 24+). No client-side serialization: a literal `/foo/i` flows through
/// unchanged.
pub(crate) fn string_or_regex_to_rust(
  input: napi::Either<String, JsRegExpLike>,
) -> napi::Result<ferridriver::UrlMatcher> {
  match input {
    napi::Either::A(glob) => ferridriver::UrlMatcher::glob(glob).map_err(|e| napi::Error::from_reason(e.to_string())),
    napi::Either::B(re) => ferridriver::UrlMatcher::regex_from_source(&re.source, re.flags.as_deref().unwrap_or(""))
      .map_err(|e| napi::Error::from_reason(e.to_string())),
  }
}
