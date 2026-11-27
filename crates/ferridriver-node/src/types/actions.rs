//! Action option bags and input/select/page-function converters.

use napi_derive::napi;

use super::{Point, f64_to_u64};

/// Playwright-parity options for `page.click` / `locator.click` /
/// `frame.click`. Mirrors `LocatorClickOptions` at
/// `/tmp/playwright/packages/playwright-core/types/types.d.ts:12986`.
/// All fields are the Playwright public surface; `noWaitAfter` is
/// accepted for signature parity but has no effect in ferridriver.
#[napi(object, object_to_js = false)]
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
  #[napi(ts_type = "AbortSignal")]
  pub signal: Option<crate::abort::Signal>,
  pub trial: Option<bool>,
  #[napi(ts_type = "'auto' | 'none'")]
  pub scroll: Option<String>,
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
      scroll: parse_scroll(o.scroll.as_deref())?,
    })
  }
}

/// Playwright-parity options for `page.dblclick` / `locator.dblclick` /
/// `frame.dblclick`. Mirrors `LocatorDblClickOptions` at
/// `/tmp/playwright/packages/playwright-core/types/types.d.ts:13116`.
/// Identical shape to [`ClickOptions`] minus `clickCount` (forced to 2
/// at dispatch).
#[napi(object, object_to_js = false)]
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
  #[napi(ts_type = "AbortSignal")]
  pub signal: Option<crate::abort::Signal>,
  pub trial: Option<bool>,
  #[napi(ts_type = "'auto' | 'none'")]
  pub scroll: Option<String>,
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
      scroll: parse_scroll(o.scroll.as_deref())?,
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
      value_or_label: None,
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
#[napi(object, object_to_js = false)]
#[derive(Debug, Clone, Default)]
pub struct SetInputFilesOptions {
  pub no_wait_after: Option<bool>,
  pub timeout: Option<f64>,
  #[napi(ts_type = "AbortSignal")]
  pub signal: Option<crate::abort::Signal>,
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

/// Lower a JS `string | string[] | FilePayload | FilePayload[]` value into
/// the core `InputFiles` union. Shared by `setInputFiles` and the `files`
/// field of `Locator.drop`'s payload so both accept the identical native
/// shape.
///
/// # Safety
///
/// `env` is the live environment of the current JS call and `napi_val` a
/// handle in its scope.
#[expect(unsafe_code, reason = "reads a JS value napi-rs handed over as a raw handle")]
unsafe fn parse_input_files(
  env: napi::sys::napi_env,
  napi_val: napi::sys::napi_value,
  context: &str,
) -> napi::Result<ferridriver::options::InputFiles> {
  use napi::JsValue;
  use napi::bindgen_prelude::JsObjectValue;
  // SAFETY: `env` and `napi_val` are live per this function's contract.
  let unknown = unsafe { napi::Unknown::from_raw_unchecked(env, napi_val) };
  match unknown.get_type()? {
    napi::ValueType::String => {
      let s = unknown.coerce_to_string()?.into_utf8()?.into_owned()?;
      Ok(ferridriver::options::InputFiles::Paths(vec![s.into()]))
    },
    napi::ValueType::Object => {
      let obj = napi::bindgen_prelude::Object::from_raw(env, napi_val);
      if unknown.is_array()? {
        let len: u32 = obj.get("length")?.unwrap_or(0);
        // Peek first entry to decide whether this is string[] or FilePayload[].
        if len == 0 {
          return Ok(ferridriver::options::InputFiles::Paths(Vec::new()));
        }
        let first: napi::Unknown<'_> = obj.get_element(0)?;
        if first.get_type()? == napi::ValueType::String {
          let mut paths = Vec::with_capacity(len as usize);
          for i in 0..len {
            let s: String = obj.get_element(i)?;
            paths.push(std::path::PathBuf::from(s));
          }
          Ok(ferridriver::options::InputFiles::Paths(paths))
        } else {
          let mut payloads = Vec::with_capacity(len as usize);
          for i in 0..len {
            let p: FilePayload = obj.get_element(i)?;
            payloads.push(p.into());
          }
          Ok(ferridriver::options::InputFiles::Payloads(payloads))
        }
      } else {
        // SAFETY: `get_type` reported an object, which is what the
        // `FilePayload` object conversion reads its fields from.
        let p: FilePayload = unsafe { unknown.cast()? };
        Ok(ferridriver::options::InputFiles::Payloads(vec![p.into()]))
      }
    },
    other => Err(napi::Error::from_reason(format!(
      "{context} expects string | string[] | FilePayload | FilePayload[], got {other}"
    ))),
  }
}

#[expect(unsafe_code, reason = "napi-rs declares FromNapiValue::from_napi_value unsafe")]
impl napi::bindgen_prelude::FromNapiValue for NapiInputFiles {
  unsafe fn from_napi_value(env: napi::sys::napi_env, napi_val: napi::sys::napi_value) -> napi::Result<Self> {
    // SAFETY: the `env` and `napi_val` napi-rs handed this conversion, live for
    // the current call.
    Ok(Self(unsafe { parse_input_files(env, napi_val, "setInputFiles")? }))
  }
}

/// Playwright-parity options for `Locator.drop`. Mirrors the trimmed
/// `FrameDropOptions` bag from `client/locator.ts` (no `payloads`,
/// `localPaths`, `streams`, `data`, `force`, `trial` -- those are folded
/// into the payload or unsupported), leaving `modifiers`, `position`, and
/// `timeout`.
#[napi(object, object_to_js = false)]
#[derive(Debug, Clone, Default)]
pub struct DropOptions {
  #[napi(ts_type = "Array<'Alt' | 'Control' | 'ControlOrMeta' | 'Meta' | 'Shift'>")]
  pub modifiers: Option<Vec<String>>,
  pub position: Option<Point>,
  pub timeout: Option<f64>,
  #[napi(ts_type = "AbortSignal")]
  pub signal: Option<crate::abort::Signal>,
}

impl TryFrom<DropOptions> for ferridriver::options::DropOptions {
  type Error = napi::Error;

  fn try_from(o: DropOptions) -> std::result::Result<Self, Self::Error> {
    let mut modifiers = Vec::new();
    if let Some(list) = o.modifiers {
      for name in list {
        let m = ferridriver::options::Modifier::parse(&name)
          .ok_or_else(|| napi::Error::from_reason(format!("Unknown modifier: {name}")))?;
        modifiers.push(m);
      }
    }
    Ok(Self {
      modifiers,
      position: o.position.map(Into::into),
      timeout: o.timeout.map(f64_to_u64),
    })
  }
}

/// Native `DropPayload` for `Locator.drop`. Mirrors Playwright's
/// `{ files?: string | FilePayload | string[] | FilePayload[], data?: { [mimeType: string]: string } }`
/// via a custom `FromNapiValue` impl -- no wire shape exposed.
#[derive(Debug, Clone)]
pub struct NapiDropPayload(pub ferridriver::options::DropPayload);

impl napi::bindgen_prelude::TypeName for NapiDropPayload {
  fn type_name() -> &'static str {
    "{ files?: string | string[] | FilePayload | FilePayload[], data?: Record<string, string> }"
  }
  fn value_type() -> napi::ValueType {
    napi::ValueType::Object
  }
}

impl napi::bindgen_prelude::ValidateNapiValue for NapiDropPayload {}

#[expect(unsafe_code, reason = "napi-rs declares FromNapiValue::from_napi_value unsafe")]
impl napi::bindgen_prelude::FromNapiValue for NapiDropPayload {
  unsafe fn from_napi_value(env: napi::sys::napi_env, napi_val: napi::sys::napi_value) -> napi::Result<Self> {
    use napi::JsValue;
    use napi::bindgen_prelude::JsObjectValue;
    let obj = napi::bindgen_prelude::Object::from_raw(env, napi_val);

    let files = match obj.get_named_property_unchecked::<napi::Unknown<'_>>("files") {
      Ok(v) if !matches!(v.get_type()?, napi::ValueType::Undefined | napi::ValueType::Null) => {
        // SAFETY: `env` is the live environment napi-rs handed this
        // conversion, and `v` was just read from its object in that scope.
        Some(unsafe { parse_input_files(env, v.raw(), "drop payload files")? })
      },
      _ => None,
    };

    let data = match obj.get::<rustc_hash::FxHashMap<String, String>>("data") {
      Ok(Some(map)) => map.into_iter().collect(),
      _ => Vec::new(),
    };

    Ok(Self(ferridriver::options::DropPayload { files, data }))
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

#[expect(unsafe_code, reason = "napi-rs declares FromNapiValue::from_napi_value unsafe")]
impl napi::bindgen_prelude::FromNapiValue for NapiSelectOptionInput {
  unsafe fn from_napi_value(env: napi::sys::napi_env, napi_val: napi::sys::napi_value) -> napi::Result<Self> {
    use napi::JsValue;
    use napi::bindgen_prelude::JsObjectValue;
    // SAFETY: the `env` and `napi_val` napi-rs handed this conversion, live for
    // the current call.
    let unknown = unsafe { napi::Unknown::from_raw_unchecked(env, napi_val) };
    let mut out: Vec<ferridriver::options::SelectOptionValue> = Vec::new();
    match unknown.get_type()? {
      napi::ValueType::String => {
        let s = unknown.coerce_to_string()?.into_utf8()?.into_owned()?;
        out.push(ferridriver::options::SelectOptionValue::by_value_or_label(s));
      },
      napi::ValueType::Object => {
        let obj = napi::bindgen_prelude::Object::from_raw(env, napi_val);
        // Array path: iterate numeric keys.
        if unknown.is_array()? {
          let len: u32 = obj.get("length")?.unwrap_or(0);
          for i in 0..len {
            let el: napi::Unknown<'_> = obj.get_element(i)?;
            match el.get_type()? {
              napi::ValueType::String => {
                let s = el.coerce_to_string()?.into_utf8()?.into_owned()?;
                out.push(ferridriver::options::SelectOptionValue::by_value_or_label(s));
              },
              napi::ValueType::Object => {
                // SAFETY: `get_type` reported an object, which is what the
                // `SelectOptionValue` object conversion reads its fields from.
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
          // SAFETY: `get_type` reported an object, which is what the
          // `SelectOptionValue` object conversion reads its fields from.
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

#[expect(unsafe_code, reason = "napi-rs declares FromNapiValue::from_napi_value unsafe")]
impl napi::bindgen_prelude::FromNapiValue for NapiPageFunction {
  unsafe fn from_napi_value(env: napi::sys::napi_env, napi_val: napi::sys::napi_value) -> napi::Result<Self> {
    // SAFETY: the `env` and `napi_val` napi-rs handed this conversion, live for
    // the current call.
    let u = unsafe { napi::Unknown::from_raw_unchecked(env, napi_val) };
    let (source, is_function) = crate::serialize_out::extract_fn_source(u)?;
    Ok(Self { source, is_function })
  }
}

pub(crate) use crate::evaluate_arg::NapiEvaluateArg;

/// Playwright's `ExposeFunctionsOptions`, `{ exposeFunctions?: boolean }`,
/// on `evaluate`, `evaluateHandle` and `addInitScript`.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct ExposeFunctionsOptions {
  pub expose_functions: Option<bool>,
}

/// Playwright-parity options for `selectOption`. Three fields per
/// Playwright's `LocatorSelectOptionOptions`.
#[napi(object, object_to_js = false)]
#[derive(Debug, Clone, Default)]
pub struct SelectOptionOptions {
  pub force: Option<bool>,
  pub no_wait_after: Option<bool>,
  pub timeout: Option<f64>,
  #[napi(ts_type = "AbortSignal")]
  pub signal: Option<crate::abort::Signal>,
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
#[napi(object, object_to_js = false)]
#[derive(Debug, Clone, Default)]
pub struct FillOptions {
  pub force: Option<bool>,
  pub no_wait_after: Option<bool>,
  pub timeout: Option<f64>,
  #[napi(ts_type = "AbortSignal")]
  pub signal: Option<crate::abort::Signal>,
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
#[napi(object, object_to_js = false)]
#[derive(Debug, Clone, Default)]
pub struct PressOptions {
  pub delay: Option<f64>,
  pub no_wait_after: Option<bool>,
  pub timeout: Option<f64>,
  #[napi(ts_type = "AbortSignal")]
  pub signal: Option<crate::abort::Signal>,
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
#[napi(object, object_to_js = false)]
#[derive(Debug, Clone, Default)]
pub struct TypeOptions {
  pub delay: Option<f64>,
  pub no_wait_after: Option<bool>,
  pub timeout: Option<f64>,
  #[napi(ts_type = "AbortSignal")]
  pub signal: Option<crate::abort::Signal>,
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
#[napi(object, object_to_js = false)]
#[derive(Debug, Clone, Default)]
pub struct CheckOptions {
  pub force: Option<bool>,
  pub no_wait_after: Option<bool>,
  pub position: Option<Point>,
  pub timeout: Option<f64>,
  #[napi(ts_type = "AbortSignal")]
  pub signal: Option<crate::abort::Signal>,
  pub trial: Option<bool>,
  #[napi(ts_type = "'auto' | 'none'")]
  pub scroll: Option<String>,
}

impl TryFrom<CheckOptions> for ferridriver::options::CheckOptions {
  type Error = napi::Error;

  fn try_from(o: CheckOptions) -> std::result::Result<Self, Self::Error> {
    Ok(Self {
      force: o.force,
      no_wait_after: o.no_wait_after,
      position: o.position.map(Into::into),
      timeout: o.timeout.map(f64_to_u64),
      trial: o.trial,
      scroll: parse_scroll(o.scroll.as_deref())?,
    })
  }
}

/// Playwright-parity options for `page.dispatchEvent` /
/// `locator.dispatchEvent` / `frame.dispatchEvent`. Single field.
#[napi(object, object_to_js = false)]
#[derive(Debug, Clone, Default)]
pub struct DispatchEventOptions {
  pub timeout: Option<f64>,
  #[napi(ts_type = "AbortSignal")]
  pub signal: Option<crate::abort::Signal>,
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
#[napi(object, object_to_js = false)]
#[derive(Debug, Clone, Default)]
pub struct HoverOptions {
  pub force: Option<bool>,
  #[napi(ts_type = "Array<'Alt' | 'Control' | 'ControlOrMeta' | 'Meta' | 'Shift'>")]
  pub modifiers: Option<Vec<String>>,
  pub no_wait_after: Option<bool>,
  pub position: Option<Point>,
  pub timeout: Option<f64>,
  #[napi(ts_type = "AbortSignal")]
  pub signal: Option<crate::abort::Signal>,
  pub trial: Option<bool>,
  #[napi(ts_type = "'auto' | 'none'")]
  pub scroll: Option<String>,
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
      scroll: parse_scroll(o.scroll.as_deref())?,
    })
  }
}

/// Playwright-parity options for `page.tap` / `locator.tap` /
/// `frame.tap`. Mirrors `LocatorTapOptions` at
/// `/tmp/playwright/packages/playwright-core/types/types.d.ts` under
/// `tap(options?)`. Explicit struct — distinct from `HoverOptions` so
/// future tap-only divergence has a home.
#[napi(object, object_to_js = false)]
#[derive(Debug, Clone, Default)]
pub struct TapOptions {
  pub force: Option<bool>,
  #[napi(ts_type = "Array<'Alt' | 'Control' | 'ControlOrMeta' | 'Meta' | 'Shift'>")]
  pub modifiers: Option<Vec<String>>,
  pub no_wait_after: Option<bool>,
  pub position: Option<Point>,
  pub timeout: Option<f64>,
  #[napi(ts_type = "AbortSignal")]
  pub signal: Option<crate::abort::Signal>,
  pub trial: Option<bool>,
  #[napi(ts_type = "'auto' | 'none'")]
  pub scroll: Option<String>,
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
      scroll: parse_scroll(o.scroll.as_deref())?,
    })
  }
}

/// Playwright-parity options for `page.dragAndDrop` and `locator.dragTo`.
/// Mirrors `FrameDragAndDropOptions & TimeoutOptions` at
/// `/tmp/playwright/packages/playwright-core/types/types.d.ts:2486` (for
/// `page.dragAndDrop`) and `:13293` (for `locator.dragTo`). `strict` has
/// no effect on `locator.dragTo` (the locator already carries strict);
/// it is meaningful only on `page.dragAndDrop`.
#[napi(object, object_to_js = false)]
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
  #[napi(ts_type = "AbortSignal")]
  pub signal: Option<crate::abort::Signal>,
  /// Perform actionability checks only; skip the actual mouse action.
  pub trial: Option<bool>,
  #[napi(ts_type = "'auto' | 'none'")]
  pub scroll: Option<String>,
}

impl TryFrom<DragAndDropOptions> for ferridriver::options::DragAndDropOptions {
  type Error = napi::Error;

  fn try_from(o: DragAndDropOptions) -> std::result::Result<Self, Self::Error> {
    Ok(Self {
      force: o.force,
      no_wait_after: o.no_wait_after,
      source_position: o.source_position.map(Into::into),
      target_position: o.target_position.map(Into::into),
      steps: o.steps,
      strict: o.strict,
      timeout: o.timeout.map(f64_to_u64),
      trial: o.trial,
      scroll: parse_scroll(o.scroll.as_deref())?,
    })
  }
}

/// Playwright's `scroll?: 'auto' | 'none'` action option.
fn parse_scroll(value: Option<&str>) -> napi::Result<Option<ferridriver::options::ActionScroll>> {
  value
    .map(|v| {
      ferridriver::options::ActionScroll::parse(v)
        .ok_or_else(|| napi::Error::from_reason(format!("scroll: expected 'auto' or 'none', got {v:?}")))
    })
    .transpose()
}
