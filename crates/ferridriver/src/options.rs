//! Option structs for the Page and Locator API.

/// A string or a regular expression — the `string | RegExp` union accepted
/// by every `getBy*` matcher, `page.waitForURL`, and similar selector
/// inputs.
///
/// Construction:
/// * `StringOrRegex::from("foo")` — literal string (substring match
///   case-insensitive by default, matched verbatim when `exact: true`).
/// * `StringOrRegex::regex("foo\\d+", "i")` — regex with pattern +
///   `ECMAScript` flags. At the binding boundary NAPI accepts a real
///   JS `RegExp` instance; `QuickJS` similarly reads `source`/`flags`
///   getters off a `RegExp` via prototype-chain access. Wire-shape
///   inputs like `{ regexSource, regexFlags }` are never exposed to
///   the user.
#[derive(Debug, Clone)]
pub enum StringOrRegex {
  String(String),
  Regex { source: String, flags: String },
}

impl StringOrRegex {
  /// Return the literal string if this is a `String` variant. Used by
  /// backends that only accept a literal (no regex support).
  #[must_use]
  pub fn as_str(&self) -> Option<&str> {
    match self {
      Self::String(s) => Some(s),
      Self::Regex { .. } => None,
    }
  }

  /// Convenience — construct the regex variant from source + flags.
  #[must_use]
  pub fn regex(source: impl Into<String>, flags: impl Into<String>) -> Self {
    Self::Regex {
      source: source.into(),
      flags: flags.into(),
    }
  }
}

impl From<&str> for StringOrRegex {
  fn from(s: &str) -> Self {
    Self::String(s.to_string())
  }
}

impl From<String> for StringOrRegex {
  fn from(s: String) -> Self {
    Self::String(s)
  }
}

/// Options for role-based locators (getByRole).
#[derive(Debug, Clone, Default)]
pub struct RoleOptions {
  /// `string | RegExp` — matches the element's accessible name.
  pub name: Option<StringOrRegex>,
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
/// [`crate::locator::Locator`] additionally enables frame-equality checking at filter
/// construction time.
#[derive(Debug, Clone)]
pub enum LocatorLike {
  /// Full locator — preferred form for Rust callers. Enables same-page
  /// checks in [`crate::locator::Locator::filter`].
  Locator(crate::locator::Locator),
  /// Inner selector string verbatim. Used by NAPI/BDD where a full
  /// [`crate::locator::Locator`] cannot be materialized across the binding boundary.
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
/// [`crate::context::ContextRef::add_init_script`]. Accepts the
/// `Function | string | { path?, content? }` union.
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
  /// errors with "Cannot evaluate a string with arguments".
  Source(String),
  /// Path to an on-disk script file. Read at [`evaluation_script`] call
  /// time; a `//# sourceURL=<path>` comment is appended. Passing `arg`
  /// alongside errors.
  Path(std::path::PathBuf),
  /// Literal script content (from the `{ content }` bag variant).
  /// Semantically equivalent to [`Self::Source`]; kept as a distinct
  /// variant so callers can route the object shape losslessly.
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
/// wire-level source string the backend receives.
///
/// Semantics:
/// - [`InitScriptSource::Function`] + `arg` → `(body)(JSON.stringify(arg))`.
///   Absent `arg` renders as `undefined` (matches `Object.is(arg, undefined)`).
///   `null` passes through as `"null"`.
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

/// Options for filtering locators — used by both
/// `Locator::filter(options)` and the `Locator` constructor. Every field
/// maps directly to a corresponding injected-selector clause:
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

/// `LocatorEvaluateOptions` — only the `timeout` field today.
#[derive(Debug, Clone, Default)]
pub struct EvaluateOptions {
  pub timeout: Option<u64>,
}

/// Rendering mode for `locator.ariaSnapshot` / `page.ariaSnapshot`.
/// Mirrors Playwright's `mode?: 'ai' | 'default'`
/// (`/tmp/playwright/packages/playwright-core/src/client/locator.ts:327`).
/// The vendored injected `AriaTreeOptions` also has `codegen` /
/// `autoexpect`, but those are not part of the public client surface.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AriaSnapshotMode {
  /// Playwright default — stable YAML without volatile refs.
  #[default]
  Default,
  /// AI-optimized — includes `[ref=eN]` labels + generic roles.
  Ai,
}

impl AriaSnapshotMode {
  #[must_use]
  pub fn as_str(self) -> &'static str {
    match self {
      AriaSnapshotMode::Default => "default",
      AriaSnapshotMode::Ai => "ai",
    }
  }

  /// Parse the public mode string; unknown values fall back to `default`
  /// (Playwright server uses `mode ?? 'default'`).
  #[must_use]
  pub fn from_opt_str(s: Option<&str>) -> Self {
    match s {
      Some("ai") => AriaSnapshotMode::Ai,
      _ => AriaSnapshotMode::Default,
    }
  }
}

/// Options for `locator.ariaSnapshot(options?)`. Mirrors Playwright's
/// `TimeoutOptions & { mode?: 'ai' | 'default', depth?: number }`
/// (`/tmp/playwright/packages/playwright-core/src/client/locator.ts:327`;
/// the vendored injected `AriaTreeOptions` has no `boxes`, so it is not
/// exposed — it would be a silent no-op).
#[derive(Debug, Clone, Default)]
pub struct AriaSnapshotOptions {
  pub mode: Option<AriaSnapshotMode>,
  /// Subtree depth limit passed to the injected `generateAriaTree`.
  pub depth: Option<i32>,
  /// Actionability/resolution timeout (ms). `None` = page default.
  pub timeout: Option<u64>,
}

/// Full `PageScreenshotOptions` surface — 13 fields. Includes the
/// `LocatorScreenshotOptions` subset for `Locator.screenshot()` (which
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
/// - `mask` — locators whose matches are overlaid with `mask_color`
///   before capture. Mirrors Playwright's `mask?: Locator[]`; the
///   selector string is extracted from each locator before backend
///   dispatch.
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
/// - `format` — `"png"` (default), `"jpeg"`, or `"webp"`. Renamed from
///   the JS `type` field because `type` is reserved in Rust. For CDP
///   both `jpeg` and `webp` honour `quality`; `webp` additionally
///   supports transparency.
#[derive(Debug, Clone, Default)]
pub struct ScreenshotOptions {
  pub animations: Option<String>,
  pub caret: Option<String>,
  pub clip: Option<ClipRect>,
  pub full_page: Option<bool>,
  pub format: Option<String>,
  pub mask: Vec<crate::locator::Locator>,
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
/// other contexts. Used by `sourcePosition`, `targetPosition`, and click
/// `position`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Point {
  pub x: f64,
  pub y: f64,
}

/// Mouse button for click/dblclick/mousedown/mouseup. The `"left" |
/// "right" | "middle"` union.
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

  /// Parse from string form. Returns `None` on unknown input so callers
  /// can raise a typed `FerriError::InvalidArgument` at the binding
  /// boundary.
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

/// Single keyboard modifier. The `Alt | Control | ControlOrMeta | Meta |
/// Shift` union. `ControlOrMeta` resolves at call time — see
/// [`Self::cdp_bit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modifier {
  Alt,
  Control,
  ControlOrMeta,
  Meta,
  Shift,
}

impl Modifier {
  /// Parse from string form. Returns `None` on unknown input.
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
  /// `Alt=1`, `Control=2`, `Meta=4`, `Shift=8` (per CDP docs).
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
/// All three expose the same 10-field surface.
///
/// Every option is `Option<T>`: callers omit fields and the backend
/// applies the documented defaults (`button: Left`, `click_count: 1`,
/// `delay: 0`, `steps: 1`, etc.).
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

/// Options for `fill` (set an input's value). Three fields.
/// `no_wait_after` is accepted for signature parity; `force` skips the
/// fillable / editable actionability check.
#[derive(Debug, Clone, Default)]
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
#[derive(Debug, Clone, Default)]
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
#[derive(Debug, Clone, Default)]
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
#[derive(Debug, Clone, Default)]
pub struct CheckOptions {
  pub force: Option<bool>,
  pub no_wait_after: Option<bool>,
  pub position: Option<Point>,
  pub timeout: Option<u64>,
  pub trial: Option<bool>,
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
    }
  }
}

/// A single descriptor used by `selectOption`. At least one of `value`,
/// `label`, or `index` must be set. An array of these descriptors
/// selects every `<option>` matching any descriptor (multi-select).
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct SelectOptionValue {
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
#[derive(Debug, Clone, Default)]
pub struct SelectOptionOptions {
  pub force: Option<bool>,
  pub no_wait_after: Option<bool>,
  pub timeout: Option<u64>,
}

/// Options for `setInputFiles`.
/// `LocatorSetInputFilesOptions` — two fields.
#[derive(Debug, Clone, Default)]
pub struct SetInputFilesOptions {
  pub no_wait_after: Option<bool>,
  pub timeout: Option<u64>,
}

/// File payload for `setInputFiles`.
/// `FilePayload` — caller supplies raw bytes plus the filename and MIME
/// type that the page should see, avoiding any on-disk write.
#[derive(Debug, Clone)]
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

/// Options for `dispatchEvent`.
/// `LocatorDispatchEventOptions` — single field (`timeout`).
#[derive(Debug, Clone, Default)]
pub struct DispatchEventOptions {
  pub timeout: Option<u64>,
}

/// Options for hover actions. Shape is [`ClickOptions`] minus `button`,
/// `click_count`, `delay` (no press/release — just a `mousemove` at the
/// target).
#[derive(Debug, Clone, Default)]
pub struct HoverOptions {
  pub force: Option<bool>,
  pub modifiers: Vec<Modifier>,
  pub no_wait_after: Option<bool>,
  pub position: Option<Point>,
  pub timeout: Option<u64>,
  pub trial: Option<bool>,
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
#[derive(Debug, Clone, Default)]
pub struct TapOptions {
  pub force: Option<bool>,
  pub modifiers: Vec<Modifier>,
  pub no_wait_after: Option<bool>,
  pub position: Option<Point>,
  pub timeout: Option<u64>,
  pub trial: Option<bool>,
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
#[derive(Debug, Clone, Default)]
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
  /// default is `1` (a single move at the destination).
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

/// Options for `Locator.drop`.
/// Mirrors Playwright's `Locator.drop(payload, options)` per
/// `client/locator.ts` — the option bag omits `payloads`, `localPaths`,
/// `streams`, `data`, `force`, and `trial` (those are folded into the
/// `DropPayload` or are unsupported), leaving the actionability +
/// positioning fields shared with the other pointer actions.
#[derive(Debug, Clone, Default)]
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

/// Options for [`crate::browser::Browser::close`].
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
  /// `page.goto(url, { referer })`. If both this and
  /// `extraHTTPHeaders.referer` are set, this wins.
  pub referer: Option<String>,
}

/// Which browser product. Three `BrowserType` instances exposed as
/// `chromium`, `firefox`, and `webkit` on the top-level module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserKind {
  /// Google Chrome / Chromium
  Chromium,
  /// Mozilla Firefox
  Firefox,
  /// Apple `WebKit` (macOS only)
  WebKit,
}

impl BrowserKind {
  /// Product name string: `"chromium"` / `"firefox"` / `"webkit"`.
  /// Matches `BrowserType.name()`.
  #[must_use]
  pub fn name(self) -> &'static str {
    match self {
      Self::Chromium => "chromium",
      Self::Firefox => "firefox",
      Self::WebKit => "webkit",
    }
  }

  /// Default backend protocol for this product. Chromium runs over the
  /// CDP pipe transport (lowest latency), Firefox over `WebDriver`
  /// `BiDi`, and `WebKit` over the native macOS host IPC.
  #[must_use]
  pub fn default_backend(self) -> crate::backend::BackendKind {
    match self {
      Self::Chromium => crate::backend::BackendKind::CdpPipe,
      Self::Firefox => crate::backend::BackendKind::Bidi,
      Self::WebKit => crate::backend::BackendKind::WebKit,
    }
  }
}

/// Public launch options bag, the `browserType.launch(options)`
/// parameter. Selection of which browser to launch happens via the
/// `BrowserType` instance you call `.launch(...)` on (`chromium()`,
/// `firefox()`, `webkit()`); this bag only carries the per-launch knobs.
#[derive(Debug, Clone, Default)]
pub struct LaunchOptions {
  /// Run in headless mode. Defaults to `true` (default).
  pub headless: Option<bool>,
  /// Path to a browser executable to run instead of the bundled one.
  pub executable_path: Option<String>,
  /// Extra command-line arguments to pass to the browser.
  pub args: Vec<String>,
  /// Browser distribution channel (e.g. `"chrome"`, `"chrome-beta"`,
  /// `"msedge"`). Currently surface-only — the bundled-browser resolver
  /// reads this when selecting between the headless shell and a
  /// channel-specific Chrome install.
  pub channel: Option<String>,
  /// Environment variables to set when spawning the browser process.
  pub env: Option<rustc_hash::FxHashMap<String, String>>,
  /// Slow down operations by this many ms (debugging).
  pub slow_mo: Option<u64>,
  /// Maximum time to wait for the browser to start. `0` means no
  /// timeout. Defaults to `30_000`.
  pub timeout: Option<u64>,
  /// Directory to use for downloads (per-context override is on
  /// [`BrowserContextOptions`] / persistent-context launch).
  pub downloads_path: Option<std::path::PathBuf>,
  /// If `true`, do not pass the bundled "default args"; if a list of
  /// strings, filter out the named default args. Currently surface-only
  /// — wired through to [`LaunchPlan`] for future filtering work.
  pub ignore_default_args: Option<IgnoreDefaultArgs>,
  /// Per-process signal handling — defaults all three to
  /// `true` (close the browser on SIGHUP / SIGINT / SIGTERM).
  pub handle_sighup: Option<bool>,
  pub handle_sigint: Option<bool>,
  pub handle_sigterm: Option<bool>,
  /// Enable Chromium sandboxing. Defaults to `false`.
  pub chromium_sandbox: Option<bool>,
  /// Firefox `about:config` user-prefs map.
  pub firefox_user_prefs: Option<rustc_hash::FxHashMap<String, serde_json::Value>>,
  /// Network proxy applied at the browser level.
  pub proxy: Option<ProxyConfig>,
  /// Tracing artifact directory.
  pub traces_dir: Option<std::path::PathBuf>,
}

/// `ignoreDefaultArgs?: boolean | string[]` shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IgnoreDefaultArgs {
  /// `true` — drop ALL default args.
  All,
  /// String list — drop just these.
  Some(Vec<String>),
}

/// Connect-to-server options bag for `browserType.connect(wsEndpoint, options)`.
#[derive(Debug, Clone, Default)]
pub struct ConnectOptions {
  pub headers: Option<rustc_hash::FxHashMap<String, String>>,
  pub slow_mo: Option<u64>,
  pub timeout: Option<u64>,
  pub expose_network: Option<String>,
}

/// Connect-over-CDP options bag for `browserType.connectOverCDP(endpointURL, options)`.
/// Chromium-only.
#[derive(Debug, Clone, Default)]
pub struct ConnectOverCdpOptions {
  pub headers: Option<rustc_hash::FxHashMap<String, String>>,
  pub slow_mo: Option<u64>,
  pub timeout: Option<u64>,
}

/// Persistent-context launch options bag for
/// `browserType.launchPersistentContext(userDataDir, options)`. Composed
/// of the launch knobs plus a full [`BrowserContextOptions`] applied to
/// the implicit default context.
#[derive(Debug, Clone, Default)]
pub struct LaunchPersistentContextOptions {
  /// Per-launch knobs (mirror [`LaunchOptions`] exactly).
  pub launch: LaunchOptions,
  /// Per-context knobs applied to the default context that ships with
  /// the persistent profile.
  pub context: BrowserContextOptions,
}

/// Per-`BrowserType` instance configuration carried by `chromium()` /
/// `firefox()` / `webkit()` factories. The single field that varies
/// today is `transport` for Chromium — `chromium` is
/// always pipe-only; ferridriver lets callers override to the
/// `WebSocket` transport (`CdpRaw`) for backend-coverage testing.
#[derive(Debug, Clone, Default)]
pub struct BrowserTypeOptions {
  /// Transport override for Chromium. Ignored for Firefox / `WebKit`.
  pub transport: Option<ChromiumTransport>,
}

/// Wire transport for the Chromium backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromiumTransport {
  /// CDP over Unix pipe (fd 3/4). Default — lowest latency.
  Pipe,
  /// CDP over WebSocket. Used by `connectOverCDP` and explicitly
  /// selectable via `chromium({ transport: 'ws' })`.
  Ws,
}

/// Internal launch plan. Carries fields that are NOT exposed on the
/// public [`LaunchOptions`] (which mirrors verbatim) but
/// that the runtime needs in order to launch / connect to the right
/// backend. Constructed by `BrowserType` from the public options bag
/// and the per-instance kind/transport, then handed to
/// [`crate::state::BrowserState::with_plan`].
#[derive(Debug, Clone)]
pub struct LaunchPlan {
  pub backend: crate::backend::BackendKind,
  pub kind: BrowserKind,
  pub headless: bool,
  pub executable_path: Option<String>,
  pub args: Vec<String>,
  pub channel: Option<String>,
  pub env: Option<rustc_hash::FxHashMap<String, String>>,
  pub user_data_dir: Option<String>,
  pub ws_endpoint: Option<String>,
  pub auto_connect: Option<AutoConnectOptions>,
  pub default_viewport: Option<ViewportConfig>,
  pub slow_mo: Option<u64>,
  pub timeout: Option<u64>,
  pub downloads_path: Option<std::path::PathBuf>,
  pub ignore_default_args: Option<IgnoreDefaultArgs>,
  pub handle_sighup: Option<bool>,
  pub handle_sigint: Option<bool>,
  pub handle_sigterm: Option<bool>,
  pub chromium_sandbox: Option<bool>,
  pub firefox_user_prefs: Option<rustc_hash::FxHashMap<String, serde_json::Value>>,
  pub proxy: Option<ProxyConfig>,
  pub traces_dir: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone)]
pub struct AutoConnectOptions {
  pub channel: String,
  pub user_data_dir: Option<String>,
}

impl Default for LaunchPlan {
  fn default() -> Self {
    Self {
      backend: crate::backend::BackendKind::CdpPipe,
      kind: BrowserKind::Chromium,
      headless: true,
      executable_path: None,
      args: Vec::new(),
      channel: None,
      env: None,
      user_data_dir: None,
      ws_endpoint: None,
      auto_connect: None,
      default_viewport: Some(ViewportConfig::default()),
      slow_mo: None,
      timeout: None,
      downloads_path: None,
      ignore_default_args: None,
      handle_sighup: None,
      handle_sigint: None,
      handle_sigterm: None,
      chromium_sandbox: None,
      firefox_user_prefs: None,
      proxy: None,
      traces_dir: None,
    }
  }
}

impl LaunchPlan {
  /// Build a launch plan from the public [`LaunchOptions`] plus the
  /// per-`BrowserType` selection (`kind` + optional Chromium transport
  /// override).
  #[must_use]
  pub fn from_public(kind: BrowserKind, transport: Option<ChromiumTransport>, opts: LaunchOptions) -> Self {
    let backend = match (kind, transport) {
      (BrowserKind::Chromium, Some(ChromiumTransport::Ws)) => crate::backend::BackendKind::CdpRaw,
      _ => kind.default_backend(),
    };
    Self {
      backend,
      kind,
      headless: opts.headless.unwrap_or(true),
      executable_path: opts.executable_path,
      args: opts.args,
      channel: opts.channel,
      env: opts.env,
      user_data_dir: None,
      ws_endpoint: None,
      auto_connect: None,
      default_viewport: Some(ViewportConfig::default()),
      slow_mo: opts.slow_mo,
      timeout: opts.timeout,
      downloads_path: opts.downloads_path,
      ignore_default_args: opts.ignore_default_args,
      handle_sighup: opts.handle_sighup,
      handle_sigint: opts.handle_sigint,
      handle_sigterm: opts.handle_sigterm,
      chromium_sandbox: opts.chromium_sandbox,
      firefox_user_prefs: opts.firefox_user_prefs,
      proxy: opts.proxy,
      traces_dir: opts.traces_dir,
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

/// Per-context video-recording configuration: `recordVideo: { dir,
/// size? }` option on `browserType.launch` + `browser.newContext`.
/// Enabling it starts `CDP` screencast / `BiDi` polyfill recording on
/// every new page in the context; the file is finalised when the page
/// closes and surfaced via `page.video().path()`.
#[derive(Debug, Clone)]
pub struct RecordVideoOptions {
  /// Directory where the video file is written. One file per page.
  /// Filenames are derived from the page's created-at timestamp.
  pub dir: std::path::PathBuf,
  /// Optional explicit video dimensions. When `None`, ferridriver
  /// defaults to `800x450` (fallback when no viewport is set) unless the
  /// caller supplies a size. Values are forced to an even number of
  /// pixels so `libx264` accepts them without `yuv420p`-conversion
  /// warnings.
  pub size: Option<VideoSize>,
}

/// Video frame dimensions for [`RecordVideoOptions::size`]. Matches
/// `recordVideo.size: { width, height }` shape.
#[derive(Debug, Clone, Copy)]
pub struct VideoSize {
  pub width: u32,
  pub height: u32,
}

impl Default for VideoSize {
  fn default() -> Self {
    Self {
      width: 800,
      height: 450,
    }
  }
}

/// Resolve a user-supplied URL against an optional base URL. Delegates
/// to the standard URL `new URL(given, base)` resolution rule.
///
/// - Absolute URLs (with scheme) are returned verbatim.
/// - Relative paths (`/foo`, `./foo`, `foo`) resolve against `base`.
/// - Invalid inputs fall through to the given URL unchanged —
///   matches try/catch fallback.
#[must_use]
pub fn construct_url_with_base(base: Option<&str>, given: &str) -> String {
  // No base, or already absolute (scheme present) → passthrough.
  if base.is_none() || given.contains("://") || given.starts_with("data:") || given.starts_with("about:") {
    return given.to_string();
  }
  let base = base.unwrap_or("");
  // Minimal URL-join: strip trailing slash from base (keep the root
  // slash only), handle given-has-leading-slash vs not. This is a
  // pragmatic subset — covers the common `baseURL + /path` and
  // `baseURL + path` cases. Absolute-URL / query / fragment rules
  // match `new URL(given, base)` for the common patterns.
  let (base_origin, base_path) = split_origin_and_path(base);
  if given.starts_with('/') {
    // Root-relative: replace the base's path entirely.
    return format!("{base_origin}{given}");
  }
  // Path-relative: strip the last segment of base_path (everything
  // after the final `/`) then append `given`.
  let cut = base_path.rfind('/').map_or(0, |i| i + 1);
  let kept = &base_path[..cut];
  format!("{base_origin}{kept}{given}")
}

fn split_origin_and_path(url: &str) -> (&str, &str) {
  // Locate the `://` separator; if missing, treat the whole thing
  // as a path (no origin).
  let Some(scheme_end) = url.find("://") else {
    return ("", url);
  };
  let rest_start = scheme_end + 3;
  let rest = &url[rest_start..];
  // The path starts at the first `/` after the host (+optional port).
  match rest.find('/') {
    Some(path_start) => (&url[..rest_start + path_start], &rest[path_start..]),
    None => (url, "/"),
  }
}

// ── BrowserContextOptions ──────────────────────────────────────────────────

/// Geographic location emulation: `Geolocation { latitude, longitude,
/// accuracy? }`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Geolocation {
  /// Latitude between -90 and 90.
  pub latitude: f64,
  /// Longitude between -180 and 180.
  pub longitude: f64,
  /// Non-negative accuracy value. defaults to 0.
  pub accuracy: f64,
}

impl Default for Geolocation {
  fn default() -> Self {
    Self {
      latitude: 0.0,
      longitude: 0.0,
      accuracy: 0.0,
    }
  }
}

/// HTTP basic/digest credentials: `HTTPCredentials { username, password,
/// origin?, send? }`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HttpCredentials {
  pub username: String,
  pub password: String,
  /// Scheme + host + optional port — restrict credential send to this
  /// origin. `None` sends on any 401 response.
  pub origin: Option<String>,
  /// `"always"` sends credentials on every `APIRequest`; `"unauthorized"`
  /// (default) waits for a 401. default: unauthorized.
  pub send: Option<HttpCredentialsSend>,
}

/// Send policy for [`HttpCredentials`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HttpCredentialsSend {
  /// Only on 401 responses (default).
  #[default]
  Unauthorized,
  /// On every request (`HttpClient` only).
  Always,
}

/// Network proxy configuration.
/// `{ server, bypass?, username?, password? }` — types.d.ts:22412.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProxyConfig {
  pub server: String,
  /// Comma-separated domain list (e.g. `".com, chromium.org"`).
  pub bypass: Option<String>,
  pub username: Option<String>,
  pub password: Option<String>,
}

/// `recordHar` options bag. `recordHar` shape —
/// types.d.ts:22441.
#[derive(Debug, Clone)]
pub struct RecordHarOptions {
  pub path: std::path::PathBuf,
  /// `omit`/`embed`/`attach`. Default derived from `.path` extension.
  pub content: Option<RecordHarContent>,
  /// `full`/`minimal`. Default: full.
  pub mode: Option<RecordHarMode>,
  /// Legacy alias for `content: "omit"`. flags this deprecated
  /// but still accepts it.
  pub omit_content: Option<bool>,
  /// Glob/regex filter for stored requests.
  pub url_filter: Option<crate::url_matcher::UrlMatcher>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordHarContent {
  Omit,
  Embed,
  Attach,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordHarMode {
  Full,
  Minimal,
}

/// Logical viewport dimensions for [`BrowserContextOptions::viewport`].
/// Three states: `Default` (omit → browser default), `Null` (explicit
/// null → opt out of viewport emulation), or `Size(w,h)`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ViewportOption {
  /// Field absent — default 1280x720.
  #[default]
  Default,
  /// Field explicitly `null` — opt out of fixed viewport.
  Null,
  /// Concrete viewport size.
  Size { width: i64, height: i64 },
}

/// `window.screen` size emulation (when viewport is set). Mirrors
/// `screen: { width, height }` — types.d.ts:22539.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenSize {
  pub width: i64,
  pub height: i64,
}

/// Storage state bag — cookies + per-origin localStorage snapshot.
/// `storageState: string | { cookies, origins }` —
/// types.d.ts:22566.
#[derive(Debug, Clone)]
pub enum StorageStateInput {
  /// Path to a JSON file written by `context.storageState({ path })`.
  Path(std::path::PathBuf),
  /// Inline state object.
  Inline(serde_json::Value),
}

/// Service-worker policy.
/// `serviceWorkers: "allow" | "block"` — types.d.ts:22557.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ServiceWorkerPolicy {
  #[default]
  Allow,
  Block,
}

/// `BrowserContextOptions` — the option bag accepted by
/// `Browser::new_context`. Full 28-field shape.
///
/// Every field is optional. `None` means "browser default"; an explicit
/// value applies the corresponding emulation at every page opened in
/// the context. Several fields have sub-options that distinguish
/// explicit `null` from absent (e.g. `viewport: null` to disable
/// viewport emulation vs. omitted).
///
/// Construction: use [`BrowserContextOptions::default`] and set fields
/// field-by-field, or use any of the dedicated builder helpers defined
/// inline.
#[derive(Debug, Clone, Default)]
pub struct BrowserContextOptions {
  pub accept_downloads: Option<bool>,
  pub base_url: Option<String>,
  pub bypass_csp: Option<bool>,
  /// `null` → disable media emulation; `Some(value)` → apply; absent →
  /// leave backend default. Use [`MediaOverride`] for the null/value
  /// distinction.
  pub color_scheme: MediaOverride,
  pub contrast: MediaOverride,
  pub device_scale_factor: Option<f64>,
  pub extra_http_headers: Option<rustc_hash::FxHashMap<String, String>>,
  pub forced_colors: MediaOverride,
  pub geolocation: Option<Geolocation>,
  pub has_touch: Option<bool>,
  pub http_credentials: Option<HttpCredentials>,
  pub ignore_https_errors: Option<bool>,
  pub is_mobile: Option<bool>,
  pub java_script_enabled: Option<bool>,
  pub locale: Option<String>,
  pub offline: Option<bool>,
  pub permissions: Option<Vec<String>>,
  pub proxy: Option<ProxyConfig>,
  pub record_har: Option<RecordHarOptions>,
  pub record_video: Option<RecordVideoOptions>,
  pub reduced_motion: MediaOverride,
  pub screen: Option<ScreenSize>,
  pub service_workers: Option<ServiceWorkerPolicy>,
  pub storage_state: Option<StorageStateInput>,
  pub strict_selectors: Option<bool>,
  pub timezone_id: Option<String>,
  pub user_agent: Option<String>,
  pub viewport: ViewportOption,
}

impl BrowserContextOptions {
  /// Resolve [`Self::viewport`] to the [`ViewportConfig`] a freshly
  /// opened page should be emulated with. Returns `None` when the
  /// caller passed `viewport: null` — the page inherits the backend's
  /// native window size. `ViewportOption::Default` folds in
  /// `device_scale_factor`, `is_mobile`, and `has_touch` into the
  /// default 1280x720; explicit `Size(w,h)` likewise.
  #[must_use]
  pub fn resolved_viewport(&self) -> Option<ViewportConfig> {
    let (width, height) = match self.viewport {
      ViewportOption::Null => return None,
      ViewportOption::Default => (1280, 720),
      ViewportOption::Size { width, height } => (width, height),
    };
    Some(ViewportConfig {
      width,
      height,
      device_scale_factor: self.device_scale_factor.unwrap_or(1.0),
      is_mobile: self.is_mobile.unwrap_or(false),
      has_touch: self.has_touch.unwrap_or(false),
      is_landscape: false,
    })
  }

  /// `true` iff any emulated-media field needs to be applied.
  #[must_use]
  pub fn any_media_override(&self) -> bool {
    self.color_scheme.is_specified()
      || self.reduced_motion.is_specified()
      || self.forced_colors.is_specified()
      || self.contrast.is_specified()
  }

  /// Collect the media fields into an [`EmulateMediaOptions`] bag for
  /// `page.emulate_media`.
  #[must_use]
  pub fn as_emulate_media(&self) -> EmulateMediaOptions {
    EmulateMediaOptions {
      media: MediaOverride::Unchanged,
      color_scheme: self.color_scheme.clone(),
      reduced_motion: self.reduced_motion.clone(),
      forced_colors: self.forced_colors.clone(),
      contrast: self.contrast.clone(),
    }
  }
}

/// Selector for [`crate::Page::frame`]. The `page.frame(frameSelector)`
/// union type `string | { name?: string; url?:
/// string|RegExp|URLPattern|(url => bool) }`.
///
/// Today we accept the string form + `{ name, url }` with both fields
/// being plain strings (exact match). Future work extends `url` to the
/// full `StringOrRegex` matcher; matching rules will remain behind this
/// struct so callers don't rebind.
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

  /// Returns `true` when neither `name` nor `url` is set.
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
    // parity: Phantom-compatible fallback to px if no known unit.
    assert_eq!(PdfSize::parse("42").unwrap(), PdfSize::Pixels(42.0));
  }

  #[test]
  fn unknown_suffix_falls_back_to_pixels() {
    // `em` is not in the table — treats the whole string as px.
    // The numeric value here is "42" (with "em" treated as suffix but then
    // falling through to the default "px" branch). The parser slices the
    // last 2 chars, sees "em" (unknown), then parses the WHOLE original
    // string as a number. "42em" isn't a number → error. Unknown suffix
    // + non-numeric body ⇒ InvalidArgument.
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
  fn centimeters_convert_using_table_constants() {
    // 37.8 / 96 (exact constant).
    let expected = 10.0 * 37.8 / 96.0;
    assert!((PdfSize::Centimeters(10.0).to_inches() - expected).abs() < 1e-9);
  }

  #[test]
  fn millimeters_convert_using_table_constants() {
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

  #[test]
  fn default_drop_options_and_payload_are_empty() {
    let opts = DropOptions::default();
    assert!(opts.modifiers.is_empty());
    assert!(opts.position.is_none());
    assert!(opts.timeout.is_none());

    let payload = DropPayload::default();
    assert!(payload.files.is_none());
    assert!(payload.data.is_empty());
  }

  #[test]
  fn drop_payload_carries_files_and_data() {
    let payload = DropPayload {
      files: Some(InputFiles::Payloads(vec![FilePayload {
        name: "card.png".into(),
        mime_type: "image/png".into(),
        buffer: vec![1, 2, 3],
      }])),
      data: vec![("text/plain".into(), "dropped".into())],
    };
    match payload.files {
      Some(InputFiles::Payloads(p)) => {
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].name, "card.png");
        assert_eq!(p[0].mime_type, "image/png");
        assert_eq!(p[0].buffer, vec![1, 2, 3]);
      },
      _ => panic!("expected payloads"),
    }
    assert_eq!(payload.data, vec![("text/plain".to_string(), "dropped".to_string())]);
  }

  #[test]
  fn drop_options_carry_modifiers_position_timeout() {
    let opts = DropOptions {
      modifiers: vec![Modifier::Shift, Modifier::ControlOrMeta],
      position: Some(Point { x: 5.0, y: 7.0 }),
      timeout: Some(1_500),
    };
    assert_eq!(opts.modifiers, vec![Modifier::Shift, Modifier::ControlOrMeta]);
    assert_eq!(opts.position, Some(Point { x: 5.0, y: 7.0 }));
    assert_eq!(opts.timeout, Some(1_500));
  }
}

#[cfg(test)]
mod init_script_tests {
  use super::*;
  use serde_json::json;

  #[test]
  fn function_with_undefined_arg_renders_literal_undefined() {
    // `Object.is(arg, undefined) ? 'undefined' : JSON.stringify(arg)`.
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
    // defaults to 1 and uses `Math.max(1, steps)`; mirror
    // the clamp so callers passing `0` still emit one mousemove.
    let opts = ClickOptions {
      steps: Some(0),
      ..Default::default()
    };
    assert_eq!(opts.resolved_steps(), 1);
  }
}
