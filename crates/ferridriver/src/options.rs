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

/// Inner-locator reference for [`FilterOptions::has`] / [`FilterOptions::has_not`].
///
/// Accepts either a full [`crate::locator::Locator`] (Rust callers
/// constructing options programmatically) or a raw selector string
/// (NAPI/BDD callers that have already extracted the inner selector). Both
/// variants produce the same encoded `internal:has=` clause —
/// [`Locator`] additionally enables frame-equality checking at filter
/// construction time.
#[derive(Debug, Clone)]
pub enum LocatorLike {
  /// Full locator — preferred form for Rust callers. Enables same-page
  /// checks in [`crate::locator::Locator::filter`].
  Locator(crate::locator::Locator),
  /// Inner selector string verbatim. Used by NAPI/BDD where a full
  /// [`Locator`] cannot be materialized across the binding boundary.
  Selector(String),
}

impl LocatorLike {
  /// The selector string the filter encoder embeds into `internal:has=...`.
  #[must_use]
  pub fn as_selector(&self) -> &str {
    match self {
      Self::Locator(l) => l.selector(),
      Self::Selector(s) => s.as_str(),
    }
  }

  /// Full [`crate::locator::Locator`] if the caller supplied one, for
  /// frame-equality checks. Returns `None` for the `Selector` variant.
  #[must_use]
  pub fn as_locator(&self) -> Option<&crate::locator::Locator> {
    match self {
      Self::Locator(l) => Some(l),
      Self::Selector(_) => None,
    }
  }
}

impl From<crate::locator::Locator> for LocatorLike {
  fn from(l: crate::locator::Locator) -> Self {
    Self::Locator(l)
  }
}

impl From<String> for LocatorLike {
  fn from(s: String) -> Self {
    Self::Selector(s)
  }
}

impl From<&str> for LocatorLike {
  fn from(s: &str) -> Self {
    Self::Selector(s.to_string())
  }
}

impl From<&String> for LocatorLike {
  fn from(s: &String) -> Self {
    Self::Selector(s.clone())
  }
}

/// Script argument shape for [`crate::page::Page::add_init_script`] and
/// [`crate::context::ContextRef::add_init_script`]. Mirrors Playwright's
/// `Function | string | { path?, content? }` union from
/// `/tmp/playwright/packages/playwright-core/src/client/page.ts:520`.
///
/// The binding layer (NAPI / `QuickJS`) is responsible for the engine-local
/// step of extracting a JS function's source via `.toString()`; everything
/// else — reading file paths, JSON-serialising `arg`, composing the
/// `(body)(arg)` wrapper, the "cannot evaluate a string with arguments"
/// invariant — runs here in core.
#[derive(Debug, Clone)]
pub enum InitScriptSource {
  /// Pre-serialised JS function body (the result of `fn.toString()` on
  /// the caller's function). Rendered as `(body)(arg)` by
  /// [`evaluation_script`]; `arg` is JSON-stringified, missing `arg`
  /// renders as the literal `undefined`.
  Function { body: String },
  /// Script source code used verbatim. Passing `arg` alongside this form
  /// errors with Playwright's "Cannot evaluate a string with arguments".
  Source(String),
  /// Path to an on-disk script file. Read at [`evaluation_script`] call
  /// time; a `//# sourceURL=<path>` comment is appended per Playwright's
  /// `addSourceUrlToScript` helper. Passing `arg` alongside errors.
  Path(std::path::PathBuf),
  /// Literal script content (from the `{ content }` bag variant).
  /// Semantically equivalent to [`Self::Source`]; kept as a distinct
  /// variant so callers can route the Playwright object shape losslessly.
  /// Passing `arg` alongside errors.
  Content(String),
}

impl From<String> for InitScriptSource {
  fn from(s: String) -> Self {
    Self::Source(s)
  }
}

impl From<&str> for InitScriptSource {
  fn from(s: &str) -> Self {
    Self::Source(s.to_string())
  }
}

impl From<std::path::PathBuf> for InitScriptSource {
  fn from(p: std::path::PathBuf) -> Self {
    Self::Path(p)
  }
}

/// Lower an [`InitScriptSource`] + optional JSON argument into the
/// wire-level source string the backend receives. Mirrors Playwright's
/// client-side `evaluationScript` in
/// `/tmp/playwright/packages/playwright-core/src/client/clientHelper.ts:31`.
///
/// Semantics:
/// - [`InitScriptSource::Function`] + `arg` → `(body)(JSON.stringify(arg))`.
///   Absent `arg` renders as `undefined` per Playwright's
///   `Object.is(arg, undefined)` check. Playwright passes `null` through as
///   `"null"` — this function does the same.
/// - [`InitScriptSource::Source`] / [`InitScriptSource::Content`] →
///   the raw source; `arg.is_some()` rejects with
///   `FerriError::InvalidArgument` ("Cannot evaluate a string with
///   arguments").
/// - [`InitScriptSource::Path`] → file contents followed by
///   `//# sourceURL=<path>` (newlines in the path are stripped so the
///   pragma stays on one line). Same `arg.is_some()` rejection.
///
/// # Errors
///
/// - `arg` is `Some` while `script` is `Source`, `Content`, or `Path`.
/// - `Path` refers to a file that cannot be read.
/// - `Function` + `arg` whose JSON serialisation fails.
pub fn evaluation_script(
  script: InitScriptSource,
  arg: Option<&serde_json::Value>,
) -> Result<String, crate::error::FerriError> {
  match script {
    InitScriptSource::Function { body } => {
      let arg_str = match arg {
        None => "undefined".to_string(),
        Some(v) => serde_json::to_string(v)?,
      };
      Ok(format!("({body})({arg_str})"))
    },
    InitScriptSource::Source(s) | InitScriptSource::Content(s) => {
      if arg.is_some() {
        return Err(crate::error::FerriError::invalid_argument(
          "arg",
          "Cannot evaluate a string with arguments",
        ));
      }
      Ok(s)
    },
    InitScriptSource::Path(p) => {
      if arg.is_some() {
        return Err(crate::error::FerriError::invalid_argument(
          "arg",
          "Cannot evaluate a string with arguments",
        ));
      }
      let source = std::fs::read_to_string(&p)?;
      let safe_path = p.display().to_string().replace('\n', "");
      Ok(format!("{source}\n//# sourceURL={safe_path}"))
    },
  }
}

/// Options for filtering locators — mirrors Playwright's `LocatorOptions`
/// used by both `Locator::filter(options)` and the `Locator` constructor.
/// Every field maps directly to a corresponding injected-selector clause
/// per `/tmp/playwright/packages/playwright-core/src/client/locator.ts`:
///
/// * `has_text` → ` >> internal:has-text=<escaped>`
/// * `has_not_text` → ` >> internal:has-not-text=<escaped>`
/// * `has` → ` >> internal:has=<JSON-encoded inner selector>`
/// * `has_not` → ` >> internal:has-not=<JSON-encoded inner selector>`
/// * `visible` → ` >> visible=true|false`
#[derive(Debug, Clone, Default)]
pub struct FilterOptions {
  pub has_text: Option<String>,
  pub has_not_text: Option<String>,
  pub has: Option<LocatorLike>,
  pub has_not: Option<LocatorLike>,
  /// When `Some(true)`, narrow to visible elements only. When `Some(false)`,
  /// narrow to non-visible elements. `None` means no visibility filter.
  pub visible: Option<bool>,
}

/// Options for waiting operations.
#[derive(Debug, Clone, Default)]
pub struct WaitOptions {
  /// "visible", "hidden", "attached", "stable"
  pub state: Option<String>,
  pub timeout: Option<u64>,
}

/// Full Playwright `PageScreenshotOptions` surface — 13 fields. Mirrors
/// `/tmp/playwright/packages/playwright-core/types/types.d.ts:23280` plus
/// the `LocatorScreenshotOptions` subset for `Locator.screenshot()` (which
/// omits `full_page` and `clip` — the locator takes the screenshot of its
/// own element, not a pixel rectangle).
///
/// Semantics per field:
/// - `animations` — `"disabled"` pauses CSS/Web Animations during capture;
///   `"allow"` (default) leaves them untouched. Finite animations are
///   fast-forwarded to completion (fires `transitionend`); infinite ones
///   revert to initial state.
/// - `caret` — `"hide"` (default) hides the text caret; `"initial"`
///   keeps it visible.
/// - `clip` — pixel rectangle relative to the viewport (not full page).
///   Only meaningful for `Page::screenshot`; ignored by `Locator`.
/// - `full_page` — capture the entire scrollable page. Only meaningful
///   for `Page::screenshot`.
/// - `mask` — selectors (held as strings so NAPI/BDD can pass through
///   without materialising a full `Locator`) whose matches are overlaid
///   with `mask_color` before capture.
/// - `mask_color` — CSS color for the mask overlay. Defaults to pink
///   `#FF00FF`.
/// - `omit_background` — transparent background when `true`. Ignored for
///   `jpeg` / `jpg` (no alpha channel).
/// - `path` — if set, the captured bytes are also written to disk.
/// - `quality` — 0–100 for `jpeg` / `webp`. Ignored for `png`.
/// - `scale` — `"css"` for 1 pixel per CSS pixel; `"device"` (default)
///   for 1 pixel per device pixel (Retina captures are 2× bigger).
/// - `style` — raw CSS injected via `addStyleTag` before capture and
///   removed afterwards. Pierces shadow DOM and applies to subframes.
/// - `timeout` — max ms for the capture. `0` = no timeout.
/// - `format` — `"png"` (default), `"jpeg"`, or `"webp"`. Mirrors
///   Playwright's `type` field (renamed because `type` is reserved in
///   Rust). For CDP both `jpeg` and `webp` honour `quality`; `webp`
///   additionally supports transparency.
#[derive(Debug, Clone, Default)]
pub struct ScreenshotOptions {
  pub animations: Option<String>,
  pub caret: Option<String>,
  pub clip: Option<ClipRect>,
  pub full_page: Option<bool>,
  pub format: Option<String>,
  pub mask: Vec<String>,
  pub mask_color: Option<String>,
  pub omit_background: Option<bool>,
  pub path: Option<std::path::PathBuf>,
  pub quality: Option<i64>,
  pub scale: Option<String>,
  pub style: Option<String>,
  pub timeout: Option<u64>,
}

/// Pixel-rectangle clip for [`ScreenshotOptions::clip`]. All values are in
/// CSS pixels relative to the viewport's top-left corner.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClipRect {
  pub x: f64,
  pub y: f64,
  pub width: f64,
  pub height: f64,
}

/// Element bounding box in viewport coordinates.
#[derive(Debug, Clone, Copy)]
pub struct BoundingBox {
  pub x: f64,
  pub y: f64,
  pub width: f64,
  pub height: f64,
}

/// A 2D point, relative to the top-left corner of an element's padding box
/// (when used as [`DragAndDropOptions::source_position`] or
/// [`DragAndDropOptions::target_position`]), or in viewport coordinates in
/// other contexts. Mirrors Playwright's `{ x: number; y: number }` point
/// object used by `sourcePosition`, `targetPosition`, and click `position`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Point {
  pub x: f64,
  pub y: f64,
}

/// Mouse button for click/dblclick/mousedown/mouseup. Mirrors Playwright's
/// `"left" | "right" | "middle"` union per
/// `/tmp/playwright/packages/playwright-core/types/types.d.ts:12990`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MouseButton {
  /// Primary button (default).
  #[default]
  Left,
  /// Context-menu button.
  Right,
  /// Scroll wheel button.
  Middle,
}

impl MouseButton {
  /// CDP `Input.dispatchMouseEvent.button` wire value.
  #[must_use]
  pub fn as_cdp(self) -> &'static str {
    match self {
      Self::Left => "left",
      Self::Right => "right",
      Self::Middle => "middle",
    }
  }

  /// `BiDi` `input.performActions.pointerDown.button` integer: `0=left`,
  /// `1=middle`, `2=right` (per W3C `WebDriver BiDi` pointer input spec).
  #[must_use]
  pub fn as_bidi(self) -> u8 {
    match self {
      Self::Left => 0,
      Self::Middle => 1,
      Self::Right => 2,
    }
  }

  /// `WebKit` host IPC `OP_MOUSE_EVENT.button` byte: `0=left, 1=right,
  /// 2=middle` — the order `host.m`'s `NSEventType*Mouse*` switches on
  /// (see `rightMouseDown:` vs `otherMouseDown:` dispatch). This is a
  /// different ordering than the CDP / `BiDi` pointer spec, so we keep a
  /// dedicated accessor to make the mapping explicit at every call site.
  #[must_use]
  pub fn as_webkit(self) -> u8 {
    match self {
      Self::Left => 0,
      Self::Right => 1,
      Self::Middle => 2,
    }
  }

  /// Parse from Playwright's string form. Returns `None` on unknown input
  /// so callers can raise a typed `FerriError::InvalidArgument` at the
  /// binding boundary.
  #[must_use]
  pub fn parse(s: &str) -> Option<Self> {
    match s {
      "left" => Some(Self::Left),
      "right" => Some(Self::Right),
      "middle" => Some(Self::Middle),
      _ => None,
    }
  }
}

/// Single keyboard modifier. Mirrors Playwright's `Alt | Control |
/// ControlOrMeta | Meta | Shift` union per
/// `/tmp/playwright/packages/playwright-core/types/types.d.ts:13012`.
/// `ControlOrMeta` resolves at call time — see [`Self::cdp_bit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modifier {
  Alt,
  Control,
  ControlOrMeta,
  Meta,
  Shift,
}

impl Modifier {
  /// Parse from Playwright's string form. Returns `None` on unknown input.
  #[must_use]
  pub fn parse(s: &str) -> Option<Self> {
    match s {
      "Alt" => Some(Self::Alt),
      "Control" => Some(Self::Control),
      "ControlOrMeta" => Some(Self::ControlOrMeta),
      "Meta" => Some(Self::Meta),
      "Shift" => Some(Self::Shift),
      _ => None,
    }
  }

  /// CDP `Input.dispatchMouseEvent.modifiers` bitmask bit.
  /// `Alt=1`, `Control=2`, `Meta=4`, `Shift=8` (per CDP docs and
  /// `/tmp/playwright/packages/playwright-core/src/server/input.ts`).
  /// `ControlOrMeta` collapses to `Meta` on macOS, `Control` elsewhere.
  #[must_use]
  pub fn cdp_bit(self) -> u8 {
    match self {
      Self::Alt => 1,
      Self::Control => 2,
      Self::Meta => 4,
      Self::Shift => 8,
      Self::ControlOrMeta => {
        if cfg!(target_os = "macos") {
          4
        } else {
          2
        }
      },
    }
  }

  /// Platform-resolved key name for keydown/keyup events when pressing
  /// modifiers around an action. `ControlOrMeta` collapses to `Meta` on
  /// macOS, `Control` elsewhere.
  #[must_use]
  pub fn key_name(self) -> &'static str {
    match self {
      Self::Alt => "Alt",
      Self::Control => "Control",
      Self::Meta => "Meta",
      Self::Shift => "Shift",
      Self::ControlOrMeta => {
        if cfg!(target_os = "macos") {
          "Meta"
        } else {
          "Control"
        }
      },
    }
  }

  /// DOM `KeyboardEvent.code` for this modifier's left variant — used
  /// when we synthesize `Input.dispatchKeyEvent` in CDP to satisfy the
  /// `code` parameter. Right variants have different codes but the
  /// outward JS observability is identical.
  #[must_use]
  pub fn key_code(self) -> &'static str {
    match self {
      Self::Alt => "AltLeft",
      Self::Control => "ControlLeft",
      Self::Shift => "ShiftLeft",
      Self::Meta => "MetaLeft",
      Self::ControlOrMeta => {
        if cfg!(target_os = "macos") {
          "MetaLeft"
        } else {
          "ControlLeft"
        }
      },
    }
  }
}

/// Fold a list of modifiers into the CDP bitmask expected by
/// `Input.dispatchMouseEvent.modifiers`.
#[must_use]
pub fn modifiers_bitmask(mods: &[Modifier]) -> u32 {
  let mut m = 0u32;
  for md in mods {
    m |= u32::from(md.cdp_bit());
  }
  m
}

/// Full click option bag shared by Page/Locator/Frame click methods.
/// Mirrors Playwright's `LocatorClickOptions` /
/// `PageClickOptions` / `FrameClickOptions` — all three expose the same
/// 10-field surface per
/// `/tmp/playwright/packages/playwright-core/types/types.d.ts:12986`.
///
/// Every option is `Option<T>`: callers omit fields and the backend
/// applies Playwright's documented defaults (`button: Left`,
/// `click_count: 1`, `delay: 0`, `steps: 1`, etc.).
#[derive(Debug, Clone, Default)]
pub struct ClickOptions {
  /// Mouse button. Default: [`MouseButton::Left`].
  pub button: Option<MouseButton>,
  /// Number of consecutive clicks (`UIEvent.detail`). Default: `1`.
  pub click_count: Option<u32>,
  /// Wait in ms between `mousedown` and `mouseup`. Default: `0`.
  pub delay: Option<u64>,
  /// Bypass actionability (visibility/attached/enabled/stable) checks.
  /// Default: `false`.
  pub force: Option<bool>,
  /// Modifier keys held during the click. Pressed before the mouse
  /// events and released after, regardless of `trial`.
  pub modifiers: Vec<Modifier>,
  /// Deprecated per Playwright — accepted for signature parity; no
  /// effect in ferridriver (we don't implicitly wait for navigation
  /// after click).
  pub no_wait_after: Option<bool>,
  /// Click position relative to the element's padding-box top-left.
  /// `None` → element's visible center.
  pub position: Option<Point>,
  /// Interpolated `mousemove` events between the current cursor and
  /// the click point. Playwright default: `1` (single move at dest).
  pub steps: Option<u32>,
  /// Maximum time in ms for the operation (actionability + click).
  /// `0` means "no timeout". `None` means "use page/context default".
  pub timeout: Option<u64>,
  /// Run actionability checks only; skip the mouse events. Modifiers
  /// are still pressed/released around the no-op per Playwright.
  pub trial: Option<bool>,
}

impl ClickOptions {
  /// [`Self::button`] with the `Left` default applied.
  #[must_use]
  pub fn resolved_button(&self) -> MouseButton {
    self.button.unwrap_or(MouseButton::Left)
  }

  /// [`Self::click_count`] with the `1` default applied.
  #[must_use]
  pub fn resolved_click_count(&self) -> u32 {
    self.click_count.unwrap_or(1)
  }

  /// [`Self::delay`] with the `0ms` default applied.
  #[must_use]
  pub fn resolved_delay_ms(&self) -> u64 {
    self.delay.unwrap_or(0)
  }

  /// [`Self::steps`] with the `1` default applied.
  #[must_use]
  pub fn resolved_steps(&self) -> u32 {
    self.steps.unwrap_or(1).max(1)
  }

  /// `true` when the caller asked to bypass actionability checks.
  #[must_use]
  pub fn is_force(&self) -> bool {
    self.force.unwrap_or(false)
  }

  /// `true` when the caller asked to run checks only (no click).
  #[must_use]
  pub fn is_trial(&self) -> bool {
    self.trial.unwrap_or(false)
  }
}

/// Options for [`crate::page::Page::drag_and_drop`] and
/// [`crate::locator::Locator::drag_to`]. Mirrors Playwright's
/// `FrameDragAndDropOptions & TimeoutOptions` surface per
/// `/tmp/playwright/packages/playwright-core/types/types.d.ts` (public
/// `page.dragAndDrop` / `locator.dragTo` signatures) and the internal
/// `FrameDragAndDropOptions` type at
/// `/tmp/playwright/packages/protocol/src/channels.d.ts:3012`.
///
/// `strict` is meaningful only on [`crate::page::Page::drag_and_drop`] (which
/// accepts bare selectors); [`crate::locator::Locator::drag_to`] ignores it
/// because the source locator already carries its own strict flag.
///
/// `no_wait_after` is accepted for Playwright signature parity but has no
/// effect (deprecated in upstream, tagged `@deprecated This option has no
/// effect.`).
#[derive(Debug, Clone, Default)]
pub struct DragAndDropOptions {
  /// Bypass actionability checks.
  pub force: Option<bool>,
  /// Deprecated — no effect. Accepted for signature parity.
  pub no_wait_after: Option<bool>,
  /// Press point relative to the source element's padding-box top-left.
  /// When absent, the source element's center is used.
  pub source_position: Option<Point>,
  /// Release point relative to the target element's padding-box top-left.
  /// When absent, the target element's center is used.
  pub target_position: Option<Point>,
  /// Number of interpolated `mousemove` events between press and release.
  /// Playwright default is `1` (a single move at the destination).
  pub steps: Option<u32>,
  /// Strict-mode override for resolving the source/target selector.
  /// Meaningful only on `page.drag_and_drop`; ignored by `locator.drag_to`.
  pub strict: Option<bool>,
  /// Maximum time in ms. `0` means no timeout. Default is inherited from
  /// the context's default action timeout.
  pub timeout: Option<u64>,
  /// Perform actionability checks only; skip the actual mouse press/move/release.
  pub trial: Option<bool>,
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

/// Three-state override for a single media-emulation field. Mirrors the
/// Playwright TS shape `T | null | undefined`:
///
/// * [`MediaOverride::Unchanged`] — the caller omitted this field; leave
///   the page's existing override (if any) in place.
/// * [`MediaOverride::Disabled`] — the caller passed `null`; clear this
///   specific override so the page falls back to the platform default.
/// * [`MediaOverride::Set`] — the caller passed a value; apply it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum MediaOverride {
  /// Field absent from the caller's options bag.
  #[default]
  Unchanged,
  /// Field explicitly set to `null` — disables the override.
  Disabled,
  /// Field set to a concrete value.
  Set(String),
}

impl MediaOverride {
  /// Borrow the set value, or `None` for `Unchanged` / `Disabled`.
  #[must_use]
  pub fn as_value(&self) -> Option<&str> {
    match self {
      Self::Set(v) => Some(v.as_str()),
      _ => None,
    }
  }

  /// `true` when the caller is overriding the field (set-or-disable).
  #[must_use]
  pub fn is_specified(&self) -> bool {
    !matches!(self, Self::Unchanged)
  }
}

impl From<Option<String>> for MediaOverride {
  fn from(o: Option<String>) -> Self {
    o.map_or(Self::Unchanged, Self::Set)
  }
}

/// Media emulation options — matches Playwright's `page.emulateMedia()` per
/// `/tmp/playwright/packages/playwright-core/types/types.d.ts:2580`.
/// Each field uses [`MediaOverride`] to distinguish *unspecified* (leave
/// current state alone) from *null* (clear any existing override) from a
/// *concrete value*.
#[derive(Debug, Clone, Default)]
pub struct EmulateMediaOptions {
  /// CSS media type: `"screen"` or `"print"`.
  pub media: MediaOverride,
  /// Prefers-color-scheme: `"light"`, `"dark"`, or `"no-preference"`.
  pub color_scheme: MediaOverride,
  /// Prefers-reduced-motion: `"reduce"` or `"no-preference"`.
  pub reduced_motion: MediaOverride,
  /// Forced-colors: `"active"` or `"none"`.
  pub forced_colors: MediaOverride,
  /// Prefers-contrast: `"more"`, `"less"`, or `"no-preference"`.
  pub contrast: MediaOverride,
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

/// Selector for [`crate::Page::frame`]. Mirrors Playwright's
/// `page.frame(frameSelector)` union type
/// `string | { name?: string; url?: string|RegExp|URLPattern|(url => bool) }`
/// (`/tmp/playwright/packages/playwright-core/types/types.d.ts:2755`).
///
/// For ferridriver 3.8 we accept the string form + `{ name, url }` with
/// both fields being plain strings (exact match). Task **3.12** extends
/// `url` to the full `StringOrRegex` matcher; matching rules will remain
/// behind this struct so callers don't rebind.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrameSelector {
  /// Match against the frame's `name` attribute (exact).
  pub name: Option<String>,
  /// Match against the frame's URL (exact).
  pub url: Option<String>,
}

impl FrameSelector {
  /// Convenience: selector that matches by frame name.
  #[must_use]
  pub fn by_name(name: impl Into<String>) -> Self {
    Self {
      name: Some(name.into()),
      url: None,
    }
  }

  /// Convenience: selector that matches by frame URL.
  #[must_use]
  pub fn by_url(url: impl Into<String>) -> Self {
    Self {
      name: None,
      url: Some(url.into()),
    }
  }

  /// Returns `true` when neither `name` nor `url` is set — Playwright's
  /// `assert(name || url, 'Either name or url matcher should be specified')`.
  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.name.is_none() && self.url.is_none()
  }
}

impl From<&str> for FrameSelector {
  fn from(name: &str) -> Self {
    Self::by_name(name)
  }
}

impl From<String> for FrameSelector {
  fn from(name: String) -> Self {
    Self::by_name(name)
  }
}

impl From<&String> for FrameSelector {
  fn from(name: &String) -> Self {
    Self::by_name(name.clone())
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

#[cfg(test)]
mod media_override_tests {
  use super::*;

  #[test]
  fn default_is_unchanged() {
    let o: MediaOverride = MediaOverride::default();
    assert_eq!(o, MediaOverride::Unchanged);
    assert!(!o.is_specified());
    assert_eq!(o.as_value(), None);
  }

  #[test]
  fn set_reports_value() {
    let o = MediaOverride::Set("dark".into());
    assert!(o.is_specified());
    assert_eq!(o.as_value(), Some("dark"));
  }

  #[test]
  fn disabled_is_specified_without_value() {
    let o = MediaOverride::Disabled;
    assert!(o.is_specified());
    assert_eq!(o.as_value(), None);
  }

  #[test]
  fn from_option_string_maps_some_to_set_and_none_to_unchanged() {
    let set: MediaOverride = Some("dark".to_string()).into();
    assert_eq!(set, MediaOverride::Set("dark".into()));
    let unch: MediaOverride = None.into();
    assert_eq!(unch, MediaOverride::Unchanged);
  }

  #[test]
  fn default_emulate_media_is_all_unchanged() {
    let o = EmulateMediaOptions::default();
    assert_eq!(o.media, MediaOverride::Unchanged);
    assert_eq!(o.color_scheme, MediaOverride::Unchanged);
    assert_eq!(o.reduced_motion, MediaOverride::Unchanged);
    assert_eq!(o.forced_colors, MediaOverride::Unchanged);
    assert_eq!(o.contrast, MediaOverride::Unchanged);
  }
}

#[cfg(test)]
mod drag_option_tests {
  use super::*;

  #[test]
  fn default_drag_options_has_no_overrides() {
    let opts = DragAndDropOptions::default();
    assert!(opts.force.is_none());
    assert!(opts.no_wait_after.is_none());
    assert!(opts.source_position.is_none());
    assert!(opts.target_position.is_none());
    assert!(opts.steps.is_none());
    assert!(opts.strict.is_none());
    assert!(opts.timeout.is_none());
    assert!(opts.trial.is_none());
  }

  #[test]
  fn drag_options_carry_every_field() {
    let opts = DragAndDropOptions {
      force: Some(true),
      no_wait_after: Some(false),
      source_position: Some(Point { x: 10.0, y: 20.0 }),
      target_position: Some(Point { x: 30.0, y: 40.0 }),
      steps: Some(5),
      strict: Some(true),
      timeout: Some(2_000),
      trial: Some(true),
    };
    assert_eq!(opts.force, Some(true));
    assert_eq!(opts.no_wait_after, Some(false));
    assert_eq!(opts.source_position, Some(Point { x: 10.0, y: 20.0 }));
    assert_eq!(opts.target_position, Some(Point { x: 30.0, y: 40.0 }));
    assert_eq!(opts.steps, Some(5));
    assert_eq!(opts.strict, Some(true));
    assert_eq!(opts.timeout, Some(2_000));
    assert_eq!(opts.trial, Some(true));
  }

  #[test]
  fn point_default_is_origin() {
    assert_eq!(Point::default(), Point { x: 0.0, y: 0.0 });
  }
}

#[cfg(test)]
mod init_script_tests {
  use super::*;
  use serde_json::json;

  #[test]
  fn function_with_undefined_arg_renders_literal_undefined() {
    // Playwright: `Object.is(arg, undefined) ? 'undefined' : JSON.stringify(arg)`.
    let src = evaluation_script(
      InitScriptSource::Function {
        body: "(x) => x + 1".to_string(),
      },
      None,
    )
    .unwrap();
    assert_eq!(src, "((x) => x + 1)(undefined)");
  }

  #[test]
  fn function_with_null_arg_renders_literal_null() {
    // `Object.is(null, undefined)` is false — null goes through JSON.stringify.
    let src = evaluation_script(
      InitScriptSource::Function {
        body: "(x) => x".to_string(),
      },
      Some(&serde_json::Value::Null),
    )
    .unwrap();
    assert_eq!(src, "((x) => x)(null)");
  }

  #[test]
  fn function_with_object_arg_renders_json() {
    let arg = json!({ "answer": 42, "nested": [1, 2, 3] });
    let src = evaluation_script(
      InitScriptSource::Function {
        body: "function (o) { window.x = o; }".to_string(),
      },
      Some(&arg),
    )
    .unwrap();
    assert_eq!(
      src,
      r#"(function (o) { window.x = o; })({"answer":42,"nested":[1,2,3]})"#
    );
  }

  #[test]
  fn source_without_arg_passes_through_verbatim() {
    let src = evaluation_script(InitScriptSource::Source("window.x = 1".into()), None).unwrap();
    assert_eq!(src, "window.x = 1");
  }

  #[test]
  fn source_with_arg_errors() {
    let err = evaluation_script(InitScriptSource::Source("window.x = 1".into()), Some(&json!(42))).unwrap_err();
    assert!(
      err.to_string().contains("Cannot evaluate a string with arguments"),
      "unexpected error: {err}"
    );
  }

  #[test]
  fn content_with_arg_errors() {
    let err = evaluation_script(InitScriptSource::Content("1".into()), Some(&json!(0))).unwrap_err();
    assert!(err.to_string().contains("Cannot evaluate a string with arguments"));
  }

  #[test]
  fn path_with_arg_errors() {
    let err = evaluation_script(
      InitScriptSource::Path(std::path::PathBuf::from("/nope")),
      Some(&json!(0)),
    )
    .unwrap_err();
    assert!(err.to_string().contains("Cannot evaluate a string with arguments"));
  }

  #[test]
  fn path_reads_file_and_appends_source_url() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("fd-init-script-{}.js", std::process::id()));
    std::fs::write(&path, "window.__fromFile = 7;").unwrap();
    let src = evaluation_script(InitScriptSource::Path(path.clone()), None).unwrap();
    let expected = format!("window.__fromFile = 7;\n//# sourceURL={}", path.display());
    assert_eq!(src, expected);
    let _ = std::fs::remove_file(path);
  }

  #[test]
  fn path_missing_errors() {
    let missing = std::path::PathBuf::from("/definitely/not/a/real/path/x.js");
    let err = evaluation_script(InitScriptSource::Path(missing), None).unwrap_err();
    // Surfaces as Io(_) via FerriError::From<io::Error>.
    assert!(matches!(err, crate::error::FerriError::Io(_)), "unexpected: {err}");
  }

  #[test]
  fn content_passes_through_verbatim() {
    let src = evaluation_script(InitScriptSource::Content("let z = 2;".into()), None).unwrap();
    assert_eq!(src, "let z = 2;");
  }
}

#[cfg(test)]
mod click_option_tests {
  use super::*;

  #[test]
  fn mouse_button_parse_round_trip() {
    assert_eq!(MouseButton::parse("left"), Some(MouseButton::Left));
    assert_eq!(MouseButton::parse("right"), Some(MouseButton::Right));
    assert_eq!(MouseButton::parse("middle"), Some(MouseButton::Middle));
    assert_eq!(MouseButton::parse("garbage"), None);
    assert_eq!(MouseButton::Left.as_cdp(), "left");
    assert_eq!(MouseButton::Right.as_cdp(), "right");
    assert_eq!(MouseButton::Middle.as_cdp(), "middle");
    assert_eq!(MouseButton::Left.as_bidi(), 0);
    assert_eq!(MouseButton::Middle.as_bidi(), 1);
    assert_eq!(MouseButton::Right.as_bidi(), 2);
    // WebKit host.m uses a different ordering than CDP/BiDi: 0=left,
    // 1=right, 2=middle (matches its NSEventType*Mouse* switch).
    assert_eq!(MouseButton::Left.as_webkit(), 0);
    assert_eq!(MouseButton::Right.as_webkit(), 1);
    assert_eq!(MouseButton::Middle.as_webkit(), 2);
  }

  #[test]
  fn modifier_parse_and_bits() {
    assert_eq!(Modifier::parse("Alt"), Some(Modifier::Alt));
    assert_eq!(Modifier::parse("Control"), Some(Modifier::Control));
    assert_eq!(Modifier::parse("Meta"), Some(Modifier::Meta));
    assert_eq!(Modifier::parse("Shift"), Some(Modifier::Shift));
    assert_eq!(Modifier::parse("ControlOrMeta"), Some(Modifier::ControlOrMeta));
    assert_eq!(Modifier::parse("garbage"), None);

    assert_eq!(Modifier::Alt.cdp_bit(), 1);
    assert_eq!(Modifier::Control.cdp_bit(), 2);
    assert_eq!(Modifier::Meta.cdp_bit(), 4);
    assert_eq!(Modifier::Shift.cdp_bit(), 8);

    // Platform-aware ControlOrMeta
    if cfg!(target_os = "macos") {
      assert_eq!(Modifier::ControlOrMeta.cdp_bit(), 4);
      assert_eq!(Modifier::ControlOrMeta.key_name(), "Meta");
      assert_eq!(Modifier::ControlOrMeta.key_code(), "MetaLeft");
    } else {
      assert_eq!(Modifier::ControlOrMeta.cdp_bit(), 2);
      assert_eq!(Modifier::ControlOrMeta.key_name(), "Control");
      assert_eq!(Modifier::ControlOrMeta.key_code(), "ControlLeft");
    }
  }

  #[test]
  fn modifiers_bitmask_folds_multiple() {
    assert_eq!(modifiers_bitmask(&[]), 0);
    assert_eq!(modifiers_bitmask(&[Modifier::Shift]), 8);
    // Alt|Control|Meta|Shift = 1|2|4|8 = 15
    assert_eq!(
      modifiers_bitmask(&[Modifier::Alt, Modifier::Control, Modifier::Meta, Modifier::Shift]),
      15
    );
    // Dedup via bitwise OR — duplicates don't double-count.
    assert_eq!(modifiers_bitmask(&[Modifier::Shift, Modifier::Shift]), 8);
  }

  #[test]
  fn click_options_default_values() {
    let opts = ClickOptions::default();
    assert_eq!(opts.resolved_button(), MouseButton::Left);
    assert_eq!(opts.resolved_click_count(), 1);
    assert_eq!(opts.resolved_delay_ms(), 0);
    assert_eq!(opts.resolved_steps(), 1);
    assert!(!opts.is_force());
    assert!(!opts.is_trial());
    assert!(opts.modifiers.is_empty());
    assert!(opts.position.is_none());
    assert!(opts.timeout.is_none());
    assert!(opts.no_wait_after.is_none());
  }

  #[test]
  fn click_options_resolved_helpers_use_overrides() {
    let opts = ClickOptions {
      button: Some(MouseButton::Right),
      click_count: Some(2),
      delay: Some(150),
      steps: Some(5),
      force: Some(true),
      trial: Some(true),
      ..Default::default()
    };
    assert_eq!(opts.resolved_button(), MouseButton::Right);
    assert_eq!(opts.resolved_click_count(), 2);
    assert_eq!(opts.resolved_delay_ms(), 150);
    assert_eq!(opts.resolved_steps(), 5);
    assert!(opts.is_force());
    assert!(opts.is_trial());
  }

  #[test]
  fn click_options_steps_coerces_zero_to_one() {
    // Playwright defaults to 1 and uses `Math.max(1, steps)`; mirror
    // the clamp so callers passing `0` still emit one mousemove.
    let opts = ClickOptions {
      steps: Some(0),
      ..Default::default()
    };
    assert_eq!(opts.resolved_steps(), 1);
  }
}
