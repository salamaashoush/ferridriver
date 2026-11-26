//! Action, PDF, and close option structs.

use super::{ActionScroll, Modifier, MouseButton, Point};

/// Every option is `Option<T>`: callers omit fields and the backend
/// applies the documented defaults (`button: Left`, `click_count: 1`,
/// `delay: 0`, `steps: 1`, etc.).
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
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
  /// Deprecated — accepted for signature parity; no effect in
  /// ferridriver (we don't implicitly wait for navigation after click).
  pub no_wait_after: Option<bool>,
  /// Click position relative to the element's padding-box top-left.
  /// `None` → element's visible center.
  pub position: Option<Point>,
  /// Interpolated `mousemove` events between the current cursor and
  /// the click point. Default: `1` (single move at dest).
  pub steps: Option<u32>,
  /// Maximum time in ms for the operation (actionability + click).
  /// `0` means "no timeout". `None` means "use page/context default".
  pub timeout: Option<u64>,
  /// Run actionability checks only; skip the mouse events. Modifiers
  /// are still pressed/released around the no-op.
  pub trial: Option<bool>,
  /// Playwright `scroll?: 'auto' | 'none'`: `none` leaves the page where
  /// it is instead of scrolling the target into view first.
  pub scroll: Option<ActionScroll>,
}

impl ClickOptions {
  /// `false` when the caller opted out of scrolling into view.
  #[must_use]
  pub fn scrolls(&self) -> bool {
    self.scroll != Some(ActionScroll::None)
  }

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

/// Options for `fill` (set an input's value). Three fields.
/// `no_wait_after` is accepted for signature parity; `force` skips the
/// fillable / editable actionability check.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FillOptions {
  pub force: Option<bool>,
  pub no_wait_after: Option<bool>,
  pub timeout: Option<u64>,
}

impl FillOptions {
  #[must_use]
  pub fn is_force(&self) -> bool {
    self.force.unwrap_or(false)
  }
}

/// Options for `press` (single key press).
/// `LocatorPressOptions` — three fields.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PressOptions {
  /// Milliseconds to hold the key down between `keydown` and `keyup`.
  pub delay: Option<u64>,
  pub no_wait_after: Option<bool>,
  pub timeout: Option<u64>,
}

impl PressOptions {
  #[must_use]
  pub fn resolved_delay_ms(&self) -> u64 {
    self.delay.unwrap_or(0)
  }
}

/// Options for `type` / `press_sequentially` (type text character-by-
/// character). `LocatorTypeOptions` — three fields.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TypeOptions {
  /// Milliseconds between consecutive `keydown` + `keyup` pairs.
  pub delay: Option<u64>,
  pub no_wait_after: Option<bool>,
  pub timeout: Option<u64>,
}

impl TypeOptions {
  #[must_use]
  pub fn resolved_delay_ms(&self) -> u64 {
    self.delay.unwrap_or(0)
  }
}

/// Options for `check` / `uncheck` / `setChecked`.
/// `LocatorCheckOptions` / `LocatorSetCheckedOptions` — five fields.
/// Internally a check is a click on a checkbox/radio; these options
/// mirror [`ClickOptions`] minus `button`, `click_count`, `delay`,
/// `modifiers`, `steps`.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CheckOptions {
  pub force: Option<bool>,
  pub no_wait_after: Option<bool>,
  pub position: Option<Point>,
  pub timeout: Option<u64>,
  pub trial: Option<bool>,
  /// Playwright `scroll?: 'auto' | 'none'`: `none` leaves the page where
  /// it is instead of scrolling the target into view first.
  pub scroll: Option<ActionScroll>,
}

impl CheckOptions {
  #[must_use]
  pub fn is_force(&self) -> bool {
    self.force.unwrap_or(false)
  }

  #[must_use]
  pub fn is_trial(&self) -> bool {
    self.trial.unwrap_or(false)
  }

  /// Lower to [`ClickOptions`] for the shared click dispatch path.
  /// Check/uncheck/setChecked all internally click the element; the
  /// caller-facing options only cover the click-invariant subset.
  #[must_use]
  pub fn into_click_options(self) -> ClickOptions {
    ClickOptions {
      button: None,
      click_count: None,
      delay: None,
      force: self.force,
      modifiers: Vec::new(),
      no_wait_after: self.no_wait_after,
      position: self.position,
      steps: None,
      timeout: self.timeout,
      trial: self.trial,
      scroll: self.scroll,
    }
  }
}

/// A single descriptor used by `selectOption`. At least one of `value`,
/// `label`, or `index` must be set. An array of these descriptors
/// selects every `<option>` matching any descriptor (multi-select).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SelectOptionValue {
  /// What a plain string asks for: an option whose value OR label is
  /// this. Playwright sends `selectOption('x')` as `{ valueOrLabel }`
  /// (`client/elementHandle.ts` `convertSelectOptionValues`); it is not
  /// a key of the object form, so it is never read from one.
  #[serde(rename = "valueOrLabel", skip_serializing_if = "Option::is_none", skip_deserializing)]
  pub value_or_label: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub value: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub label: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub index: Option<u32>,
}

impl SelectOptionValue {
  /// Shortcut for `{ value: Some(s), ... }` — the most common form.
  #[must_use]
  pub fn by_value(s: impl Into<String>) -> Self {
    Self {
      value: Some(s.into()),
      ..Self::default()
    }
  }

  /// What a plain string selects: the option whose value or label is
  /// `s`, as Playwright's `selectOption('s')` does.
  #[must_use]
  pub fn by_value_or_label(s: impl Into<String>) -> Self {
    Self {
      value_or_label: Some(s.into()),
      ..Self::default()
    }
  }

  /// Shortcut for `{ label: Some(s), ... }` — selects by the option's
  /// visible text.
  #[must_use]
  pub fn by_label(s: impl Into<String>) -> Self {
    Self {
      label: Some(s.into()),
      ..Self::default()
    }
  }

  /// Shortcut for `{ index: Some(i), ... }`.
  #[must_use]
  pub fn by_index(i: u32) -> Self {
    Self {
      index: Some(i),
      ..Self::default()
    }
  }
}

/// Options for `selectOption`.
/// `LocatorSelectOptionOptions` — three fields.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SelectOptionOptions {
  pub force: Option<bool>,
  pub no_wait_after: Option<bool>,
  pub timeout: Option<u64>,
}

/// Options for `setInputFiles`.
/// `LocatorSetInputFilesOptions` — two fields.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SetInputFilesOptions {
  pub no_wait_after: Option<bool>,
  pub timeout: Option<u64>,
}

/// File payload for `setInputFiles`.
/// `FilePayload` — caller supplies raw bytes plus the filename and MIME
/// type that the page should see, avoiding any on-disk write.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilePayload {
  pub name: String,
  pub mime_type: String,
  pub buffer: Vec<u8>,
}

/// Input-file argument for `setInputFiles`.
/// `string | string[] | FilePayload | FilePayload[]` union from
/// `types.d.ts` under `setInputFiles`.
#[derive(Debug, Clone)]
pub enum InputFiles {
  /// Paths on disk — read and uploaded as-is.
  Paths(Vec<std::path::PathBuf>),
  /// In-memory payloads — uploaded without touching disk.
  Payloads(Vec<FilePayload>),
}

impl From<&std::path::Path> for InputFiles {
  fn from(path: &std::path::Path) -> Self {
    Self::Paths(vec![path.to_path_buf()])
  }
}

impl From<std::path::PathBuf> for InputFiles {
  fn from(path: std::path::PathBuf) -> Self {
    Self::Paths(vec![path])
  }
}

impl From<Vec<std::path::PathBuf>> for InputFiles {
  fn from(paths: Vec<std::path::PathBuf>) -> Self {
    Self::Paths(paths)
  }
}

impl From<&str> for InputFiles {
  fn from(path: &str) -> Self {
    Self::Paths(vec![std::path::PathBuf::from(path)])
  }
}

impl From<FilePayload> for InputFiles {
  fn from(payload: FilePayload) -> Self {
    Self::Payloads(vec![payload])
  }
}

impl From<Vec<FilePayload>> for InputFiles {
  fn from(payloads: Vec<FilePayload>) -> Self {
    Self::Payloads(payloads)
  }
}

/// Argument for `select_option` — anything option-descriptor-shaped
/// converts: `"value or label"`, `["a", "b"]`, a single [`SelectOptionValue`]
/// (see [`SelectOptionValue::by_label`] / [`SelectOptionValue::by_index`]),
/// or a prepared `Vec<SelectOptionValue>`.
#[derive(Debug, Clone, Default)]
pub struct SelectOptionValues(pub Vec<SelectOptionValue>);

impl From<&str> for SelectOptionValues {
  fn from(value: &str) -> Self {
    Self(vec![SelectOptionValue::by_value_or_label(value)])
  }
}

impl From<String> for SelectOptionValues {
  fn from(value: String) -> Self {
    Self(vec![SelectOptionValue::by_value_or_label(value)])
  }
}

impl From<SelectOptionValue> for SelectOptionValues {
  fn from(value: SelectOptionValue) -> Self {
    Self(vec![value])
  }
}

impl From<Vec<SelectOptionValue>> for SelectOptionValues {
  fn from(values: Vec<SelectOptionValue>) -> Self {
    Self(values)
  }
}

impl<const N: usize> From<[&str; N]> for SelectOptionValues {
  fn from(values: [&str; N]) -> Self {
    Self(values.into_iter().map(SelectOptionValue::by_value_or_label).collect())
  }
}

impl From<Vec<&str>> for SelectOptionValues {
  fn from(values: Vec<&str>) -> Self {
    Self(values.into_iter().map(SelectOptionValue::by_value_or_label).collect())
  }
}

impl From<&[&str]> for SelectOptionValues {
  fn from(values: &[&str]) -> Self {
    Self(
      values
        .iter()
        .copied()
        .map(SelectOptionValue::by_value_or_label)
        .collect(),
    )
  }
}

/// Options for `dispatchEvent`.
/// `LocatorDispatchEventOptions` — single field (`timeout`).
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DispatchEventOptions {
  pub timeout: Option<u64>,
}

/// Options for hover actions. Shape is [`ClickOptions`] minus `button`,
/// `click_count`, `delay` (no press/release — just a `mousemove` at the
/// target).
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HoverOptions {
  pub force: Option<bool>,
  pub modifiers: Vec<Modifier>,
  pub no_wait_after: Option<bool>,
  pub position: Option<Point>,
  pub timeout: Option<u64>,
  pub trial: Option<bool>,
  /// Playwright `scroll?: 'auto' | 'none'`: `none` leaves the page where
  /// it is instead of scrolling the target into view first.
  pub scroll: Option<ActionScroll>,
}

impl HoverOptions {
  /// `true` when the caller asked to bypass actionability checks.
  #[must_use]
  pub fn is_force(&self) -> bool {
    self.force.unwrap_or(false)
  }

  /// `true` when the caller asked to run checks only (no mousemove).
  #[must_use]
  pub fn is_trial(&self) -> bool {
    self.trial.unwrap_or(false)
  }
}

/// Options for tap actions (touch input). Distinct from [`HoverOptions`]
/// so future tap-only divergence (e.g. native touch options) has a stable
/// home.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TapOptions {
  pub force: Option<bool>,
  pub modifiers: Vec<Modifier>,
  pub no_wait_after: Option<bool>,
  pub position: Option<Point>,
  pub timeout: Option<u64>,
  pub trial: Option<bool>,
  /// Playwright `scroll?: 'auto' | 'none'`: `none` leaves the page where
  /// it is instead of scrolling the target into view first.
  pub scroll: Option<ActionScroll>,
}

impl TapOptions {
  /// `true` when the caller asked to bypass actionability checks.
  #[must_use]
  pub fn is_force(&self) -> bool {
    self.force.unwrap_or(false)
  }

  /// `true` when the caller asked to run checks only (no touch dispatch).
  #[must_use]
  pub fn is_trial(&self) -> bool {
    self.trial.unwrap_or(false)
  }
}

/// Options for double-click actions. Identical to [`ClickOptions`] minus
/// `click_count` (which is forced to `2` at dispatch time).
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DblClickOptions {
  pub button: Option<MouseButton>,
  pub delay: Option<u64>,
  pub force: Option<bool>,
  pub modifiers: Vec<Modifier>,
  pub no_wait_after: Option<bool>,
  pub position: Option<Point>,
  pub steps: Option<u32>,
  pub timeout: Option<u64>,
  pub trial: Option<bool>,
  /// Playwright `scroll?: 'auto' | 'none'`: `none` leaves the page where
  /// it is instead of scrolling the target into view first.
  pub scroll: Option<ActionScroll>,
}

impl DblClickOptions {
  /// Lower to [`ClickOptions`] with `click_count` forced to `2`. The
  /// shared click dispatch path then emits two `mousedown`/`mouseup`
  /// pairs with `clickCount=1` then `clickCount=2`.
  #[must_use]
  pub fn into_click_options(self) -> ClickOptions {
    ClickOptions {
      button: self.button,
      click_count: Some(2),
      delay: self.delay,
      force: self.force,
      modifiers: self.modifiers,
      no_wait_after: self.no_wait_after,
      position: self.position,
      steps: self.steps,
      timeout: self.timeout,
      trial: self.trial,
      scroll: self.scroll,
    }
  }
}

/// Options for [`crate::page::Page::drag_and_drop`] and
/// [`crate::locator::Locator::drag_to`].
///
/// `strict` is meaningful only on [`crate::page::Page::drag_and_drop`] (which
/// accepts bare selectors); [`crate::locator::Locator::drag_to`] ignores it
/// because the source locator already carries its own strict flag.
///
/// `no_wait_after` is accepted for signature parity but has no effect.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
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
  /// Default is `5` — enough for drag libraries to observe their drag
  /// threshold being crossed (Playwright defaults to `1` but intercepts
  /// the native drag on Chromium, which ferridriver does not).
  pub steps: Option<u32>,
  /// Strict-mode override for resolving the source/target selector.
  /// Meaningful only on `page.drag_and_drop`; ignored by `locator.drag_to`.
  pub strict: Option<bool>,
  /// Maximum time in ms. `0` means no timeout. Default is inherited from
  /// the context's default action timeout.
  pub timeout: Option<u64>,
  /// Perform actionability checks only; skip the actual mouse press/move/release.
  pub trial: Option<bool>,
  /// Playwright `scroll?: 'auto' | 'none'`: `none` leaves the page where
  /// it is instead of scrolling the target into view first.
  pub scroll: Option<ActionScroll>,
}

/// Polling cadence for `waitForFunction`. Playwright's `polling` union:
/// `'raf'` (poll on every animation frame, the default) or a number of
/// milliseconds between polls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Polling {
  /// One poll per animation frame.
  Raf,
  /// Fixed interval between polls, in milliseconds.
  Interval(u64),
}

impl From<u64> for Polling {
  fn from(ms: u64) -> Self {
    Self::Interval(ms)
  }
}

impl From<std::time::Duration> for Polling {
  fn from(interval: std::time::Duration) -> Self {
    Self::Interval(u64::try_from(interval.as_millis()).unwrap_or(u64::MAX))
  }
}

impl<'de> serde::Deserialize<'de> for Polling {
  fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Raw {
      Num(u64),
      Text(String),
    }
    match Raw::deserialize(deserializer)? {
      Raw::Num(ms) => Ok(Self::Interval(ms)),
      Raw::Text(s) if s == "raf" => Ok(Self::Raf),
      Raw::Text(s) => Err(serde::de::Error::custom(format!("Unknown polling option: {s}"))),
    }
  }
}

/// Options for `page.waitForFunction` / `frame.waitForFunction`.
/// Mirrors Playwright's `WaitForFunctionOptions`
/// (`types.d.ts::PageWaitForFunctionOptions`).
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WaitForFunctionOptions {
  /// `'raf'` (default) polls on every animation frame; a number polls
  /// on that interval in milliseconds.
  pub polling: Option<Polling>,
  /// Maximum time in ms. `0` means no timeout. Default is inherited
  /// from the context's default timeout.
  pub timeout: Option<u64>,
}

/// Options for `Locator.drop`.
/// Mirrors Playwright's `Locator.drop(payload, options)` per
/// `client/locator.ts` — the option bag omits `payloads`, `localPaths`,
/// `streams`, `data`, `force`, and `trial` (those are folded into the
/// `DropPayload` or are unsupported), leaving the actionability +
/// positioning fields shared with the other pointer actions.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DropOptions {
  /// Modifier keys held during the drop's pointer events.
  pub modifiers: Vec<Modifier>,
  /// Drop point relative to the target element's padding-box top-left.
  /// When absent, the target element's center is used.
  pub position: Option<Point>,
  /// Maximum time in ms. `0` means no timeout. Default is inherited from
  /// the context's default action timeout.
  pub timeout: Option<u64>,
}

/// Drop payload for `Locator.drop`.
/// Mirrors Playwright's `DropPayload` (`client/types.ts`):
/// `{ files?: string | FilePayload | string[] | FilePayload[], data?: { [mimeType: string]: string } }`.
/// Both fields are optional; an empty payload still dispatches the
/// drag/drop event sequence with an empty `DataTransfer`.
///
/// Native shape only — file payloads are `FilePayload { name, mimeType,
/// buffer }`, never the `{ buffer: base64 }` wire form, and `data` is a
/// list of `(mimeType, value)` pairs lowered from the JS object/map.
#[derive(Debug, Clone, Default)]
pub struct DropPayload {
  /// Files dragged onto the target. `None` means no files; an empty list
  /// behaves the same as `None` (no `File` objects added to the transfer).
  pub files: Option<InputFiles>,
  /// `(mimeType, value)` entries set on the transfer via `DataTransfer.setData`.
  pub data: Vec<(String, String)>,
}

/// Viewport configuration -- consistent across all backends.
/// Matches viewport options.
#[derive(Debug, Clone, PartialEq)]
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
/// TS shape `T | null | undefined`:
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

/// Media emulation options — matches `page.emulateMedia()`. Each field
/// uses [`MediaOverride`] to distinguish *unspecified* (leave current
/// state alone) from *null* (clear any existing override) from a
/// *concrete value*.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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

impl EmulateMediaOptions {
  /// The fields `self` specifies, falling back to `base` for the rest.
  #[must_use]
  pub fn layered_over(&self, base: &Self) -> Self {
    let pick = |own: &MediaOverride, base: &MediaOverride| {
      if own.is_specified() { own.clone() } else { base.clone() }
    };
    Self {
      media: pick(&self.media, &base.media),
      color_scheme: pick(&self.color_scheme, &base.color_scheme),
      reduced_motion: pick(&self.reduced_motion, &base.reduced_motion),
      forced_colors: pick(&self.forced_colors, &base.forced_colors),
      contrast: pick(&self.contrast, &base.contrast),
    }
  }

  /// `true` when no field is specified.
  #[must_use]
  pub fn is_empty(&self) -> bool {
    [
      &self.media,
      &self.color_scheme,
      &self.reduced_motion,
      &self.forced_colors,
      &self.contrast,
    ]
    .iter()
    .all(|field| !field.is_specified())
  }
}

/// PDF page-size dimension as accepted by `PDFOptions.width`,
/// `PDFOptions.height`, and `PDFOptions.margin.*` fields.
///
/// TS accepts `string | number`. A bare number is interpreted as CSS
/// pixels; a string must end with one of the unit suffixes `px`, `in`,
/// `cm`, `mm`.
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
  /// Convert to inches. CDP `Page.printToPDF` expects inches
  /// for `paperWidth` / `paperHeight` / `marginTop` / etc.
  ///
  /// Conversion constants: `px÷96`, `in`, `cm·37.8/96`, `mm·3.78/96`.
  #[must_use]
  pub fn to_inches(&self) -> f64 {
    match *self {
      Self::Pixels(v) => v / 96.0,
      Self::Inches(v) => v,
      Self::Centimeters(v) => v * 37.8 / 96.0,
      Self::Millimeters(v) => v * 3.78 / 96.0,
    }
  }

  /// Parse a size string (`"10px"`, `"2in"`, `"5cm"`, `"15mm"`). Unknown
  /// suffix — or no suffix — is treated as bare pixels.
  ///
  /// # Errors
  ///
  /// Returns [`crate::error::FerriError::InvalidArgument`] if the numeric
  /// portion cannot be parsed.
  pub fn parse(text: &str) -> crate::error::Result<Self> {
    let trimmed = text.trim();
    let suffixed = trimmed
      .len()
      .checked_sub(2)
      .and_then(|at| trimmed.split_at_checked(at))
      .and_then(|(head, tail)| {
        let unit: fn(f64) -> Self = match tail.to_ascii_lowercase().as_str() {
          "px" => Self::Pixels,
          "in" => Self::Inches,
          "cm" => Self::Centimeters,
          "mm" => Self::Millimeters,
          _ => return None,
        };
        Some((head, unit))
      });
    let (num_str, unit) = suffixed.unwrap_or((trimmed, Self::Pixels));
    let value: f64 = num_str.trim().parse().map_err(|_| {
      crate::error::FerriError::invalid_argument("pdf size", format!("cannot parse numeric portion of {text:?}"))
    })?;
    Ok(unit(value))
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

/// Full `PDFOptions` surface (15 fields). Routes through the CDP
/// `Page.printToPDF` plumbing.
///
/// Defaults: every field is `None`/empty; the CDP layer applies its own
/// defaults (`scale = 1`, `landscape = false`, `pageRanges = ""`, ...).
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
  /// Scale factor. default is `1.0` (applied by CDP backend
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

/// Paper-format size lookup. Case-insensitive. Sizes are in inches.
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

/// Options for [`crate::page::Page::close`].
/// `page.close({ runBeforeUnload, reason })`.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PageCloseOptions {
  /// When `true`, the page's `beforeunload` handlers fire before close.
  /// Chromium mapping: switches the CDP method from `Target.closeTarget`
  /// (force-close) to `Page.close` (fires `beforeunload`).
  pub run_before_unload: Option<bool>,
  /// Human-readable reason attached to the resulting `TargetClosed` error
  /// that any in-flight operation on this page will receive.
  pub reason: Option<String>,
}

/// Behavior for [`crate::page::Page::unroute_all`].
/// Mirrors Playwright's `page.unrouteAll({ behavior })` union
/// `'wait' | 'ignoreErrors' | 'default'`.
///
/// In Playwright this controls whether to wait for currently-running
/// route handlers to finish (`wait`), wait but swallow their errors
/// (`ignoreErrors`), or not wait at all (`default`). In ferridriver
/// route handlers run synchronously inside the interception loop, so no
/// detached handler task can still be in flight after the routes are
/// cleared; the variant is accepted for API parity and selects the same
/// post-clear teardown in every case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnrouteBehavior {
  /// Do not wait for active handlers (Playwright default).
  #[default]
  Default,
  /// Wait for active handlers to finish.
  Wait,
  /// Wait for active handlers and ignore any errors they raise.
  IgnoreErrors,
}

/// Options for [`crate::browser::Browser::close`].
/// `browser.close({ reason })`.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BrowserCloseOptions {
  /// Human-readable reason surfaced to `TargetClosed` errors emitted by any
  /// in-flight operation on pages/contexts from this browser.
  pub reason: Option<String>,
}

/// Options for `context.close()` — Playwright's `{ reason? }`. The
/// reason, when set, is recorded on the browser state and surfaces in
/// errors from operations interrupted by the close.
#[derive(Debug, Clone, Default)]
pub struct ContextCloseOptions {
  pub reason: Option<String>,
}
