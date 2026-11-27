//! Shared NAPI object types for ferridriver bindings.

mod actions;
mod audits;
mod navigation;

pub(crate) use actions::*;
pub(crate) use audits::*;
pub(crate) use navigation::*;

use napi_derive::napi;

/// The workspace's `FxHashMap` under the one name napi-derive renders as
/// `Record<K, V>` in `index.d.ts`; under any other name the typings would
/// carry the Rust type's name instead.
pub(crate) type HashMap<K, V> = rustc_hash::FxHashMap<K, V>;

/// Convert a JS `number` (f64) to u64 for millisecond timeouts and similar values.
/// Negative values are clamped to 0; fractional parts are truncated.
/// This is the correct semantic for the NAPI boundary where JS has only f64 numbers.
#[expect(
  clippy::cast_possible_truncation,
  clippy::cast_sign_loss,
  reason = "a float-to-int `as` truncates toward zero, saturates at both bounds and maps NaN to 0, exactly the conversion wanted"
)]
pub(crate) fn f64_to_u64(v: f64) -> u64 {
  if v < 0.0 { 0 } else { v as u64 }
}

/// [`f64_to_u64`], saturating at `u32::MAX`.
pub(crate) fn f64_to_u32(v: f64) -> u32 {
  u32::try_from(f64_to_u64(v)).unwrap_or(u32::MAX)
}

/// [`f64_to_u64`], saturating at `i64::MAX`.
pub(crate) fn f64_to_i64(v: f64) -> i64 {
  i64::try_from(f64_to_u64(v)).unwrap_or(i64::MAX)
}

/// [`f64_to_u64`], saturating at `usize::MAX`.
pub(crate) fn f64_to_usize(v: f64) -> usize {
  usize::try_from(f64_to_u64(v)).unwrap_or(usize::MAX)
}

/// A count, id or millisecond value as the JS `number` it crosses as.
#[expect(
  clippy::cast_precision_loss,
  reason = "a JS number is an f64; a value past 2^53 rounds exactly as it would in JS"
)]
pub(crate) fn u64_to_f64(v: u64) -> f64 {
  v as f64
}

/// [`u64_to_f64`] for a `usize` count.
pub(crate) fn usize_to_f64(v: usize) -> f64 {
  u64_to_f64(u64::try_from(v).unwrap_or(u64::MAX))
}

/// Options for `page.route` / `browserContext.route`.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct RouteOptions {
  /// Playwright: `times?: number` — how many times the handler may fire
  /// before it is automatically removed.
  pub times: Option<f64>,
}

impl RouteOptions {
  /// Resolve `times` to a `u32` count (truncates fractional, clamps negative).
  #[must_use]
  pub fn times_u32(&self) -> Option<u32> {
    self.times.map(f64_to_u32)
  }
}

/// Options for role-based locators (getByRole).
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct RoleOptions {
  /// Playwright: `name?: string | RegExp` — the accessible-name
  /// matcher. `JsRegExpLike` walks the prototype chain so a real JS
  /// `RegExp` passed as `name` round-trips cleanly; a plain string
  /// still works. No `{ source, flags }` wire shape is exposed.
  #[napi(ts_type = "string | RegExp")]
  pub name: Option<napi::Either<String, JsRegExpLike>>,
  /// Playwright: `description?: string | RegExp` — the accessible-description
  /// matcher (Playwright 1.60). Same prototype-chain `RegExp` handling as
  /// `name`; `exact` applies to both.
  #[napi(ts_type = "string | RegExp")]
  pub description: Option<napi::Either<String, JsRegExpLike>>,
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

/// A JS `Date` lowered to epoch milliseconds at the NAPI boundary.
///
/// napi-rs only binds `Date` through chrono types (behind a feature
/// this crate does not enable), so this reads the timestamp directly
/// via `napi_get_date_value`; `validate` gates on `napi_is_date` so an
/// `Either` union dispatches plain numbers/strings to their own arms.
#[derive(Debug, Clone, Copy)]
pub struct JsDateLike {
  pub time_ms: f64,
}

impl napi::bindgen_prelude::TypeName for JsDateLike {
  fn type_name() -> &'static str {
    "Date"
  }

  fn value_type() -> napi::ValueType {
    napi::ValueType::Object
  }
}

#[expect(unsafe_code, reason = "napi-rs declares ValidateNapiValue::validate unsafe")]
impl napi::bindgen_prelude::ValidateNapiValue for JsDateLike {
  unsafe fn validate(env: napi::sys::napi_env, napi_val: napi::sys::napi_value) -> napi::Result<napi::sys::napi_value> {
    let mut is_date = false;
    // SAFETY: the `env` and `napi_val` napi-rs handed this conversion are live
    // for the current call; `is_date` is a writable out-parameter.
    let status = unsafe { napi::sys::napi_is_date(env, napi_val, &raw mut is_date) };
    napi::check_status!(status)?;
    if !is_date {
      return Err(napi::Error::new(
        napi::Status::InvalidArg,
        "Expected a Date object".to_owned(),
      ));
    }
    Ok(std::ptr::null_mut())
  }
}

#[expect(unsafe_code, reason = "napi-rs declares FromNapiValue::from_napi_value unsafe")]
impl napi::bindgen_prelude::FromNapiValue for JsDateLike {
  unsafe fn from_napi_value(env: napi::sys::napi_env, napi_val: napi::sys::napi_value) -> napi::Result<Self> {
    let mut time_ms = 0f64;
    // SAFETY: the `env` and `napi_val` napi-rs handed this conversion are live
    // for the current call; `time_ms` is a writable out-parameter. A value
    // that is not a `Date` comes back as `napi_date_expected`.
    let status = unsafe { napi::sys::napi_get_date_value(env, napi_val, &raw mut time_ms) };
    napi::check_status!(status)?;
    Ok(Self { time_ms })
  }
}

#[expect(unsafe_code, reason = "napi-rs declares ToNapiValue::to_napi_value unsafe")]
impl napi::bindgen_prelude::ToNapiValue for JsDateLike {
  unsafe fn to_napi_value(env: napi::sys::napi_env, val: Self) -> napi::Result<napi::sys::napi_value> {
    let mut ptr = std::ptr::null_mut();
    // SAFETY: `env` is the live environment napi-rs handed this conversion;
    // `ptr` is a writable out-parameter.
    let status = unsafe { napi::sys::napi_create_date(env, val.time_ms, &raw mut ptr) };
    napi::check_status!(status)?;
    Ok(ptr)
  }
}

/// Locator-shaped input: anything with a `.selector` string accessor.
///
/// Same prototype-chain trick as [`JsRegExpLike`] — `napi_get_named_property`
/// walks the prototype chain and fires getters, so a real NAPI `Locator`
/// class instance (which exposes `.selector` via `#[napi(getter)]`)
/// deserializes cleanly into this struct without any client-side
/// unwrapping. Callers can also pass a plain `{ selector: '...' }`
/// object if they already have the raw selector string.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct LocatorRef {
  pub selector: String,
}

/// Options for filtering locators. Mirrors Playwright's `LocatorOptions`:
/// `hasText`, `hasNotText`, `has` (inner `Locator`), `hasNot` (inner
/// `Locator`), `visible` (boolean).
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct FilterOptions {
  /// Playwright: `hasText?: string | RegExp`. Same prototype-chain
  /// `RegExp` handling as [`RoleOptions::name`].
  #[napi(ts_type = "string | RegExp")]
  pub has_text: Option<napi::Either<String, JsRegExpLike>>,
  /// Playwright: `hasNotText?: string | RegExp`.
  #[napi(ts_type = "string | RegExp")]
  pub has_not_text: Option<napi::Either<String, JsRegExpLike>>,
  pub has: Option<LocatorRef>,
  pub has_not: Option<LocatorRef>,
  pub visible: Option<bool>,
}

/// Options for waiting operations.
#[napi(object, object_to_js = false)]
#[derive(Debug, Clone, Default)]
pub struct WaitOptions {
  /// "visible", "hidden", "attached", "stable"
  pub state: Option<String>,
  /// More than one match is an error rather than the first being taken.
  pub strict: Option<bool>,
  pub timeout: Option<f64>,
  #[napi(ts_type = "AbortSignal")]
  pub signal: Option<crate::abort::Signal>,
}

/// Playwright's `{ timeout?: number }` bag for
/// `elementHandle.waitForElementState` / `elementHandle.waitForSelector`.
#[napi(object, object_to_js = false)]
#[derive(Debug, Clone, Default)]
pub struct ElementHandleWaitOptions {
  pub timeout: Option<f64>,
  #[napi(ts_type = "AbortSignal")]
  pub signal: Option<crate::abort::Signal>,
}

/// Playwright's `geolocation` bag for `browserContext.setGeolocation`.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct GeolocationOptions {
  pub latitude: f64,
  pub longitude: f64,
  pub accuracy: Option<f64>,
}

/// Playwright `PageWaitForFunctionOptions` — matches
/// `{ polling?: number | 'raf', timeout?: number }`.
#[napi(object, object_to_js = false)]
#[derive(Debug, Clone, Default)]
pub struct WaitForFunctionOptions {
  #[napi(ts_type = "number | 'raf'")]
  pub polling: Option<napi::Either<f64, String>>,
  pub timeout: Option<f64>,
  #[napi(ts_type = "AbortSignal")]
  pub signal: Option<crate::abort::Signal>,
}

impl TryFrom<WaitForFunctionOptions> for ferridriver::options::WaitForFunctionOptions {
  type Error = napi::Error;

  fn try_from(o: WaitForFunctionOptions) -> std::result::Result<Self, napi::Error> {
    let polling = match o.polling {
      None => None,
      Some(napi::Either::A(ms)) => Some(ferridriver::options::Polling::Interval(f64_to_u64(ms))),
      Some(napi::Either::B(s)) if s == "raf" => Some(ferridriver::options::Polling::Raf),
      Some(napi::Either::B(s)) => {
        return Err(napi::Error::from_reason(format!("Unknown polling option: {s}")));
      },
    };
    Ok(Self {
      polling,
      timeout: o.timeout.map(f64_to_u64),
    })
  }
}

/// Playwright `locator.evaluate`'s `TimeoutOptions & EvaluateOptions`.
#[napi(object, object_to_js = false)]
#[derive(Debug, Clone, Default)]
pub struct EvaluateOptions {
  pub timeout: Option<f64>,
  #[napi(ts_type = "AbortSignal")]
  pub signal: Option<crate::abort::Signal>,
  pub expose_functions: Option<bool>,
}

impl From<EvaluateOptions> for ferridriver::options::EvaluateOptions {
  fn from(o: EvaluateOptions) -> Self {
    Self {
      timeout: o.timeout.map(f64_to_u64),
    }
  }
}

/// Pixel rectangle for [`ScreenshotOptions::clip`]. All values are in
/// CSS pixels relative to the viewport (or the full-page bounds when
/// `fullPage` is also set).
#[napi(object)]
#[derive(Debug, Clone, Copy)]
pub struct ClipRect {
  pub x: f64,
  pub y: f64,
  pub width: f64,
  pub height: f64,
}

impl From<ClipRect> for ferridriver::options::ClipRect {
  fn from(c: ClipRect) -> Self {
    Self {
      x: c.x,
      y: c.y,
      width: c.width,
      height: c.height,
    }
  }
}

/// Playwright-parity `PageScreenshotOptions`. Mirrors
/// `/tmp/playwright/packages/playwright-core/types/types.d.ts:23280`.
#[napi(object, object_to_js = false)]
#[derive(Debug, Clone, Default)]
pub struct ScreenshotOptions {
  #[napi(ts_type = "'disabled' | 'allow'")]
  pub animations: Option<String>,
  #[napi(ts_type = "'hide' | 'initial'")]
  pub caret: Option<String>,
  pub clip: Option<ClipRect>,
  pub full_page: Option<bool>,
  /// Screenshot image type — Playwright calls this `type` in TS, but
  /// `type` is reserved in Rust; we rename to `format` internally and
  /// expose as `type` in the generated `.d.ts` to stay byte-for-byte
  /// identical with Playwright.
  #[napi(ts_type = "'png' | 'jpeg' | 'webp'", js_name = "type")]
  pub format: Option<String>,
  /// Locators whose matches are painted over with [`Self::mask_color`].
  /// Playwright takes `Array<Locator>`; each `Locator` instance lowers
  /// to its `.selector` string at the NAPI boundary via the
  /// prototype-chain read on [`LocatorRef`]. `ts_type` forces the
  /// generated `.d.ts` to show `Array<Locator>` rather than leaking
  /// the `{ selector }` wire shape.
  #[napi(ts_type = "Array<Locator>")]
  pub mask: Option<Vec<LocatorRef>>,
  /// CSS color for the mask overlay. Default `#FF00FF`.
  pub mask_color: Option<String>,
  pub omit_background: Option<bool>,
  /// If set, the captured bytes are additionally written to disk.
  pub path: Option<String>,
  pub quality: Option<i32>,
  #[napi(ts_type = "'css' | 'device'")]
  pub scale: Option<String>,
  /// Raw CSS injected before capture and removed afterwards.
  pub style: Option<String>,
  pub timeout: Option<f64>,
  #[napi(ts_type = "AbortSignal")]
  pub signal: Option<crate::abort::Signal>,
}

/// Playwright-parity `LocatorScreenshotOptions` /
/// `ElementHandleScreenshotOptions`: [`ScreenshotOptions`] without
/// `clip` and `fullPage`.
#[napi(object, object_to_js = false)]
#[derive(Debug, Clone, Default)]
pub struct ElementScreenshotOptions {
  #[napi(ts_type = "'disabled' | 'allow'")]
  pub animations: Option<String>,
  #[napi(ts_type = "'hide' | 'initial'")]
  pub caret: Option<String>,
  #[napi(ts_type = "'png' | 'jpeg' | 'webp'", js_name = "type")]
  pub format: Option<String>,
  #[napi(ts_type = "Array<Locator>")]
  pub mask: Option<Vec<LocatorRef>>,
  pub mask_color: Option<String>,
  pub omit_background: Option<bool>,
  pub path: Option<String>,
  pub quality: Option<i32>,
  #[napi(ts_type = "'css' | 'device'")]
  pub scale: Option<String>,
  pub style: Option<String>,
  pub timeout: Option<f64>,
  #[napi(ts_type = "AbortSignal")]
  pub signal: Option<crate::abort::Signal>,
}

impl ElementScreenshotOptions {
  /// Lower to core options, materialising `mask` against `page`.
  #[must_use]
  pub fn into_core(self, page: &std::sync::Arc<ferridriver::Page>) -> ferridriver::options::ElementScreenshotOptions {
    ferridriver::options::ElementScreenshotOptions {
      animations: self
        .animations
        .as_deref()
        .map(ferridriver::options::AnimationsMode::from),
      caret: self.caret.as_deref().map(ferridriver::options::CaretMode::from),
      format: self.format.as_deref().map(ferridriver::options::ScreenshotFormat::from),
      mask: self
        .mask
        .unwrap_or_default()
        .iter()
        .map(|l| page.locator(&l.selector))
        .collect(),
      mask_color: self.mask_color,
      omit_background: self.omit_background,
      path: self.path.map(std::path::PathBuf::from),
      quality: self.quality.map(i64::from),
      scale: self.scale.as_deref().map(ferridriver::options::ScreenshotScale::from),
      style: self.style,
      timeout: self.timeout.map(f64_to_u64),
    }
  }
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
  /// `SameSite` attribute: "Strict", "Lax", or "None".
  pub same_site: Option<String>,
  /// Playwright `SetNetworkCookieParam.url` — when set, the browser
  /// derives domain/path from it. Absent on cookies read back.
  pub url: Option<String>,
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

impl From<ferridriver::BoundingBox> for BoundingBox {
  fn from(b: ferridriver::BoundingBox) -> Self {
    Self {
      x: b.x,
      y: b.y,
      width: b.width,
      height: b.height,
    }
  }
}

/// 2-D point, in CSS pixels. Used for drag-and-drop `sourcePosition` /
/// `targetPosition` relative to an element's padding-box top-left.
#[napi(object)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Point {
  pub x: f64,
  pub y: f64,
}

impl From<Point> for ferridriver::options::Point {
  fn from(p: Point) -> Self {
    Self { x: p.x, y: p.y }
  }
}

// ── Conversion helpers ────────────────────────────────────────────────────

impl From<RoleOptions> for ferridriver::options::RoleOptions {
  fn from(o: RoleOptions) -> Self {
    Self {
      name: o.name.map(getby_input_to_rust),
      description: o.description.map(getby_input_to_rust),
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
      has_text: o.has_text.map(getby_input_to_rust),
      has_not_text: o.has_not_text.map(getby_input_to_rust),
      has: o.has.map(|r| ferridriver::options::LocatorLike::Selector(r.selector)),
      has_not: o
        .has_not
        .map(|r| ferridriver::options::LocatorLike::Selector(r.selector)),
      visible: o.visible,
    }
  }
}

impl TryFrom<WaitOptions> for ferridriver::options::WaitOptions {
  type Error = napi::Error;

  fn try_from(o: WaitOptions) -> std::result::Result<Self, napi::Error> {
    Ok(Self {
      state: o
        .state
        .as_deref()
        .map(|s| {
          ferridriver::options::WaitState::try_from_str(s)
            .map_err(|bad| napi::Error::from_reason(format!("unknown wait state: {bad}")))
        })
        .transpose()?,
      timeout: o.timeout.map(f64_to_u64),
      strict: o.strict,
    })
  }
}

impl From<ScreenshotOptions> for ferridriver::options::ScreenshotOptions {
  fn from(o: ScreenshotOptions) -> Self {
    Self {
      animations: o.animations.as_deref().map(ferridriver::options::AnimationsMode::from),
      caret: o.caret.as_deref().map(ferridriver::options::CaretMode::from),
      clip: o.clip.map(Into::into),
      full_page: o.full_page,
      format: o.format.as_deref().map(ferridriver::options::ScreenshotFormat::from),
      // `mask` needs a `Page` to materialise core `Locator` values; the
      // caller (`Page::screenshot`) fills it from `o.mask` selectors.
      mask: Vec::new(),
      mask_color: o.mask_color,
      omit_background: o.omit_background,
      path: o.path.map(std::path::PathBuf::from),
      quality: o.quality.map(i64::from),
      scale: o.scale.as_deref().map(ferridriver::options::ScreenshotScale::from),
      style: o.style,
      timeout: o.timeout.map(f64_to_u64),
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
      url: o.url.clone(),
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
      url: o.url.clone(),
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
/// methods (predicate needs a `ThreadsafeFunction`; `URLPattern` is Node
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

/// Lower a JS `string | RegExp` into [`ferridriver::options::StringOrRegex`]
/// for every `getBy*` matcher and `RoleOptions.name`. Mirrors the
/// Playwright TS union byte-for-byte via `JsRegExpLike`'s prototype-chain
/// trick — no `{ source, flags }` wire shape ever leaks to the user.
pub(crate) fn getby_input_to_rust(input: napi::Either<String, JsRegExpLike>) -> ferridriver::options::StringOrRegex {
  match input {
    napi::Either::A(s) => ferridriver::options::StringOrRegex::String(s),
    napi::Either::B(re) => ferridriver::options::StringOrRegex::Regex {
      source: re.source,
      flags: re.flags.unwrap_or_default(),
    },
  }
}

/// Lower `Locator.highlight`'s `style` option (`string | Record<string,
/// string | number>`) into [`ferridriver::options::HighlightStyle`]. A
/// string becomes `Css`; an object becomes `Object` with each value
/// rendered to its CSS text (numbers stringified, strings verbatim).
/// Non-string/non-object values (or non-string/number record values) are
/// ignored, matching how Playwright's `cssObjectToString` template-literals
/// whatever the value is.
pub(crate) fn highlight_style_to_rust(value: serde_json::Value) -> Option<ferridriver::options::HighlightStyle> {
  match value {
    serde_json::Value::String(s) => Some(ferridriver::options::HighlightStyle::Css(s)),
    serde_json::Value::Object(map) => {
      let entries = map
        .into_iter()
        .filter_map(|(k, v)| {
          let rendered = match v {
            serde_json::Value::String(s) => s,
            serde_json::Value::Number(n) => n.to_string(),
            _ => return None,
          };
          Some((k, rendered))
        })
        .collect::<Vec<_>>();
      Some(ferridriver::options::HighlightStyle::Object(entries))
    },
    _ => None,
  }
}

/// `addInitScript` first-argument shape at the NAPI boundary.
///
/// Mirrors Playwright's `Function | string | { path?: string, content?: string }`
/// union from `/tmp/playwright/packages/playwright-core/src/client/page.ts:520`.
/// We don't lower into [`ferridriver::options::InitScriptSource`] inline in a
/// `#[napi]` async fn because `napi::Unknown<'_>` is `!Send` and the generated
/// future would fail the runtime's `Send` bound. Instead the
/// [`napi::bindgen_prelude::FromNapiValue`] impl here runs during the
/// synchronous NAPI→Rust unmarshal (where holding the JS scope is fine) and
/// produces an owned, `Send`-safe value before the async body starts.
#[derive(Debug, Clone)]
pub enum NapiInitScript {
  /// JS function — we captured its `.toString()` here so core can compose
  /// `(body)(arg)` without touching the JS engine.
  Function(String),
  /// Bare source string, used verbatim.
  Source(String),
  /// `{ content: "…" }` — content wins over `path` if both are present.
  Content(String),
  /// `{ path: "…" }` — core reads the file.
  Path(String),
}

impl From<NapiInitScript> for ferridriver::options::InitScriptSource {
  fn from(s: NapiInitScript) -> Self {
    match s {
      NapiInitScript::Function(body) => Self::Function { body },
      NapiInitScript::Source(s) => Self::Source(s),
      NapiInitScript::Content(c) => Self::Content(c),
      NapiInitScript::Path(p) => Self::Path(p.into()),
    }
  }
}

impl napi::bindgen_prelude::TypeName for NapiInitScript {
  fn type_name() -> &'static str {
    "Function | string | { path?, content? }"
  }
  fn value_type() -> napi::ValueType {
    napi::ValueType::Unknown
  }
}

impl napi::bindgen_prelude::ValidateNapiValue for NapiInitScript {}

#[expect(unsafe_code, reason = "napi-rs declares FromNapiValue::from_napi_value unsafe")]
impl napi::bindgen_prelude::FromNapiValue for NapiInitScript {
  unsafe fn from_napi_value(env: napi::sys::napi_env, napi_val: napi::sys::napi_value) -> napi::Result<Self> {
    use napi::JsValue;
    // SAFETY: the `env` and `napi_val` napi-rs handed this conversion, live for
    // the current call.
    let unknown = unsafe { napi::Unknown::from_raw_unchecked(env, napi_val) };
    match unknown.get_type()? {
      napi::ValueType::Function => {
        // `.toString()` on a JS function gives its source text — same
        // primitive Playwright's client uses via `fun.toString()`.
        let s = unknown.coerce_to_string()?.into_utf8()?.into_owned()?;
        Ok(Self::Function(s))
      },
      napi::ValueType::String => {
        let s = unknown.coerce_to_string()?.into_utf8()?.into_owned()?;
        Ok(Self::Source(s))
      },
      napi::ValueType::Object => {
        let obj = napi::bindgen_prelude::Object::from_raw(env, napi_val);
        // `content` wins over `path` — same precedence Playwright's
        // `evaluationScript` uses.
        if let Some(content) = obj.get::<String>("content")? {
          return Ok(Self::Content(content));
        }
        if let Some(path) = obj.get::<String>("path")? {
          return Ok(Self::Path(path));
        }
        Err(napi::Error::from_reason(
          "Either path or content property must be present",
        ))
      },
      other => Err(napi::Error::from_reason(format!(
        "addInitScript expects Function | string | {{ path?, content? }}, got {other}"
      ))),
    }
  }
}

/// `addInitScript` second-argument shape — the optional JSON-serialisable
/// `arg`. Distinguishes `null` from "not passed / undefined" because
/// Playwright's `Object.is(arg, undefined)` renders `null` as the string
/// `"null"` but absent arg as the literal `undefined`; the vanilla
/// `Option<serde_json::Value>` unmarshal collapses both JS `null` and
/// `undefined` to `None`, which would silently change semantics for the
/// `page.addInitScript(fn, null)` case.
pub struct NapiInitScriptArg {
  /// What a plain init script receives: `JSON.stringify(arg)`, as
  /// Playwright's `evaluationScript` inlines it.
  json: Option<serde_json::Value>,
  /// The argument with its functions bound, for `exposeFunctions`.
  exposed: crate::evaluate_arg::NapiEvaluateArg,
}

impl NapiInitScriptArg {
  /// The argument a plain init script receives.
  pub fn into_json(self) -> Option<serde_json::Value> {
    self.json
  }

  /// The argument an init script with `exposeFunctions` receives.
  pub fn into_exposed(self) -> napi::Result<ferridriver::protocol::SerializedArgument> {
    self.exposed.into_argument(true)
  }
}

impl napi::bindgen_prelude::TypeName for NapiInitScriptArg {
  fn type_name() -> &'static str {
    "any"
  }
  fn value_type() -> napi::ValueType {
    napi::ValueType::Unknown
  }
}

/// Options for `page.snapshotForAI(options?)`.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct SnapshotForAiOptions {
  /// Maximum traversal depth. `None` or negative = unlimited.
  pub depth: Option<i32>,
  /// Track key for incremental snapshots — pass the same key across
  /// calls to receive the `incremental` field with only changed
  /// nodes since the last call with this key.
  pub track: Option<String>,
}

impl From<SnapshotForAiOptions> for ferridriver::snapshot::SnapshotOptions {
  fn from(o: SnapshotForAiOptions) -> Self {
    Self {
      depth: o.depth,
      track: o.track,
    }
  }
}

/// Options for `locator.ariaSnapshot(options?)`. Mirrors Playwright's
/// `TimeoutOptions & { mode?: 'ai' | 'default', depth?: number }`.
#[napi(object, object_to_js = false)]
#[derive(Debug, Clone, Default)]
pub struct AriaSnapshotOptions {
  /// `'default'` (Playwright stable YAML) or `'ai'` (ref-annotated).
  pub mode: Option<String>,
  /// Subtree depth limit. `None` = unlimited.
  pub depth: Option<i32>,
  /// Append each element's bounding box as `[box=x,y,width,height]`
  /// (Playwright 1.60).
  pub boxes: Option<bool>,
  /// Resolution/actionability timeout in ms. `None` = page default.
  pub timeout: Option<f64>,
  #[napi(ts_type = "AbortSignal")]
  pub signal: Option<crate::abort::Signal>,
}

impl From<AriaSnapshotOptions> for ferridriver::options::AriaSnapshotOptions {
  fn from(o: AriaSnapshotOptions) -> Self {
    Self {
      mode: Some(ferridriver::options::AriaSnapshotMode::from_opt_str(o.mode.as_deref())),
      depth: o.depth,
      boxes: o.boxes,
      timeout: o.timeout.map(crate::types::f64_to_u64),
    }
  }
}

/// Filter options for `context.clearCookies(options?)`. Playwright:
/// `{ name?: string | RegExp, domain?: string | RegExp, path?: string |
/// RegExp }` — a string filters by exact match, a `RegExp` `.test()`s
/// the field. `JsRegExpLike` reads a real JS `RegExp` via its prototype
/// getters, so no `{ source, flags }` wire shape leaks.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct ClearCookieOptions {
  #[napi(ts_type = "string | RegExp")]
  pub name: Option<napi::Either<String, JsRegExpLike>>,
  #[napi(ts_type = "string | RegExp")]
  pub domain: Option<napi::Either<String, JsRegExpLike>>,
  #[napi(ts_type = "string | RegExp")]
  pub path: Option<napi::Either<String, JsRegExpLike>>,
}

impl From<ClearCookieOptions> for ferridriver::backend::ClearCookieOptions {
  fn from(o: ClearCookieOptions) -> Self {
    Self {
      name: o.name.map(getby_input_to_rust),
      domain: o.domain.map(getby_input_to_rust),
      path: o.path.map(getby_input_to_rust),
    }
  }
}

impl napi::bindgen_prelude::ValidateNapiValue for NapiInitScriptArg {}

#[expect(unsafe_code, reason = "napi-rs declares FromNapiValue::from_napi_value unsafe")]
impl napi::bindgen_prelude::FromNapiValue for NapiInitScriptArg {
  unsafe fn from_napi_value(env: napi::sys::napi_env, napi_val: napi::sys::napi_value) -> napi::Result<Self> {
    // SAFETY: the `env` and `napi_val` napi-rs handed this conversion, live for
    // the current call; an evaluate argument accepts any JS value.
    let exposed = unsafe { crate::evaluate_arg::NapiEvaluateArg::from_napi_value(env, napi_val)? };
    let mut t: napi::sys::napi_valuetype = 0;
    // SAFETY: as above; `t` is a writable out-parameter.
    let status = unsafe { napi::sys::napi_typeof(env, napi_val, &raw mut t) };
    napi::check_status!(status)?;
    let json = if t == napi::sys::ValueType::napi_undefined {
      None
    } else {
      // SAFETY: the `env` and `napi_val` napi-rs handed this conversion.
      Some(unsafe { crate::evaluate_arg::json_stringify(env, napi_val)? })
    };
    Ok(Self { json, exposed })
  }
}

/// `page.content(options?)` / `frame.content(options?)`.
#[napi(object)]
pub struct ContentOptions {
  pub include_shadow: Option<bool>,
}

crate::abort::signalled!(
  AriaSnapshotOptions,
  CheckOptions,
  ClickOptions,
  DblClickOptions,
  DispatchEventOptions,
  DragAndDropOptions,
  DropOptions,
  ElementHandleWaitOptions,
  EvaluateOptions,
  ElementScreenshotOptions,
  FillOptions,
  GotoOptions,
  HoverOptions,
  PressOptions,
  ScreenshotOptions,
  SelectOptionOptions,
  SelectorReadOptions,
  SetContentOptions,
  SetInputFilesOptions,
  TapOptions,
  TimeoutOptions,
  TypeOptions,
  WaitForFunctionOptions,
  WaitForLoadStateOptions,
  WaitForNavigationOptions,
  WaitForUrlOptions,
  WaitOptions,
);
