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
  pub has_text: Option<String>,
  pub has_not_text: Option<String>,
  pub has: Option<LocatorRef>,
  pub has_not: Option<LocatorRef>,
  pub visible: Option<bool>,
}

/// Options for waiting operations.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct WaitOptions {
  /// "visible", "hidden", "attached", "stable"
  pub state: Option<String>,
  pub timeout: Option<f64>,
}

/// Playwright `LocatorEvaluateOptions` — matches `{ timeout?: number }`.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct EvaluateOptions {
  pub timeout: Option<f64>,
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
#[napi(object)]
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
  /// Selectors whose matches are painted over with [`Self::mask_color`].
  /// Playwright takes `Array<Locator>`; we accept selectors here
  /// because `Locator` instances lower to their selector string at
  /// the NAPI boundary (see [`LocatorRef`]).
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

/// Playwright-parity options for `page.click` / `locator.click` /
/// `frame.click`. Mirrors `LocatorClickOptions` at
/// `/tmp/playwright/packages/playwright-core/types/types.d.ts:12986`.
/// All fields are the Playwright public surface; `noWaitAfter` is
/// accepted for signature parity but has no effect in ferridriver.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct ClickOptions {
  #[napi(ts_type = "'left' | 'right' | 'middle'")]
  pub button: Option<String>,
  pub click_count: Option<u32>,
  /// Wait in ms between `mousedown` and `mouseup`. Default `0`.
  pub delay: Option<f64>,
  pub force: Option<bool>,
  #[napi(ts_type = "Array<'Alt' | 'Control' | 'ControlOrMeta' | 'Meta' | 'Shift'>")]
  pub modifiers: Option<Vec<String>>,
  pub no_wait_after: Option<bool>,
  pub position: Option<Point>,
  pub steps: Option<u32>,
  pub timeout: Option<f64>,
  pub trial: Option<bool>,
}

impl TryFrom<ClickOptions> for ferridriver::options::ClickOptions {
  type Error = napi::Error;

  fn try_from(o: ClickOptions) -> std::result::Result<Self, Self::Error> {
    let button = match o.button.as_deref() {
      None => None,
      Some(s) => Some(
        ferridriver::options::MouseButton::parse(s)
          .ok_or_else(|| napi::Error::from_reason(format!("Unknown mouse button: {s}")))?,
      ),
    };
    let mut modifiers = Vec::new();
    if let Some(list) = o.modifiers {
      for name in list {
        let m = ferridriver::options::Modifier::parse(&name)
          .ok_or_else(|| napi::Error::from_reason(format!("Unknown modifier: {name}")))?;
        modifiers.push(m);
      }
    }
    Ok(Self {
      button,
      click_count: o.click_count,
      delay: o.delay.map(f64_to_u64),
      force: o.force,
      modifiers,
      no_wait_after: o.no_wait_after,
      position: o.position.map(Into::into),
      steps: o.steps,
      timeout: o.timeout.map(f64_to_u64),
      trial: o.trial,
    })
  }
}

/// Playwright-parity options for `page.dblclick` / `locator.dblclick` /
/// `frame.dblclick`. Mirrors `LocatorDblClickOptions` at
/// `/tmp/playwright/packages/playwright-core/types/types.d.ts:13116`.
/// Identical shape to [`ClickOptions`] minus `clickCount` (forced to 2
/// at dispatch).
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct DblClickOptions {
  #[napi(ts_type = "'left' | 'right' | 'middle'")]
  pub button: Option<String>,
  pub delay: Option<f64>,
  pub force: Option<bool>,
  #[napi(ts_type = "Array<'Alt' | 'Control' | 'ControlOrMeta' | 'Meta' | 'Shift'>")]
  pub modifiers: Option<Vec<String>>,
  pub no_wait_after: Option<bool>,
  pub position: Option<Point>,
  pub steps: Option<u32>,
  pub timeout: Option<f64>,
  pub trial: Option<bool>,
}

impl TryFrom<DblClickOptions> for ferridriver::options::DblClickOptions {
  type Error = napi::Error;

  fn try_from(o: DblClickOptions) -> std::result::Result<Self, Self::Error> {
    let button = match o.button.as_deref() {
      None => None,
      Some(s) => Some(
        ferridriver::options::MouseButton::parse(s)
          .ok_or_else(|| napi::Error::from_reason(format!("Unknown mouse button: {s}")))?,
      ),
    };
    let mut modifiers = Vec::new();
    if let Some(list) = o.modifiers {
      for name in list {
        let m = ferridriver::options::Modifier::parse(&name)
          .ok_or_else(|| napi::Error::from_reason(format!("Unknown modifier: {name}")))?;
        modifiers.push(m);
      }
    }
    Ok(Self {
      button,
      delay: o.delay.map(f64_to_u64),
      force: o.force,
      modifiers,
      no_wait_after: o.no_wait_after,
      position: o.position.map(Into::into),
      steps: o.steps,
      timeout: o.timeout.map(f64_to_u64),
      trial: o.trial,
    })
  }
}

/// Single descriptor for `selectOption`. Mirrors Playwright's
/// `{ value?, label?, index? }` object shape. At least one field must
/// be set for a descriptor to match anything.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct SelectOptionValue {
  pub value: Option<String>,
  pub label: Option<String>,
  pub index: Option<u32>,
}

impl From<SelectOptionValue> for ferridriver::options::SelectOptionValue {
  fn from(o: SelectOptionValue) -> Self {
    Self {
      value: o.value,
      label: o.label,
      index: o.index,
    }
  }
}

/// In-memory file payload for `setInputFiles`. Matches Playwright's
/// `FilePayload` — callers supply name, mimeType, and raw bytes.
/// `napi::bindgen_prelude::Buffer` doesn't implement `Clone`/`Debug`
/// (it holds a non-cloneable JS handle), so this struct does the same
/// — it's used as an in-out shuttle type only.
#[napi(object)]
pub struct FilePayload {
  pub name: String,
  pub mime_type: String,
  pub buffer: napi::bindgen_prelude::Buffer,
}

impl From<FilePayload> for ferridriver::options::FilePayload {
  fn from(o: FilePayload) -> Self {
    Self {
      name: o.name,
      mime_type: o.mime_type,
      buffer: o.buffer.to_vec(),
    }
  }
}

/// Playwright-parity options for `setInputFiles`. Two fields.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct SetInputFilesOptions {
  pub no_wait_after: Option<bool>,
  pub timeout: Option<f64>,
}

impl From<SetInputFilesOptions> for ferridriver::options::SetInputFilesOptions {
  fn from(o: SetInputFilesOptions) -> Self {
    Self {
      no_wait_after: o.no_wait_after,
      timeout: o.timeout.map(f64_to_u64),
    }
  }
}

/// Polymorphic `files` argument for `setInputFiles`. Mirrors
/// Playwright's `string | string[] | FilePayload | FilePayload[]`
/// union via a custom `FromNapiValue` impl.
#[derive(Debug, Clone)]
pub struct NapiInputFiles(pub ferridriver::options::InputFiles);

impl napi::bindgen_prelude::TypeName for NapiInputFiles {
  fn type_name() -> &'static str {
    "string | string[] | FilePayload | FilePayload[]"
  }
  fn value_type() -> napi::ValueType {
    napi::ValueType::Unknown
  }
}

impl napi::bindgen_prelude::ValidateNapiValue for NapiInputFiles {}

impl napi::bindgen_prelude::FromNapiValue for NapiInputFiles {
  unsafe fn from_napi_value(env: napi::sys::napi_env, napi_val: napi::sys::napi_value) -> napi::Result<Self> {
    use napi::JsValue;
    use napi::bindgen_prelude::JsObjectValue;
    let unknown = unsafe { napi::Unknown::from_raw_unchecked(env, napi_val) };
    match unknown.get_type()? {
      napi::ValueType::String => {
        let s = unknown.coerce_to_string()?.into_utf8()?.into_owned()?;
        Ok(Self(ferridriver::options::InputFiles::Paths(vec![s.into()])))
      },
      napi::ValueType::Object => {
        let obj = napi::bindgen_prelude::Object::from_raw(env, napi_val);
        let is_array = unsafe {
          let mut is_arr = false;
          napi::check_status!(napi::sys::napi_is_array(env, napi_val, &raw mut is_arr))?;
          is_arr
        };
        if is_array {
          let len: u32 = obj.get("length")?.unwrap_or(0);
          // Peek first entry to decide whether this is string[] or FilePayload[].
          if len == 0 {
            return Ok(Self(ferridriver::options::InputFiles::Paths(Vec::new())));
          }
          let first: napi::Unknown<'_> = obj.get_element(0)?;
          if first.get_type()? == napi::ValueType::String {
            let mut paths = Vec::with_capacity(len as usize);
            for i in 0..len {
              let s: String = obj.get_element(i)?;
              paths.push(std::path::PathBuf::from(s));
            }
            Ok(Self(ferridriver::options::InputFiles::Paths(paths)))
          } else {
            let mut payloads = Vec::with_capacity(len as usize);
            for i in 0..len {
              let p: FilePayload = obj.get_element(i)?;
              payloads.push(p.into());
            }
            Ok(Self(ferridriver::options::InputFiles::Payloads(payloads)))
          }
        } else {
          let p: FilePayload = unsafe { unknown.cast()? };
          Ok(Self(ferridriver::options::InputFiles::Payloads(vec![p.into()])))
        }
      },
      other => Err(napi::Error::from_reason(format!(
        "setInputFiles expects string | string[] | FilePayload | FilePayload[], got {other}"
      ))),
    }
  }
}

/// Polymorphic `values` argument for `selectOption`. Mirrors
/// Playwright's
/// `string | string[] | { value?, label?, index? } | Array<...>`
/// union at the NAPI boundary via a single custom `FromNapiValue` impl
/// (avoids dumping `any` into the generated `.d.ts`).
#[derive(Debug, Clone)]
pub struct NapiSelectOptionInput(pub Vec<ferridriver::options::SelectOptionValue>);

impl napi::bindgen_prelude::TypeName for NapiSelectOptionInput {
  fn type_name() -> &'static str {
    "string | string[] | { value?, label?, index? } | Array<{ value?, label?, index? }>"
  }
  fn value_type() -> napi::ValueType {
    napi::ValueType::Unknown
  }
}

impl napi::bindgen_prelude::ValidateNapiValue for NapiSelectOptionInput {}

impl napi::bindgen_prelude::FromNapiValue for NapiSelectOptionInput {
  unsafe fn from_napi_value(env: napi::sys::napi_env, napi_val: napi::sys::napi_value) -> napi::Result<Self> {
    use napi::JsValue;
    use napi::bindgen_prelude::JsObjectValue;
    let unknown = unsafe { napi::Unknown::from_raw_unchecked(env, napi_val) };
    let mut out: Vec<ferridriver::options::SelectOptionValue> = Vec::new();
    match unknown.get_type()? {
      napi::ValueType::String => {
        let s = unknown.coerce_to_string()?.into_utf8()?.into_owned()?;
        out.push(ferridriver::options::SelectOptionValue::by_value(s));
      },
      napi::ValueType::Object => {
        let obj = napi::bindgen_prelude::Object::from_raw(env, napi_val);
        // Array path: iterate numeric keys.
        let is_array = unsafe {
          let mut is_arr = false;
          napi::check_status!(napi::sys::napi_is_array(env, napi_val, &raw mut is_arr))?;
          is_arr
        };
        if is_array {
          let len: u32 = obj.get("length")?.unwrap_or(0);
          for i in 0..len {
            let el: napi::Unknown<'_> = obj.get_element(i)?;
            match el.get_type()? {
              napi::ValueType::String => {
                let s = el.coerce_to_string()?.into_utf8()?.into_owned()?;
                out.push(ferridriver::options::SelectOptionValue::by_value(s));
              },
              napi::ValueType::Object => {
                let v: SelectOptionValue = unsafe { el.cast()? };
                out.push(v.into());
              },
              other => {
                return Err(napi::Error::from_reason(format!(
                  "selectOption array element must be string or object, got {other}"
                )));
              },
            }
          }
        } else {
          let v: SelectOptionValue = unsafe { unknown.cast()? };
          out.push(v.into());
        }
      },
      other => {
        return Err(napi::Error::from_reason(format!(
          "selectOption expects string | string[] | object | object[], got {other}"
        )));
      },
    }
    Ok(Self(out))
  }
}

/// Polymorphic `arg` parameter for the evaluate family (`page.evaluate`,
/// `page.evaluateHandle`, `handle.evaluate`, `handle.evaluateHandle`,
/// `elementHandle.$eval`, etc.). Detects `JSHandle` / `ElementHandle`
/// class instances at the NAPI boundary so a handle passed as the
/// argument surfaces inside the user function as a native page-side
/// object, mirroring Playwright's `serializeArgument` at
/// `/tmp/playwright/packages/playwright-core/src/client/jsHandle.ts:91`.
///
/// Nested handles (inside arrays / plain objects) are not detected in
/// this pass — top-level handles cover every shipped Playwright test
/// that passes a handle as `arg`; nested is a follow-up that walks the
/// object tree before it hits the JSON fallback.
/// User-visible `pageFunction` arg on every evaluate method. Accepts
/// either a JS string or a JS function — matches Playwright's
/// `String(pageFunction)` + `typeof pageFunction === 'function'` check
/// (`/tmp/playwright/packages/playwright-core/src/client/frame.ts:196`).
/// The binding extracts the function source via the engine's own
/// `ToString` (`Function.prototype.toString` for functions), plus an
/// `is_function` hint so the backend knows whether to invoke-or-evaluate.
#[derive(Debug, Clone)]
pub struct NapiPageFunction {
  pub source: String,
  pub is_function: Option<bool>,
}

impl napi::bindgen_prelude::TypeName for NapiPageFunction {
  fn type_name() -> &'static str {
    "unknown"
  }
  fn value_type() -> napi::ValueType {
    napi::ValueType::Unknown
  }
}

impl napi::bindgen_prelude::ValidateNapiValue for NapiPageFunction {}

impl napi::bindgen_prelude::FromNapiValue for NapiPageFunction {
  unsafe fn from_napi_value(env: napi::sys::napi_env, napi_val: napi::sys::napi_value) -> napi::Result<Self> {
    let u = unsafe { napi::Unknown::from_raw_unchecked(env, napi_val) };
    let (source, is_function) = crate::serialize_out::extract_fn_source(u)?;
    Ok(Self { source, is_function })
  }
}

#[derive(Debug, Clone)]
pub struct NapiEvaluateArg(pub ferridriver::protocol::SerializedArgument);

impl napi::bindgen_prelude::TypeName for NapiEvaluateArg {
  fn type_name() -> &'static str {
    "unknown"
  }
  fn value_type() -> napi::ValueType {
    napi::ValueType::Unknown
  }
}

impl napi::bindgen_prelude::ValidateNapiValue for NapiEvaluateArg {}

impl napi::bindgen_prelude::FromNapiValue for NapiEvaluateArg {
  unsafe fn from_napi_value(env: napi::sys::napi_env, napi_val: napi::sys::napi_value) -> napi::Result<Self> {
    use ferridriver::protocol::{SerializationContext, SerializedArgument, SerializedValue, SpecialValue};

    // Try top-level class-instance detection. `napi_unwrap` returns an
    // error when the value isn't a wrapped class of the target `T`;
    // attempts fall through to the JSON path. When the value IS a
    // `JSHandle` / `ElementHandle`, the binding extracts the backend
    // remote and hands it to `HandleRemote::to_serialized_argument`
    // — the canonical packaging lives on core so the NAPI and
    // QuickJS surfaces produce identical wire shapes (Rule 1: Rust
    // is source of truth; bindings are thin mirrors).
    if let Ok(jh) = unsafe {
      <napi::bindgen_prelude::ClassInstance<'_, crate::js_handle::JSHandle> as napi::bindgen_prelude::FromNapiValue>::from_napi_value(env, napi_val)
    } {
      return Ok(Self(jh.inner_ref().backing().to_serialized_argument()));
    }
    if let Ok(eh) = unsafe {
      <napi::bindgen_prelude::ClassInstance<'_, crate::element_handle::ElementHandle> as napi::bindgen_prelude::FromNapiValue>::from_napi_value(env, napi_val)
    } {
      return Ok(Self(eh.inner_ref().as_js_handle().backing().to_serialized_argument()));
    }

    // Quick `undefined` check via the C API to avoid importing `JsValue`
    // just for one call. A missing / undefined arg surfaces as the
    // `undefined` sentinel so the utility script skips the arg slot.
    let mut ty = napi::sys::ValueType::napi_undefined;
    unsafe {
      napi::check_status!(napi::sys::napi_typeof(env, napi_val, &raw mut ty))?;
    }
    if ty == napi::sys::ValueType::napi_undefined {
      return Ok(Self(SerializedArgument {
        value: SerializedValue::Special(SpecialValue::Undefined),
        handles: Vec::new(),
      }));
    }
    // JSON fallback (null / primitives / plain objects / arrays).
    // `FromNapiValue` on `serde_json::Value` does the walk.
    let json: serde_json::Value =
      unsafe { <serde_json::Value as napi::bindgen_prelude::FromNapiValue>::from_napi_value(env, napi_val)? };
    let mut alloc = SerializationContext::default();
    Ok(Self(SerializedArgument {
      value: SerializedValue::from_json(&json, &mut alloc),
      handles: Vec::new(),
    }))
  }
}

/// Playwright-parity options for `selectOption`. Three fields per
/// Playwright's `LocatorSelectOptionOptions`.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct SelectOptionOptions {
  pub force: Option<bool>,
  pub no_wait_after: Option<bool>,
  pub timeout: Option<f64>,
}

impl From<SelectOptionOptions> for ferridriver::options::SelectOptionOptions {
  fn from(o: SelectOptionOptions) -> Self {
    Self {
      force: o.force,
      no_wait_after: o.no_wait_after,
      timeout: o.timeout.map(f64_to_u64),
    }
  }
}

/// Playwright-parity options for `page.fill` / `locator.fill` /
/// `frame.fill`. Three fields per Playwright's `LocatorFillOptions`.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct FillOptions {
  pub force: Option<bool>,
  pub no_wait_after: Option<bool>,
  pub timeout: Option<f64>,
}

impl From<FillOptions> for ferridriver::options::FillOptions {
  fn from(o: FillOptions) -> Self {
    Self {
      force: o.force,
      no_wait_after: o.no_wait_after,
      timeout: o.timeout.map(f64_to_u64),
    }
  }
}

/// Playwright-parity options for `page.press` / `locator.press` /
/// `frame.press`. Three fields per Playwright's `LocatorPressOptions`.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct PressOptions {
  pub delay: Option<f64>,
  pub no_wait_after: Option<bool>,
  pub timeout: Option<f64>,
}

impl From<PressOptions> for ferridriver::options::PressOptions {
  fn from(o: PressOptions) -> Self {
    Self {
      delay: o.delay.map(f64_to_u64),
      no_wait_after: o.no_wait_after,
      timeout: o.timeout.map(f64_to_u64),
    }
  }
}

/// Playwright-parity options for `page.type` / `locator.type` /
/// `locator.pressSequentially`. Same shape as [`PressOptions`] per
/// Playwright's `LocatorTypeOptions`.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct TypeOptions {
  pub delay: Option<f64>,
  pub no_wait_after: Option<bool>,
  pub timeout: Option<f64>,
}

impl From<TypeOptions> for ferridriver::options::TypeOptions {
  fn from(o: TypeOptions) -> Self {
    Self {
      delay: o.delay.map(f64_to_u64),
      no_wait_after: o.no_wait_after,
      timeout: o.timeout.map(f64_to_u64),
    }
  }
}

/// Playwright-parity options for `check` / `uncheck` / `setChecked`.
/// Five fields per Playwright's `LocatorCheckOptions`.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct CheckOptions {
  pub force: Option<bool>,
  pub no_wait_after: Option<bool>,
  pub position: Option<Point>,
  pub timeout: Option<f64>,
  pub trial: Option<bool>,
}

impl From<CheckOptions> for ferridriver::options::CheckOptions {
  fn from(o: CheckOptions) -> Self {
    Self {
      force: o.force,
      no_wait_after: o.no_wait_after,
      position: o.position.map(Into::into),
      timeout: o.timeout.map(f64_to_u64),
      trial: o.trial,
    }
  }
}

/// Playwright-parity options for `page.dispatchEvent` /
/// `locator.dispatchEvent` / `frame.dispatchEvent`. Single field.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct DispatchEventOptions {
  pub timeout: Option<f64>,
}

impl From<DispatchEventOptions> for ferridriver::options::DispatchEventOptions {
  fn from(o: DispatchEventOptions) -> Self {
    Self {
      timeout: o.timeout.map(f64_to_u64),
    }
  }
}

/// Playwright-parity options for `page.hover` / `locator.hover` /
/// `frame.hover`. Mirrors `LocatorHoverOptions` at
/// `/tmp/playwright/packages/playwright-core/types/types.d.ts` under
/// `hover(options?)`. No `steps` — hover always moves in a single step
/// in Playwright (steps is a click/dblclick-only field).
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct HoverOptions {
  pub force: Option<bool>,
  #[napi(ts_type = "Array<'Alt' | 'Control' | 'ControlOrMeta' | 'Meta' | 'Shift'>")]
  pub modifiers: Option<Vec<String>>,
  pub no_wait_after: Option<bool>,
  pub position: Option<Point>,
  pub timeout: Option<f64>,
  pub trial: Option<bool>,
}

impl TryFrom<HoverOptions> for ferridriver::options::HoverOptions {
  type Error = napi::Error;

  fn try_from(o: HoverOptions) -> std::result::Result<Self, Self::Error> {
    let mut modifiers = Vec::new();
    if let Some(list) = o.modifiers {
      for name in list {
        let m = ferridriver::options::Modifier::parse(&name)
          .ok_or_else(|| napi::Error::from_reason(format!("Unknown modifier: {name}")))?;
        modifiers.push(m);
      }
    }
    Ok(Self {
      force: o.force,
      modifiers,
      no_wait_after: o.no_wait_after,
      position: o.position.map(Into::into),
      timeout: o.timeout.map(f64_to_u64),
      trial: o.trial,
    })
  }
}

/// Playwright-parity options for `page.tap` / `locator.tap` /
/// `frame.tap`. Mirrors `LocatorTapOptions` at
/// `/tmp/playwright/packages/playwright-core/types/types.d.ts` under
/// `tap(options?)`. Explicit struct — distinct from `HoverOptions` so
/// future tap-only divergence has a home.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct TapOptions {
  pub force: Option<bool>,
  #[napi(ts_type = "Array<'Alt' | 'Control' | 'ControlOrMeta' | 'Meta' | 'Shift'>")]
  pub modifiers: Option<Vec<String>>,
  pub no_wait_after: Option<bool>,
  pub position: Option<Point>,
  pub timeout: Option<f64>,
  pub trial: Option<bool>,
}

impl TryFrom<TapOptions> for ferridriver::options::TapOptions {
  type Error = napi::Error;

  fn try_from(o: TapOptions) -> std::result::Result<Self, Self::Error> {
    let mut modifiers = Vec::new();
    if let Some(list) = o.modifiers {
      for name in list {
        let m = ferridriver::options::Modifier::parse(&name)
          .ok_or_else(|| napi::Error::from_reason(format!("Unknown modifier: {name}")))?;
        modifiers.push(m);
      }
    }
    Ok(Self {
      force: o.force,
      modifiers,
      no_wait_after: o.no_wait_after,
      position: o.position.map(Into::into),
      timeout: o.timeout.map(f64_to_u64),
      trial: o.trial,
    })
  }
}

/// Playwright-parity options for `page.dragAndDrop` and `locator.dragTo`.
/// Mirrors `FrameDragAndDropOptions & TimeoutOptions` at
/// `/tmp/playwright/packages/playwright-core/types/types.d.ts:2486` (for
/// `page.dragAndDrop`) and `:13293` (for `locator.dragTo`). `strict` has
/// no effect on `locator.dragTo` (the locator already carries strict);
/// it is meaningful only on `page.dragAndDrop`.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct DragAndDropOptions {
  /// Bypass actionability checks.
  pub force: Option<bool>,
  /// Deprecated — has no effect. Accepted for signature parity.
  pub no_wait_after: Option<bool>,
  /// Press point relative to the source element's padding-box top-left.
  pub source_position: Option<Point>,
  /// Release point relative to the target element's padding-box top-left.
  pub target_position: Option<Point>,
  /// Interpolated `mousemove` events between press and release. Default `1`.
  pub steps: Option<u32>,
  /// Strict-mode override (only meaningful on `page.dragAndDrop`).
  pub strict: Option<bool>,
  /// Maximum time in ms. `0` disables the timeout.
  pub timeout: Option<f64>,
  /// Perform actionability checks only; skip the actual mouse action.
  pub trial: Option<bool>,
}

impl From<DragAndDropOptions> for ferridriver::options::DragAndDropOptions {
  fn from(o: DragAndDropOptions) -> Self {
    Self {
      force: o.force,
      no_wait_after: o.no_wait_after,
      source_position: o.source_position.map(Into::into),
      target_position: o.target_position.map(Into::into),
      steps: o.steps,
      strict: o.strict,
      timeout: o.timeout.map(f64_to_u64),
      trial: o.trial,
    }
  }
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

/// Playwright-parity `page.emulateMedia` options per
/// `/tmp/playwright/packages/playwright-core/types/types.d.ts:2580`.
///
/// Every field is `Option<Either<String, Null>>` so we can surface all
/// three states Playwright's TS signature requires: absent (field not
/// present on the object → don't change), JS `null` (reset the override),
/// or a string value (apply it). Plain `Option<String>` would conflate
/// undefined and null — napi-rs would either reject null or silently fold
/// it into None, breaking the contract.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct EmulateMediaOptions {
  #[napi(ts_type = "null | 'screen' | 'print'")]
  pub media: Option<napi::Either<String, napi::bindgen_prelude::Null>>,
  #[napi(ts_type = "null | 'light' | 'dark' | 'no-preference'")]
  pub color_scheme: Option<napi::Either<String, napi::bindgen_prelude::Null>>,
  #[napi(ts_type = "null | 'reduce' | 'no-preference'")]
  pub reduced_motion: Option<napi::Either<String, napi::bindgen_prelude::Null>>,
  #[napi(ts_type = "null | 'active' | 'none'")]
  pub forced_colors: Option<napi::Either<String, napi::bindgen_prelude::Null>>,
  #[napi(ts_type = "null | 'no-preference' | 'more'")]
  pub contrast: Option<napi::Either<String, napi::bindgen_prelude::Null>>,
}

fn lower_override(v: Option<napi::Either<String, napi::bindgen_prelude::Null>>) -> ferridriver::options::MediaOverride {
  match v {
    None => ferridriver::options::MediaOverride::Unchanged,
    Some(napi::Either::A(s)) => ferridriver::options::MediaOverride::Set(s),
    Some(napi::Either::B(_)) => ferridriver::options::MediaOverride::Disabled,
  }
}

impl From<EmulateMediaOptions> for ferridriver::options::EmulateMediaOptions {
  fn from(o: EmulateMediaOptions) -> Self {
    Self {
      media: lower_override(o.media),
      color_scheme: lower_override(o.color_scheme),
      reduced_motion: lower_override(o.reduced_motion),
      forced_colors: lower_override(o.forced_colors),
      contrast: lower_override(o.contrast),
    }
  }
}

/// Selector bag form of [`FrameSelectorArg`] — matches Playwright's
/// object form `{ name?, url? }` (the `string` form is handled by
/// [`FrameSelectorArg`]'s `Either::A`).
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct FrameSelectorBag {
  pub name: Option<String>,
  pub url: Option<String>,
}

/// Playwright's `page.frame(frameSelector)` union —
/// `string | { name?, url? }`. napi-rs resolves the union at the JS
/// boundary; the `ts_args_type` on the call site forces the generated
/// `.d.ts` to the precise shape.
pub type FrameSelectorArg = napi::Either<String, FrameSelectorBag>;

impl From<FrameSelectorBag> for ferridriver::options::FrameSelector {
  fn from(b: FrameSelectorBag) -> Self {
    Self {
      name: b.name.filter(|s| !s.is_empty()),
      url: b.url.filter(|s| !s.is_empty()),
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
      has: o.has.map(|r| ferridriver::options::LocatorLike::Selector(r.selector)),
      has_not: o
        .has_not
        .map(|r| ferridriver::options::LocatorLike::Selector(r.selector)),
      visible: o.visible,
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
      animations: o.animations,
      caret: o.caret,
      clip: o.clip.map(Into::into),
      full_page: o.full_page,
      format: o.format,
      mask: o.mask.unwrap_or_default().into_iter().map(|l| l.selector).collect(),
      mask_color: o.mask_color,
      omit_background: o.omit_background,
      path: o.path.map(std::path::PathBuf::from),
      quality: o.quality.map(i64::from),
      scale: o.scale,
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

impl napi::bindgen_prelude::FromNapiValue for NapiInitScript {
  unsafe fn from_napi_value(env: napi::sys::napi_env, napi_val: napi::sys::napi_value) -> napi::Result<Self> {
    use napi::JsValue;
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
#[derive(Debug, Clone, Default)]
pub struct NapiInitScriptArg(pub Option<serde_json::Value>);

impl napi::bindgen_prelude::TypeName for NapiInitScriptArg {
  fn type_name() -> &'static str {
    "any"
  }
  fn value_type() -> napi::ValueType {
    napi::ValueType::Unknown
  }
}

impl napi::bindgen_prelude::ValidateNapiValue for NapiInitScriptArg {}

impl napi::bindgen_prelude::FromNapiValue for NapiInitScriptArg {
  unsafe fn from_napi_value(env: napi::sys::napi_env, napi_val: napi::sys::napi_value) -> napi::Result<Self> {
    let mut t: napi::sys::napi_valuetype = 0;
    napi::check_status!(unsafe { napi::sys::napi_typeof(env, napi_val, &raw mut t) })?;
    if t == napi::sys::ValueType::napi_undefined {
      return Ok(Self(None));
    }
    if t == napi::sys::ValueType::napi_null {
      return Ok(Self(Some(serde_json::Value::Null)));
    }
    let v: serde_json::Value = unsafe { napi::bindgen_prelude::FromNapiValue::from_napi_value(env, napi_val)? };
    Ok(Self(Some(v)))
  }
}
