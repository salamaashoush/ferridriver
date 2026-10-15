//! ElementHandle class -- NAPI binding for `ferridriver::ElementHandle`.
//!
//! Mirrors Playwright's `ElementHandle<T extends Node>` interface
//! (`/tmp/playwright/packages/playwright-core/types/types.d.ts`).
//!
//! The phase-C surface covers lifecycle (`dispose`, `isDisposed`,
//! `asJsHandle`) so the per-backend release paths can be exercised from
//! JS via `page.query_selector` + `handle.dispose()`. Phase E bolts the
//! Playwright DOM methods on top of this same class.

use crate::abort::Bag;
use crate::error::IntoNapi;
use napi::Result;
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

/// Axis-aligned bounding rectangle returned by
/// [`ElementHandle::boundingBox`]. Mirrors Playwright's `BoundingBox`.
#[napi(object)]
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

/// Handle to a DOM element living in a page.
///
/// Created via `page.querySelector(selector)` — phase F adds
/// `page.querySelectorAll`, `locator.elementHandle`, and
/// `locator.elementHandles` as additional materialisation paths.
#[napi]
pub struct ElementHandle {
  inner: ferridriver::ElementHandle,
}

impl ElementHandle {
  pub(crate) fn wrap(inner: ferridriver::ElementHandle) -> Self {
    Self { inner }
  }

  #[allow(dead_code)]
  pub(crate) fn inner_ref(&self) -> &ferridriver::ElementHandle {
    &self.inner
  }
}

#[napi]
impl ElementHandle {
  /// `true` once [`Self::dispose`] has run for this handle (or any clone
  /// sharing the same remote). Playwright:
  /// `elementHandle.isDisposed` — exposed as a `boolean` getter here to
  /// match the JS convention.
  #[napi(getter)]
  pub fn is_disposed(&self) -> bool {
    self.inner.is_disposed()
  }

  /// Release the underlying remote element. See
  /// [`crate::js_handle::JSHandle::dispose`] for semantics.
  #[napi]
  pub async fn dispose(&self) -> Result<()> {
    self.inner.dispose().await.into_napi()
  }

  /// Return this handle as a general `JSHandle`. Playwright:
  /// `elementHandle` is-a `JSHandle`, so the cast is always infallible
  /// — we surface a companion `JSHandle` wrapping the same remote.
  /// The two handles share the same dispose flag: disposing either
  /// releases the remote.
  #[napi]
  pub fn as_js_handle(&self) -> crate::js_handle::JSHandle {
    crate::js_handle::JSHandle::wrap(self.inner.as_js_handle().clone())
  }

  /// Playwright: `elementHandle.evaluate(pageFunction, arg?): Promise<R>`.
  /// Delegates through the companion `JSHandle`.
  #[napi(
    ts_args_type = "pageFunction: string | Function, arg?: unknown, options?: ExposeFunctionsOptions",
    ts_return_type = "Promise<unknown>"
  )]
  pub async fn evaluate(
    &self,
    page_function: crate::types::NapiPageFunction,
    arg: Option<crate::types::NapiEvaluateArg>,
    options: Option<crate::types::ExposeFunctionsOptions>,
  ) -> Result<crate::serialize_out::Evaluated> {
    let serialized =
      crate::page::build_evaluate_argument(arg, options.and_then(|o| o.expose_functions).unwrap_or(false))?;
    let result = self
      .inner
      .as_js_handle()
      .evaluate(&page_function.source, serialized, page_function.is_function)
      .await
      .into_napi()?;
    Ok(crate::serialize_out::Evaluated(result))
  }

  /// Playwright: `elementHandle.evaluateHandle(pageFunction, arg?): Promise<JSHandle>`.
  #[napi(ts_args_type = "pageFunction: string | Function, arg?: unknown, options?: ExposeFunctionsOptions")]
  pub async fn evaluate_handle(
    &self,
    page_function: crate::types::NapiPageFunction,
    arg: Option<crate::types::NapiEvaluateArg>,
    options: Option<crate::types::ExposeFunctionsOptions>,
  ) -> Result<crate::js_handle::JSHandle> {
    let serialized =
      crate::page::build_evaluate_argument(arg, options.and_then(|o| o.expose_functions).unwrap_or(false))?;
    let handle = self
      .inner
      .as_js_handle()
      .evaluate_handle(&page_function.source, serialized, page_function.is_function)
      .await
      .into_napi()?;
    Ok(crate::js_handle::JSHandle::wrap(handle))
  }

  // ── Content reads (Phase E) ──────────────────────────────────────────

  /// Playwright: `elementHandle.innerHTML(): Promise<string>`.
  #[napi(js_name = "innerHTML")]
  pub async fn inner_html(&self) -> Result<String> {
    self.inner.inner_html().await.into_napi()
  }

  /// Playwright: `elementHandle.innerText(): Promise<string>`.
  #[napi]
  pub async fn inner_text(&self) -> Result<String> {
    self.inner.inner_text().await.into_napi()
  }

  /// Playwright: `elementHandle.textContent(): Promise<string | null>`.
  #[napi]
  pub async fn text_content(&self) -> Result<Option<String>> {
    self.inner.text_content().await.into_napi()
  }

  /// Playwright: `elementHandle.getAttribute(name): Promise<string | null>`.
  #[napi]
  pub async fn get_attribute(&self, name: String) -> Result<Option<String>> {
    self.inner.get_attribute(&name).await.into_napi()
  }

  /// Playwright: `elementHandle.inputValue(): Promise<string>`.
  #[napi]
  pub async fn input_value(&self) -> Result<String> {
    self.inner.input_value().await.into_napi()
  }

  // ── State predicates (Phase E) ───────────────────────────────────────

  #[napi]
  pub async fn is_visible(&self) -> Result<bool> {
    self.inner.is_visible().await.into_napi()
  }

  #[napi]
  pub async fn is_hidden(&self) -> Result<bool> {
    self.inner.is_hidden().await.into_napi()
  }

  #[napi]
  pub async fn is_disabled(&self) -> Result<bool> {
    self.inner.is_disabled().await.into_napi()
  }

  #[napi]
  pub async fn is_enabled(&self) -> Result<bool> {
    self.inner.is_enabled().await.into_napi()
  }

  #[napi]
  pub async fn is_checked(&self) -> Result<bool> {
    self.inner.is_checked().await.into_napi()
  }

  #[napi]
  pub async fn is_editable(&self) -> Result<bool> {
    self.inner.is_editable().await.into_napi()
  }

  // ── Geometry (Phase E) ───────────────────────────────────────────────

  /// Playwright: `elementHandle.boundingBox(): Promise<BoundingBox | null>`.
  #[napi]
  pub async fn bounding_box(&self) -> Result<Option<BoundingBox>> {
    Ok(self.inner.bounding_box().await.into_napi()?.map(Into::into))
  }

  // ── Actions (Phase E) ────────────────────────────────────────────────

  #[napi]
  pub async fn click(&self, options: Option<crate::types::ClickOptions>) -> Result<()> {
    self.inner.click().bag(options)?.await.into_napi()
  }

  #[napi]
  pub async fn dblclick(&self, options: Option<crate::types::DblClickOptions>) -> Result<()> {
    self.inner.dblclick().bag(options)?.await.into_napi()
  }

  #[napi]
  pub async fn hover(&self, options: Option<crate::types::HoverOptions>) -> Result<()> {
    self.inner.hover().bag(options)?.await.into_napi()
  }

  #[napi]
  pub async fn type_str(&self, text: String, options: Option<crate::types::TypeOptions>) -> Result<()> {
    self.inner.type_str(&text).bag(options)?.await.into_napi()
  }

  #[napi]
  pub async fn focus(&self) -> Result<()> {
    self.inner.focus().await.into_napi()
  }

  #[napi]
  pub async fn scroll_into_view_if_needed(&self) -> Result<()> {
    self.inner.scroll_into_view_if_needed().await.into_napi()
  }

  /// Playwright: `elementHandle.screenshot(options?: ElementHandleScreenshotOptions): Promise<Buffer>`.
  #[napi]
  pub async fn screenshot(&self, mut options: Option<crate::types::ElementScreenshotOptions>) -> Result<Buffer> {
    let signal = crate::abort::take(&mut options);
    let opts = options.unwrap_or_default().into_core(self.inner.page());
    let bytes = self
      .inner
      .screenshot()
      .options(opts)
      .maybe_signal(signal)
      .await
      .into_napi()?;
    Ok(bytes.into())
  }

  // ── $eval / $$eval (Playwright parity) ───────────────────────────────

  /// Playwright: `elementHandle.$eval(selector, pageFunction, arg?): Promise<R>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:215`).
  #[napi(
    js_name = "$eval",
    ts_args_type = "selector: string, pageFunction: string | Function, arg?: unknown",
    ts_return_type = "Promise<unknown>"
  )]
  pub async fn dollar_eval(
    &self,
    selector: String,
    page_function: crate::types::NapiPageFunction,
    arg: Option<crate::types::NapiEvaluateArg>,
  ) -> Result<crate::serialize_out::Evaluated> {
    let serialized = crate::page::build_serialized_argument(arg)?;
    let result = self
      .inner
      .eval_on_selector(&selector, &page_function.source, serialized)
      .await
      .into_napi()?;
    Ok(crate::serialize_out::Evaluated(result))
  }

  /// Playwright: `elementHandle.$$eval(selector, pageFunction, arg?): Promise<R>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:220`).
  #[napi(
    js_name = "$$eval",
    ts_args_type = "selector: string, pageFunction: string | Function, arg?: unknown",
    ts_return_type = "Promise<unknown>"
  )]
  pub async fn dollar_dollar_eval(
    &self,
    selector: String,
    page_function: crate::types::NapiPageFunction,
    arg: Option<crate::types::NapiEvaluateArg>,
  ) -> Result<crate::serialize_out::Evaluated> {
    let serialized = crate::page::build_serialized_argument(arg)?;
    let result = self
      .inner
      .eval_on_selector_all(&selector, &page_function.source, serialized)
      .await
      .into_napi()?;
    Ok(crate::serialize_out::Evaluated(result))
  }

  // ── $ / $$ (query shortcuts — Playwright parity) ─────────────────────

  /// Playwright: `elementHandle.$(selector): Promise<ElementHandle | null>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:206`).
  #[napi(js_name = "$")]
  pub async fn query_selector(&self, selector: String) -> Result<Option<ElementHandle>> {
    let maybe = self.inner.query_selector(&selector).await.into_napi()?;
    Ok(maybe.map(ElementHandle::wrap))
  }

  /// Playwright: `elementHandle.$$(selector): Promise<ElementHandle[]>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:210`).
  #[napi(js_name = "$$")]
  pub async fn query_selector_all(&self, selector: String) -> Result<Vec<ElementHandle>> {
    let handles = self.inner.query_selector_all(&selector).await.into_napi()?;
    Ok(handles.into_iter().map(ElementHandle::wrap).collect())
  }

  // ── Frame accessors ──────────────────────────────────────────────────

  /// Playwright: `elementHandle.ownerFrame(): Promise<Frame | null>`.
  #[napi(ts_return_type = "Promise<Frame | null>")]
  pub async fn owner_frame(&self) -> Result<Option<crate::identity::FrameObject>> {
    let maybe = self.inner.owner_frame().await.into_napi()?;
    Ok(maybe.map(crate::identity::FrameObject))
  }

  /// Playwright: `elementHandle.contentFrame(): Promise<Frame | null>`.
  #[napi(ts_return_type = "Promise<Frame | null>")]
  pub async fn content_frame(&self) -> Result<Option<crate::identity::FrameObject>> {
    let maybe = self.inner.content_frame().await.into_napi()?;
    Ok(maybe.map(crate::identity::FrameObject))
  }

  // ── wait_for_* helpers ───────────────────────────────────────────────

  /// Playwright: `elementHandle.waitForElementState(state, options?: { timeout? })`.
  #[napi]
  pub async fn wait_for_element_state(
    &self,
    state: String,
    mut options: Option<crate::types::ElementHandleWaitOptions>,
  ) -> Result<()> {
    let st = ferridriver::ElementState::parse(&state).into_napi()?;
    let signal = crate::abort::take(&mut options);
    let timeout = options.and_then(|o| o.timeout).map(crate::types::f64_to_u64);
    crate::abort::run(signal, self.inner.wait_for_element_state(st, timeout)).await
  }

  /// Playwright: `elementHandle.waitForSelector(selector, options?: { timeout? })`.
  #[napi]
  pub async fn wait_for_selector(
    &self,
    selector: String,
    mut options: Option<crate::types::ElementHandleWaitOptions>,
  ) -> Result<Option<ElementHandle>> {
    let signal = crate::abort::take(&mut options);
    let timeout = options.and_then(|o| o.timeout).map(crate::types::f64_to_u64);
    let maybe = crate::abort::run(signal, self.inner.wait_for_selector(&selector, timeout)).await?;
    Ok(maybe.map(ElementHandle::wrap))
  }

  // ── Action methods (temp-tag bridge — Playwright parity) ─────────────

  /// Playwright: `elementHandle.fill(value, options?)`.
  #[napi]
  pub async fn fill(&self, value: String, options: Option<crate::types::FillOptions>) -> Result<()> {
    self.inner.fill(&value).bag(options)?.await.into_napi()
  }

  /// Playwright: `elementHandle.check(options?)`.
  #[napi]
  pub async fn check(&self, options: Option<crate::types::CheckOptions>) -> Result<()> {
    self.inner.check().bag(options)?.await.into_napi()
  }

  /// Playwright: `elementHandle.uncheck(options?)`.
  #[napi]
  pub async fn uncheck(&self, options: Option<crate::types::CheckOptions>) -> Result<()> {
    self.inner.uncheck().bag(options)?.await.into_napi()
  }

  /// Playwright: `elementHandle.setChecked(checked, options?)`.
  #[napi]
  pub async fn set_checked(&self, checked: bool, options: Option<crate::types::CheckOptions>) -> Result<()> {
    self.inner.set_checked(checked).bag(options)?.await.into_napi()
  }

  /// Playwright: `elementHandle.tap(options?)`.
  #[napi]
  pub async fn tap(&self, options: Option<crate::types::TapOptions>) -> Result<()> {
    self.inner.tap().bag(options)?.await.into_napi()
  }

  /// Playwright: `elementHandle.press(key, options?)`.
  #[napi]
  pub async fn press(&self, key: String, options: Option<crate::types::PressOptions>) -> Result<()> {
    self.inner.press(&key).bag(options)?.await.into_napi()
  }

  /// Playwright: `elementHandle.dispatchEvent(type, eventInit?)`.
  #[napi]
  pub async fn dispatch_event(
    &self,
    event_type: String,
    event_init: Option<serde_json::Value>,
    options: Option<crate::types::DispatchEventOptions>,
  ) -> Result<()> {
    self
      .inner
      .dispatch_event(&event_type, event_init)
      .bag(options)?
      .await
      .into_napi()
  }

  /// Playwright: `elementHandle.selectOption(values, options?)`.
  #[napi]
  pub async fn select_option(
    &self,
    values: crate::types::NapiSelectOptionInput,
    options: Option<crate::types::SelectOptionOptions>,
  ) -> Result<Vec<String>> {
    self.inner.select_option(values.0).bag(options)?.await.into_napi()
  }

  /// Playwright: `elementHandle.selectText(options?)`.
  #[napi]
  pub async fn select_text(&self) -> Result<()> {
    self.inner.select_text().await.into_napi()
  }

  /// Playwright: `elementHandle.setInputFiles(files, options?)`.
  #[napi(ts_args_type = "files: string | string[] | FilePayload | FilePayload[], options?: SetInputFilesOptions")]
  pub async fn set_input_files(
    &self,
    files: crate::types::NapiInputFiles,
    options: Option<crate::types::SetInputFilesOptions>,
  ) -> Result<()> {
    self.inner.set_input_files(files.0).bag(options)?.await.into_napi()
  }
}
