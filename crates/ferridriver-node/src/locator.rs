//! Locator class -- NAPI binding for `ferridriver::Locator`.

use crate::error::IntoNapi;
use crate::types::{BoundingBox, FilterOptions, RoleOptions, TextOptions, WaitOptions};
use napi::Result;
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

/// A lazy element locator. Does not query the DOM until an action is called.
#[napi]
pub struct Locator {
  inner: ferridriver::Locator,
}

impl Locator {
  pub(crate) fn wrap(inner: ferridriver::Locator) -> Self {
    Self { inner }
  }
}

#[napi]
impl Locator {
  /// The selector string for this locator.
  #[napi(getter)]
  pub fn selector(&self) -> String {
    self.inner.selector().to_string()
  }

  /// Whether this locator runs under strict mode (Playwright default). Use
  /// [`Locator::setStrict`] to opt out on a per-locator basis, or `first()` /
  /// `last()` / `nth(i)` which drop strictness implicitly.
  #[napi(getter)]
  pub fn is_strict(&self) -> bool {
    self.inner.is_strict()
  }

  /// Returns a copy of this locator with strict-mode toggled. Mirrors
  /// Playwright's strict-selectors context option on a per-locator basis.
  #[napi]
  pub fn set_strict(&self, strict: bool) -> Locator {
    Self::wrap(self.inner.strict(strict))
  }

  // ── Handle materialisation (Playwright `locator.elementHandle`) ───────

  /// Playwright: `locator.elementHandle(): Promise<ElementHandle>`.
  /// Resolves the locator and returns a pinned ElementHandle.
  #[napi]
  pub async fn element_handle(&self) -> Result<crate::element_handle::ElementHandle> {
    let inner = self.inner.element_handle().await.map_err(crate::error::to_napi)?;
    Ok(crate::element_handle::ElementHandle::wrap(inner))
  }

  /// Playwright: `locator.elementHandles(): Promise<ElementHandle[]>`.
  #[napi]
  pub async fn element_handles(&self) -> Result<Vec<crate::element_handle::ElementHandle>> {
    let inner = self.inner.element_handles().await.map_err(crate::error::to_napi)?;
    Ok(
      inner
        .into_iter()
        .map(crate::element_handle::ElementHandle::wrap)
        .collect(),
    )
  }

  // ── Sub-locators ────────────────────────────────────────────────────────

  /// Playwright: `locator.locator(selectorOrLocator, options?: Omit<LocatorOptions, 'visible'>): Locator`
  /// (`/tmp/playwright/packages/playwright-core/src/client/locator.ts:164`).
  /// Thin delegator to Rust core's `Locator::locator` — core does all
  /// encoding and option application; this binding only lowers JS types.
  #[napi(ts_args_type = "selectorOrLocator: string | Locator, options?: FilterOptions")]
  pub fn locator(
    &self,
    selector_or_locator: napi::Either<String, crate::types::LocatorRef>,
    options: Option<crate::types::FilterOptions>,
  ) -> Locator {
    let like = match selector_or_locator {
      napi::Either::A(selector) => ferridriver::options::LocatorLike::Selector(selector),
      napi::Either::B(inner) => ferridriver::options::LocatorLike::Selector(inner.selector),
    };
    let opts = options.map(ferridriver::options::FilterOptions::from);
    Self::wrap(self.inner.locator(like, opts))
  }

  #[napi]
  pub fn get_by_role(&self, role: String, options: Option<RoleOptions>) -> Locator {
    let opts: ferridriver::options::RoleOptions = options.map_or_else(Default::default, Into::into);
    Self::wrap(self.inner.get_by_role(&role, &opts))
  }

  #[napi]
  pub fn get_by_text(&self, text: String, options: Option<TextOptions>) -> Locator {
    let opts: ferridriver::options::TextOptions = options.map_or_else(Default::default, Into::into);
    Self::wrap(self.inner.get_by_text(&text, &opts))
  }

  #[napi]
  pub fn get_by_label(&self, text: String, options: Option<TextOptions>) -> Locator {
    let opts: ferridriver::options::TextOptions = options.map_or_else(Default::default, Into::into);
    Self::wrap(self.inner.get_by_label(&text, &opts))
  }

  #[napi]
  pub fn get_by_placeholder(&self, text: String, options: Option<TextOptions>) -> Locator {
    let opts: ferridriver::options::TextOptions = options.map_or_else(Default::default, Into::into);
    Self::wrap(self.inner.get_by_placeholder(&text, &opts))
  }

  #[napi]
  pub fn get_by_alt_text(&self, text: String, options: Option<TextOptions>) -> Locator {
    let opts: ferridriver::options::TextOptions = options.map_or_else(Default::default, Into::into);
    Self::wrap(self.inner.get_by_alt_text(&text, &opts))
  }

  #[napi]
  pub fn get_by_title(&self, text: String, options: Option<TextOptions>) -> Locator {
    let opts: ferridriver::options::TextOptions = options.map_or_else(Default::default, Into::into);
    Self::wrap(self.inner.get_by_title(&text, &opts))
  }

  #[napi]
  pub fn get_by_test_id(&self, test_id: String) -> Locator {
    Self::wrap(self.inner.get_by_test_id(&test_id))
  }

  #[napi]
  pub fn first(&self) -> Locator {
    Self::wrap(self.inner.first())
  }

  #[napi]
  pub fn last(&self) -> Locator {
    Self::wrap(self.inner.last())
  }

  #[napi]
  pub fn nth(&self, index: i32) -> Locator {
    Self::wrap(self.inner.nth(index))
  }

  #[napi]
  pub fn filter(&self, options: FilterOptions) -> Locator {
    Self::wrap(self.inner.filter(&ferridriver::options::FilterOptions::from(options)))
  }

  /// Intersection: matches the same element that both locators match.
  /// Mirrors Playwright's `locator.and(other)`.
  #[napi]
  pub fn and(&self, other: &Locator) -> Locator {
    Self::wrap(self.inner.and(&other.inner))
  }

  /// Union: matches elements resolved by either locator. Mirrors
  /// Playwright's `locator.or(other)`.
  #[napi]
  pub fn or(&self, other: &Locator) -> Locator {
    Self::wrap(self.inner.or(&other.inner))
  }

  // ── Actions ─────────────────────────────────────────────────────────────

  /// Click the element matched by this locator. Accepts Playwright's
  /// full `LocatorClickOptions` bag — see
  /// `/tmp/playwright/packages/playwright-core/types/types.d.ts:12986`.
  #[napi]
  pub async fn click(&self, options: Option<crate::types::ClickOptions>) -> Result<()> {
    let opts = options.map(TryInto::try_into).transpose()?;
    self.inner.click(opts).await.map_err(napi::Error::from_reason)
  }

  /// Double-click the element matched by this locator. Accepts
  /// Playwright's full `LocatorDblClickOptions` bag.
  #[napi]
  pub async fn dblclick(&self, options: Option<crate::types::DblClickOptions>) -> Result<()> {
    let opts = options.map(TryInto::try_into).transpose()?;
    self.inner.dblclick(opts).await.map_err(napi::Error::from_reason)
  }

  /// Fill an input with `value`. Accepts Playwright's full
  /// `LocatorFillOptions` bag.
  #[napi]
  pub async fn fill(&self, value: String, options: Option<crate::types::FillOptions>) -> Result<()> {
    let opts = options.map(Into::into);
    self.inner.fill(&value, opts).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn clear(&self) -> Result<()> {
    self.inner.clear().await.map_err(napi::Error::from_reason)
  }

  /// Type `text` character-by-character. Accepts Playwright's full
  /// `LocatorTypeOptions` bag. Deprecated in Playwright in favor of
  /// `pressSequentially`; both call paths are identical in ferridriver.
  #[napi]
  pub async fn type_text(&self, text: String, options: Option<crate::types::TypeOptions>) -> Result<()> {
    let opts = options.map(Into::into);
    self.inner.r#type(&text, opts).await.map_err(napi::Error::from_reason)
  }

  #[napi(js_name = "type")]
  pub async fn type_alias(&self, text: String, options: Option<crate::types::TypeOptions>) -> Result<()> {
    let opts = options.map(Into::into);
    self.inner.r#type(&text, opts).await.map_err(napi::Error::from_reason)
  }

  /// Press a key or key combination. Accepts Playwright's full
  /// `LocatorPressOptions` bag.
  #[napi]
  pub async fn press(&self, key: String, options: Option<crate::types::PressOptions>) -> Result<()> {
    let opts = options.map(Into::into);
    self.inner.press(&key, opts).await.map_err(napi::Error::from_reason)
  }

  /// Hover over the element matched by this locator. Accepts
  /// Playwright's full `LocatorHoverOptions` bag.
  #[napi]
  pub async fn hover(&self, options: Option<crate::types::HoverOptions>) -> Result<()> {
    let opts = options.map(TryInto::try_into).transpose()?;
    self.inner.hover(opts).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn focus(&self) -> Result<()> {
    self.inner.focus().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn blur(&self) -> Result<()> {
    self.inner.blur().await.map_err(napi::Error::from_reason)
  }

  /// Check a checkbox or radio. Accepts Playwright's full
  /// `LocatorCheckOptions` bag.
  #[napi]
  pub async fn check(&self, options: Option<crate::types::CheckOptions>) -> Result<()> {
    let opts = options.map(Into::into);
    self.inner.check(opts).await.map_err(napi::Error::from_reason)
  }

  /// Uncheck a checkbox. Accepts Playwright's full `LocatorUncheckOptions` bag.
  #[napi]
  pub async fn uncheck(&self, options: Option<crate::types::CheckOptions>) -> Result<()> {
    let opts = options.map(Into::into);
    self.inner.uncheck(opts).await.map_err(napi::Error::from_reason)
  }

  /// Select options on a `<select>` element. Accepts Playwright's
  /// full `string | string[] | { value?, label?, index? } | Array<...>`
  /// union plus the `LocatorSelectOptionOptions` bag.
  #[napi]
  pub async fn select_option(
    &self,
    values: crate::types::NapiSelectOptionInput,
    options: Option<crate::types::SelectOptionOptions>,
  ) -> Result<Vec<String>> {
    let opts = options.map(Into::into);
    self
      .inner
      .select_option(values.0, opts)
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn scroll_into_view(&self) -> Result<()> {
    self
      .inner
      .scroll_into_view_if_needed()
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi(js_name = "scrollIntoViewIfNeeded")]
  pub async fn scroll_into_view_if_needed(&self) -> Result<()> {
    self.scroll_into_view().await
  }

  /// Dispatch a DOM event of `type` on this element with an optional
  /// `eventInit` dict. Mirrors Playwright's
  /// `locator.dispatchEvent(type, eventInit?, options?)`.
  #[napi]
  pub async fn dispatch_event(
    &self,
    event_type: String,
    event_init: Option<serde_json::Value>,
    options: Option<crate::types::DispatchEventOptions>,
  ) -> Result<()> {
    self
      .inner
      .dispatch_event(&event_type, event_init, options.map(Into::into))
      .await
      .map_err(napi::Error::from_reason)
  }

  /// Type `text` character-by-character. Accepts Playwright's full
  /// `LocatorPressSequentiallyOptions` (same shape as `TypeOptions`).
  #[napi]
  pub async fn press_sequentially(&self, text: String, options: Option<crate::types::TypeOptions>) -> Result<()> {
    let opts = options.map(Into::into);
    self
      .inner
      .press_sequentially(&text, opts)
      .await
      .map_err(napi::Error::from_reason)
  }

  // ── Content & state ─────────────────────────────────────────────────────

  #[napi]
  pub async fn text_content(&self) -> Result<Option<String>> {
    self.inner.text_content().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn inner_text(&self) -> Result<String> {
    self.inner.inner_text().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn inner_html(&self) -> Result<String> {
    self.inner.inner_html().await.map_err(napi::Error::from_reason)
  }

  #[napi(js_name = "innerHTML")]
  pub async fn inner_html_alias(&self) -> Result<String> {
    self.inner.inner_html().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn get_attribute(&self, name: String) -> Result<Option<String>> {
    self.inner.get_attribute(&name).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn input_value(&self) -> Result<String> {
    self.inner.input_value().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn is_visible(&self) -> Result<bool> {
    self.inner.is_visible().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn is_hidden(&self) -> Result<bool> {
    self.inner.is_hidden().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn is_enabled(&self) -> Result<bool> {
    self.inner.is_enabled().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn is_disabled(&self) -> Result<bool> {
    self.inner.is_disabled().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn is_checked(&self) -> Result<bool> {
    self.inner.is_checked().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn is_editable(&self) -> Result<bool> {
    self.inner.is_editable().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn count(&self) -> Result<i32> {
    let n = self.inner.count().await.map_err(napi::Error::from_reason)?;
    i32::try_from(n).map_err(|_| napi::Error::from_reason(format!("element count {n} exceeds i32::MAX")))
  }

  #[napi]
  pub async fn bounding_box(&self) -> Result<Option<BoundingBox>> {
    let bb = self.inner.bounding_box().await.map_err(napi::Error::from_reason)?;
    Ok(bb.map(|b| BoundingBox {
      x: b.x,
      y: b.y,
      width: b.width,
      height: b.height,
    }))
  }

  /// Drag this element to `target`. Mirrors Playwright's
  /// `locator.dragTo(target, options?)` per
  /// `/tmp/playwright/packages/playwright-core/types/types.d.ts:13293`.
  #[napi(js_name = "dragTo")]
  pub async fn drag_to(&self, target: &Locator, options: Option<crate::types::DragAndDropOptions>) -> Result<()> {
    let opts = options.map(Into::into);
    self.inner.drag_to(&target.inner, opts).await.into_napi()
  }

  // ── Waiting ─────────────────────────────────────────────────────────────

  #[napi]
  pub async fn wait_for(&self, options: Option<WaitOptions>) -> Result<()> {
    let opts: ferridriver::options::WaitOptions = options.map_or_else(Default::default, Into::into);
    self.inner.wait_for(opts).await.map_err(napi::Error::from_reason)
  }

  // ── Screenshot ──────────────────────────────────────────────────────────

  #[napi]
  pub async fn screenshot(&self) -> Result<Buffer> {
    let bytes = self.inner.screenshot().await.map_err(napi::Error::from_reason)?;
    Ok(bytes.into())
  }

  // ── All matches ─────────────────────────────────────────────────────────

  #[napi]
  pub async fn all_text_contents(&self) -> Result<Vec<String>> {
    self.inner.all_text_contents().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn all_inner_texts(&self) -> Result<Vec<String>> {
    self.inner.all_inner_texts().await.map_err(napi::Error::from_reason)
  }

  // ── Missing methods ─────────────────────────────────────────────────────

  #[napi]
  pub async fn right_click(&self) -> Result<()> {
    self.inner.right_click().await.map_err(napi::Error::from_reason)
  }

  /// Tap (touch event) the element matched by this locator. Accepts
  /// Playwright's full `LocatorTapOptions` bag.
  #[napi]
  pub async fn tap(&self, options: Option<crate::types::TapOptions>) -> Result<()> {
    let opts = options.map(TryInto::try_into).transpose()?;
    self.inner.tap(opts).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn select_text(&self) -> Result<()> {
    self.inner.select_text().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn set_checked(&self, checked: bool, options: Option<crate::types::CheckOptions>) -> Result<()> {
    let opts = options.map(Into::into);
    self
      .inner
      .set_checked(checked, opts)
      .await
      .map_err(napi::Error::from_reason)
  }

  /// Set files on a `<input type=file>` element. Accepts Playwright's
  /// full `string | string[] | FilePayload | FilePayload[]` union plus
  /// the `LocatorSetInputFilesOptions` bag.
  #[napi]
  pub async fn set_input_files(
    &self,
    files: crate::types::NapiInputFiles,
    options: Option<crate::types::SetInputFilesOptions>,
  ) -> Result<()> {
    let opts = options.map(Into::into);
    self
      .inner
      .set_input_files(files.0, opts)
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn is_attached(&self) -> Result<bool> {
    self.inner.is_attached().await.map_err(napi::Error::from_reason)
  }

  /// Playwright: `locator.evaluate(pageFunction, arg?, options?): Promise<R>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/locator.ts:129`).
  #[napi(
    ts_args_type = "pageFunction: string | Function, arg?: unknown, options?: { timeout?: number }",
    ts_return_type = "Promise<unknown>"
  )]
  pub async fn evaluate(
    &self,
    page_function: crate::types::NapiPageFunction,
    arg: Option<crate::types::NapiEvaluateArg>,
    options: Option<crate::types::EvaluateOptions>,
  ) -> Result<crate::serialize_out::Evaluated> {
    let serialized = crate::page::build_serialized_argument(arg);
    let opts = options.map(Into::into);
    let result = self
      .inner
      .evaluate(&page_function.source, serialized, page_function.is_function, opts)
      .await
      .into_napi()?;
    Ok(crate::serialize_out::Evaluated(result))
  }

  /// Playwright: `locator.evaluateHandle(pageFunction, arg?, options?): Promise<JSHandle>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/locator.ts:138`).
  #[napi(ts_args_type = "pageFunction: string | Function, arg?: unknown, options?: { timeout?: number }")]
  pub async fn evaluate_handle(
    &self,
    page_function: crate::types::NapiPageFunction,
    arg: Option<crate::types::NapiEvaluateArg>,
    options: Option<crate::types::EvaluateOptions>,
  ) -> Result<crate::js_handle::JSHandle> {
    let serialized = crate::page::build_serialized_argument(arg);
    let opts = options.map(Into::into);
    let handle = self
      .inner
      .evaluate_handle(&page_function.source, serialized, page_function.is_function, opts)
      .await
      .into_napi()?;
    Ok(crate::js_handle::JSHandle::wrap(handle))
  }

  /// Playwright: `locator.evaluateAll(pageFunction, arg?): Promise<R>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/locator.ts:133`).
  #[napi(
    ts_args_type = "pageFunction: string | Function, arg?: unknown",
    ts_return_type = "Promise<unknown>"
  )]
  pub async fn evaluate_all(
    &self,
    page_function: crate::types::NapiPageFunction,
    arg: Option<crate::types::NapiEvaluateArg>,
  ) -> Result<crate::serialize_out::Evaluated> {
    let serialized = crate::page::build_serialized_argument(arg);
    let result = self
      .inner
      .evaluate_all(&page_function.source, serialized, page_function.is_function)
      .await
      .into_napi()?;
    Ok(crate::serialize_out::Evaluated(result))
  }

  #[napi]
  pub fn or_locator(&self, other: &Locator) -> Locator {
    Locator {
      inner: self.inner.or(&other.inner),
    }
  }

  #[napi]
  pub fn and_locator(&self, other: &Locator) -> Locator {
    Locator {
      inner: self.inner.and(&other.inner),
    }
  }

  #[napi]
  pub async fn all(&self) -> Result<Vec<Locator>> {
    let locators = self.inner.all().await.map_err(napi::Error::from_reason)?;
    Ok(locators.into_iter().map(|l| Locator { inner: l }).collect())
  }

  // ── Expect assertions (delegates to Rust core, all polling in Rust) ──

  #[napi]
  pub async fn expect_visible(&self, not: Option<bool>, timeout_ms: Option<f64>) -> Result<()> {
    let mut e = ferridriver_test::expect::expect(&self.inner);
    if not.unwrap_or(false) {
      e = e.not();
    }
    if let Some(t) = timeout_ms {
      e = e.with_timeout(std::time::Duration::from_millis(t as u64));
    }
    e.to_be_visible().await.map_err(|e| napi::Error::from_reason(e.message))
  }

  #[napi]
  pub async fn expect_hidden(&self, not: Option<bool>, timeout_ms: Option<f64>) -> Result<()> {
    let mut e = ferridriver_test::expect::expect(&self.inner);
    if not.unwrap_or(false) {
      e = e.not();
    }
    if let Some(t) = timeout_ms {
      e = e.with_timeout(std::time::Duration::from_millis(t as u64));
    }
    e.to_be_hidden().await.map_err(|e| napi::Error::from_reason(e.message))
  }

  #[napi]
  pub async fn expect_enabled(&self, not: Option<bool>, timeout_ms: Option<f64>) -> Result<()> {
    let mut e = ferridriver_test::expect::expect(&self.inner);
    if not.unwrap_or(false) {
      e = e.not();
    }
    if let Some(t) = timeout_ms {
      e = e.with_timeout(std::time::Duration::from_millis(t as u64));
    }
    e.to_be_enabled().await.map_err(|e| napi::Error::from_reason(e.message))
  }

  #[napi]
  pub async fn expect_disabled(&self, not: Option<bool>, timeout_ms: Option<f64>) -> Result<()> {
    let mut e = ferridriver_test::expect::expect(&self.inner);
    if not.unwrap_or(false) {
      e = e.not();
    }
    if let Some(t) = timeout_ms {
      e = e.with_timeout(std::time::Duration::from_millis(t as u64));
    }
    e.to_be_disabled()
      .await
      .map_err(|e| napi::Error::from_reason(e.message))
  }

  #[napi]
  pub async fn expect_checked(&self, not: Option<bool>, timeout_ms: Option<f64>) -> Result<()> {
    let mut e = ferridriver_test::expect::expect(&self.inner);
    if not.unwrap_or(false) {
      e = e.not();
    }
    if let Some(t) = timeout_ms {
      e = e.with_timeout(std::time::Duration::from_millis(t as u64));
    }
    e.to_be_checked().await.map_err(|e| napi::Error::from_reason(e.message))
  }

  #[napi]
  pub async fn expect_text(&self, expected: String, not: Option<bool>, timeout_ms: Option<f64>) -> Result<()> {
    let mut e = ferridriver_test::expect::expect(&self.inner);
    if not.unwrap_or(false) {
      e = e.not();
    }
    if let Some(t) = timeout_ms {
      e = e.with_timeout(std::time::Duration::from_millis(t as u64));
    }
    e.to_have_text(expected.as_str())
      .await
      .map_err(|e| napi::Error::from_reason(e.message))
  }

  #[napi]
  pub async fn expect_contain_text(&self, expected: String, not: Option<bool>, timeout_ms: Option<f64>) -> Result<()> {
    let mut e = ferridriver_test::expect::expect(&self.inner);
    if not.unwrap_or(false) {
      e = e.not();
    }
    if let Some(t) = timeout_ms {
      e = e.with_timeout(std::time::Duration::from_millis(t as u64));
    }
    e.to_contain_text(expected.as_str())
      .await
      .map_err(|e| napi::Error::from_reason(e.message))
  }

  #[napi]
  pub async fn expect_value(&self, expected: String, not: Option<bool>, timeout_ms: Option<f64>) -> Result<()> {
    let mut e = ferridriver_test::expect::expect(&self.inner);
    if not.unwrap_or(false) {
      e = e.not();
    }
    if let Some(t) = timeout_ms {
      e = e.with_timeout(std::time::Duration::from_millis(t as u64));
    }
    e.to_have_value(expected.as_str())
      .await
      .map_err(|e| napi::Error::from_reason(e.message))
  }

  #[napi]
  pub async fn expect_attribute(
    &self,
    name: String,
    value: String,
    not: Option<bool>,
    timeout_ms: Option<f64>,
  ) -> Result<()> {
    let mut e = ferridriver_test::expect::expect(&self.inner);
    if not.unwrap_or(false) {
      e = e.not();
    }
    if let Some(t) = timeout_ms {
      e = e.with_timeout(std::time::Duration::from_millis(t as u64));
    }
    e.to_have_attribute(&name, value.as_str())
      .await
      .map_err(|e| napi::Error::from_reason(e.message))
  }

  #[napi]
  pub async fn expect_count(&self, expected: i32, not: Option<bool>, timeout_ms: Option<f64>) -> Result<()> {
    let mut e = ferridriver_test::expect::expect(&self.inner);
    if not.unwrap_or(false) {
      e = e.not();
    }
    if let Some(t) = timeout_ms {
      e = e.with_timeout(std::time::Duration::from_millis(t as u64));
    }
    e.to_have_count(expected as usize)
      .await
      .map_err(|e| napi::Error::from_reason(e.message))
  }
}
