//! `ElementHandle` class -- NAPI binding for `ferridriver::ElementHandle`.
//!
//! Mirrors Playwright's `ElementHandle<T extends Node>` interface
//! (`/tmp/playwright/packages/playwright-core/types/types.d.ts`).

use crate::error::IntoNapi;
use napi::Result;
use napi_derive::napi;

/// Handle to a DOM element living in a page.
#[napi]
pub struct ElementHandle {
  inner: ferridriver::ElementHandle,
}

impl ElementHandle {
  pub(crate) fn wrap(inner: ferridriver::ElementHandle) -> Self {
    Self { inner }
  }

  pub(crate) fn inner_ref(&self) -> &ferridriver::ElementHandle {
    &self.inner
  }
}

crate::macros::napi_element_actions!(ElementHandle);

#[napi]
impl ElementHandle {
  #[napi(getter)]
  pub fn is_disposed(&self) -> bool {
    self.inner.is_disposed()
  }

  #[napi]
  pub async fn dispose(&self) -> Result<()> {
    self.inner.dispose().await.into_napi()
  }

  #[napi]
  pub fn as_js_handle(&self) -> crate::js_handle::JSHandle {
    crate::js_handle::JSHandle::wrap(self.inner.as_js_handle().clone())
  }

  /// Playwright: `elementHandle.evaluate(pageFunction, arg?): Promise<R>`.
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

  // ── Content reads ────────────────────────────────────────────────────

  #[napi(js_name = "innerHTML")]
  pub async fn inner_html(&self) -> Result<String> {
    self.inner.inner_html().await.into_napi()
  }

  #[napi]
  pub async fn inner_text(&self) -> Result<String> {
    self.inner.inner_text().await.into_napi()
  }

  #[napi]
  pub async fn text_content(&self) -> Result<Option<String>> {
    self.inner.text_content().await.into_napi()
  }

  #[napi]
  pub async fn get_attribute(&self, name: String) -> Result<Option<String>> {
    self.inner.get_attribute(&name).await.into_napi()
  }

  #[napi]
  pub async fn input_value(&self) -> Result<String> {
    self.inner.input_value().await.into_napi()
  }

  // ── State predicates ─────────────────────────────────────────────────

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

  #[napi]
  pub async fn type_str(&self, text: String, options: Option<crate::types::TypeOptions>) -> Result<()> {
    crate::abort::Bag::bag(self.inner.type_str(&text), options)?
      .await
      .into_napi()
  }

  // ── $eval / $$eval ───────────────────────────────────────────────────

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

  // ── $ / $$ ───────────────────────────────────────────────────────────

  #[napi(js_name = "$")]
  pub async fn query_selector(&self, selector: String) -> Result<Option<ElementHandle>> {
    let maybe = self.inner.query_selector(&selector).await.into_napi()?;
    Ok(maybe.map(ElementHandle::wrap))
  }

  #[napi(js_name = "$$")]
  pub async fn query_selector_all(&self, selector: String) -> Result<Vec<ElementHandle>> {
    let handles = self.inner.query_selector_all(&selector).await.into_napi()?;
    Ok(handles.into_iter().map(ElementHandle::wrap).collect())
  }

  // ── Frame accessors ──────────────────────────────────────────────────

  #[napi(ts_return_type = "Promise<Frame | null>")]
  pub async fn owner_frame(&self) -> Result<Option<crate::identity::FrameObject>> {
    let maybe = self.inner.owner_frame().await.into_napi()?;
    Ok(maybe.map(crate::identity::FrameObject))
  }

  #[napi(ts_return_type = "Promise<Frame | null>")]
  pub async fn content_frame(&self) -> Result<Option<crate::identity::FrameObject>> {
    let maybe = self.inner.content_frame().await.into_napi()?;
    Ok(maybe.map(crate::identity::FrameObject))
  }

  // ── wait_for_* helpers ───────────────────────────────────────────────

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
}
