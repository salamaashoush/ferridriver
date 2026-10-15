//! Frame class -- NAPI binding for `ferridriver::Frame`.

use crate::abort::Bag;
use crate::error::IntoNapi;
use crate::locator::Locator;
use napi::Result;
use napi_derive::napi;

/// A frame within a page (main frame or iframe).
/// Mirrors Playwright's Frame interface.
#[napi]
pub struct Frame {
  inner: ferridriver::Frame,
}

impl Frame {
  pub(crate) fn wrap(inner: ferridriver::Frame) -> Self {
    Self { inner }
  }
}

#[napi]
impl Frame {
  #[napi(getter)]
  pub fn webmcp(&self) -> crate::web_mcp::WebMcp {
    crate::web_mcp::WebMcp::wrap(self.inner.webmcp())
  }

  /// Frame name (from the `name` attribute of the iframe element).
  /// Playwright: `frame.name(): string` (sync).
  #[napi]
  pub fn name(&self) -> String {
    self.inner.name()
  }

  /// Frame URL. Playwright: `frame.url(): string` (sync).
  #[napi]
  pub fn url(&self) -> String {
    self.inner.url()
  }

  /// Whether this is the main (top-level) frame.
  #[napi]
  pub fn is_main_frame(&self) -> bool {
    self.inner.is_main_frame()
  }

  /// Parent frame. Returns null for the main frame.
  /// Playwright: `frame.parentFrame(): Frame | null` (sync).
  #[napi(ts_return_type = "Frame | null")]
  pub fn parent_frame(&self) -> Option<crate::identity::FrameObject> {
    self.inner.parent_frame().map(crate::identity::FrameObject)
  }

  /// Child frames of this frame.
  /// Playwright: `frame.childFrames(): Frame[]` (sync).
  #[napi(ts_return_type = "Frame[]")]
  pub fn child_frames(&self) -> Vec<crate::identity::FrameObject> {
    self
      .inner
      .child_frames()
      .into_iter()
      .map(crate::identity::FrameObject)
      .collect()
  }

  // ── Evaluation ────────────────────────────────────────────────────────

  /// Playwright: `frame.evaluate(pageFunction, arg?): Promise<R>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/frame.ts:196`).
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
      .evaluate(&page_function.source, serialized, page_function.is_function)
      .await
      .into_napi()?;
    Ok(crate::serialize_out::Evaluated(result))
  }

  /// Playwright: `frame.$eval(selector, pageFunction, arg?): Promise<R>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/frame.ts:242`).
  #[napi(
    js_name = "$eval",
    ts_args_type = "selector: string, pageFunction: string | Function, arg?: unknown",
    ts_return_type = "Promise<unknown>"
  )]
  pub async fn eval_on_selector(
    &self,
    selector: String,
    page_function: crate::types::NapiPageFunction,
    arg: Option<crate::types::NapiEvaluateArg>,
  ) -> Result<crate::serialize_out::Evaluated> {
    let serialized = crate::page::build_serialized_argument(arg)?;
    let result = self
      .inner
      .eval_on_selector(&selector, &page_function.source, serialized, page_function.is_function)
      .await
      .into_napi()?;
    Ok(crate::serialize_out::Evaluated(result))
  }

  /// Playwright: `frame.$$eval(selector, pageFunction, arg?): Promise<R>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/frame.ts:248`).
  #[napi(
    js_name = "$$eval",
    ts_args_type = "selector: string, pageFunction: string | Function, arg?: unknown",
    ts_return_type = "Promise<unknown>"
  )]
  pub async fn eval_on_selector_all(
    &self,
    selector: String,
    page_function: crate::types::NapiPageFunction,
    arg: Option<crate::types::NapiEvaluateArg>,
  ) -> Result<crate::serialize_out::Evaluated> {
    let serialized = crate::page::build_serialized_argument(arg)?;
    let result = self
      .inner
      .eval_on_selector_all(&selector, &page_function.source, serialized, page_function.is_function)
      .await
      .into_napi()?;
    Ok(crate::serialize_out::Evaluated(result))
  }

  /// Playwright: `frame.evaluateHandle(pageFunction, arg?): Promise<JSHandle>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/frame.ts:190`).
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
      .evaluate_handle(&page_function.source, serialized, page_function.is_function)
      .await
      .into_napi()?;
    Ok(crate::js_handle::JSHandle::wrap(handle))
  }

  // ── Locators ──────────────────────────────────────────────────────────

  /// Playwright: `frame.locator(selector, options?: LocatorOptions): Locator`.
  /// Thin delegator to Rust core's `Frame::locator`.
  #[napi]
  pub fn locator(&self, selector: String, options: Option<crate::types::FilterOptions>) -> Locator {
    let opts = options.map(ferridriver::options::FilterOptions::from);
    Locator::wrap(match opts {
      Some(f) => self.inner.locator_with(&selector, &f),
      None => self.inner.locator(&selector),
    })
  }

  #[napi]
  pub fn get_by_role(&self, role: String, options: Option<crate::types::RoleOptions>) -> Locator {
    let opts = options.map(ferridriver::options::RoleOptions::from);
    Locator::wrap(self.inner.get_by_role(role.as_str()).maybe_options(opts).into_locator())
  }

  #[napi(ts_args_type = "text: string | RegExp, options?: TextOptions")]
  pub fn get_by_text(
    &self,
    text: napi::Either<String, crate::types::JsRegExpLike>,
    options: Option<crate::types::TextOptions>,
  ) -> Locator {
    let opts = options.map(ferridriver::options::TextOptions::from);
    Locator::wrap(
      self
        .inner
        .get_by_text(crate::types::getby_input_to_rust(text))
        .maybe_options(opts)
        .into_locator(),
    )
  }

  #[napi(ts_args_type = "testId: string | RegExp")]
  pub fn get_by_test_id(&self, test_id: napi::Either<String, crate::types::JsRegExpLike>) -> Locator {
    Locator::wrap(self.inner.get_by_test_id(crate::types::getby_input_to_rust(test_id)))
  }

  // ── Content ───────────────────────────────────────────────────────────

  /// Playwright: `frame.content(options?: { includeShadow?: boolean }): Promise<string>`.
  #[napi(ts_args_type = "options?: { includeShadow?: boolean }")]
  pub async fn content(&self, options: Option<crate::types::ContentOptions>) -> Result<String> {
    let options = ferridriver::options::ContentOptions {
      include_shadow: options.and_then(|o| o.include_shadow),
    };
    self.inner.content().options(options).await.into_napi()
  }

  #[napi]
  pub async fn title(&self) -> Result<String> {
    self.inner.title().await.into_napi()
  }

  // ── Navigation ────────────────────────────────────────────────────────

  /// Playwright: `frame.goto(url, options?: { referer?, timeout?,
  /// waitUntil? }): Promise<Response | null>`. `null` for a navigation
  /// within the document.
  #[napi(ts_return_type = "Promise<Response | null>")]
  pub async fn goto(
    &self,
    url: String,
    options: Option<crate::types::GotoOptions>,
  ) -> Result<Option<crate::network::Response>> {
    let resp = self.inner.goto(&url).bag(options)?.await.into_napi()?;
    let page = self.inner.page_arc().clone();
    Ok(resp.map(|r| crate::network::Response::from_core_with_page(r, page)))
  }

  // ── Waiting ───────────────────────────────────────────────────────────

  /// Playwright: `frame.waitForLoadState(state?, options?: { timeout? })`.
  #[napi(
    ts_args_type = "state?: 'load' | 'domcontentloaded' | 'networkidle', options?: { timeout?: number, signal?: AbortSignal }"
  )]
  pub async fn wait_for_load_state(
    &self,
    state: Option<String>,
    options: Option<crate::types::WaitForLoadStateOptions>,
  ) -> Result<()> {
    self
      .inner
      .wait_for_load_state(state.as_deref())
      .bag(options)?
      .await
      .into_napi()
  }

  /// Playwright: `frame.waitForURL(url, options?: { timeout?, waitUntil? })`.
  #[napi(
    js_name = "waitForURL",
    ts_args_type = "url: string | RegExp, options?: { timeout?: number, signal?: AbortSignal, waitUntil?: 'load' | 'domcontentloaded' | 'networkidle' | 'commit' }"
  )]
  pub async fn wait_for_url(
    &self,
    url: napi::Either<String, crate::types::JsRegExpLike>,
    options: Option<crate::types::WaitForUrlOptions>,
  ) -> Result<()> {
    let matcher = crate::types::string_or_regex_to_rust(url)?;
    self.inner.wait_for_url(matcher).bag(options)?.await.into_napi()
  }

  /// Playwright: `frame.waitForNavigation(options?: { timeout?, url?,
  /// waitUntil? }): Promise<Response | null>`.
  #[napi(
    ts_args_type = "options?: { timeout?: number, signal?: AbortSignal, url?: string | RegExp, waitUntil?: 'load' | 'domcontentloaded' | 'networkidle' | 'commit' }",
    ts_return_type = "Promise<Response | null>"
  )]
  pub async fn wait_for_navigation(
    &self,
    options: Option<crate::types::WaitForNavigationOptions>,
  ) -> Result<Option<crate::network::Response>> {
    let resp = self.inner.wait_for_navigation().bag(options)?.await.into_napi()?;
    let page = self.inner.page_arc().clone();
    Ok(resp.map(|r| crate::network::Response::from_core_with_page(r, page)))
  }

  /// Playwright: `frame.waitForFunction(pageFunction, arg?, options?: {
  /// polling?, timeout? }): Promise<JSHandle>`.
  #[napi(
    ts_args_type = "pageFunction: string | Function, arg?: unknown, options?: { polling?: number | 'raf', timeout?: number, signal?: AbortSignal }"
  )]
  pub async fn wait_for_function(
    &self,
    page_function: crate::types::NapiPageFunction,
    arg: Option<crate::types::NapiEvaluateArg>,
    mut options: Option<crate::types::WaitForFunctionOptions>,
  ) -> Result<crate::js_handle::JSHandle> {
    let serialized = crate::page::build_serialized_argument(arg)?;
    let signal = crate::abort::take(&mut options);
    let opts = options
      .map(ferridriver::options::WaitForFunctionOptions::try_from)
      .transpose()?;
    let handle = crate::abort::run(
      signal,
      self
        .inner
        .wait_for_function(&page_function.source, serialized, page_function.is_function, opts),
    )
    .await?;
    Ok(crate::js_handle::JSHandle::wrap(handle))
  }

  /// Playwright: `frame.waitForTimeout(timeout): Promise<void>`.
  #[napi]
  pub async fn wait_for_timeout(&self, timeout: f64) {
    self.inner.wait_for_timeout(crate::types::f64_to_u64(timeout)).await;
  }

  // ── Handles ───────────────────────────────────────────────────────────

  /// Playwright: `frame.$(selector, options?: { strict?: boolean }):
  /// Promise<ElementHandle | null>`.
  #[napi(js_name = "$")]
  pub async fn dollar(
    &self,
    selector: String,
    options: Option<crate::types::QuerySelectorOptions>,
  ) -> Result<Option<crate::element_handle::ElementHandle>> {
    let handle = self
      .inner
      .query_selector(&selector, options.and_then(|o| o.strict))
      .await
      .into_napi()?;
    Ok(handle.map(crate::element_handle::ElementHandle::wrap))
  }

  /// Playwright: `frame.$$(selector): Promise<ElementHandle[]>`.
  #[napi(js_name = "$$")]
  pub async fn dollar_dollar(&self, selector: String) -> Result<Vec<crate::element_handle::ElementHandle>> {
    let handles = self.inner.query_selector_all(&selector).await.into_napi()?;
    Ok(
      handles
        .into_iter()
        .map(crate::element_handle::ElementHandle::wrap)
        .collect(),
    )
  }

  /// Playwright: `frame.frameElement(): Promise<ElementHandle>`.
  #[napi]
  pub async fn frame_element(&self) -> Result<crate::element_handle::ElementHandle> {
    let handle = self.inner.frame_element().await.into_napi()?;
    Ok(crate::element_handle::ElementHandle::wrap(handle))
  }

  /// Playwright: `frame.waitForSelector(selector, options?)`. Returns the
  /// matched handle for `state: 'attached' | 'visible'` (default), or
  /// `null` for `state: 'hidden' | 'detached'`.
  #[napi]
  pub async fn wait_for_selector(
    &self,
    selector: String,
    options: Option<crate::types::WaitOptions>,
  ) -> Result<Option<crate::element_handle::ElementHandle>> {
    let handle = self
      .inner
      .wait_for_selector(&selector)
      .bag(options)?
      .await
      .into_napi()?;
    Ok(handle.map(crate::element_handle::ElementHandle::wrap))
  }

  // ── Additional content methods ───────────────────────────────────────

  /// Playwright: `frame.setContent(html, options?: { timeout?, waitUntil? })`.
  #[napi(
    ts_args_type = "html: string, options?: { timeout?: number, signal?: AbortSignal, waitUntil?: 'load' | 'domcontentloaded' | 'networkidle' | 'commit' }"
  )]
  pub async fn set_content(&self, html: String, options: Option<crate::types::SetContentOptions>) -> Result<()> {
    self.inner.set_content(&html).bag(options)?.await.into_napi()
  }

  /// Playwright: `frame.addScriptTag(options?: { content?, path?, type?,
  /// url? }): Promise<ElementHandle>`.
  #[napi(ts_args_type = "options?: { content?: string, path?: string, type?: string, url?: string }")]
  pub async fn add_script_tag(
    &self,
    options: Option<crate::types::AddScriptTagOptions>,
  ) -> Result<crate::element_handle::ElementHandle> {
    let handle = self
      .inner
      .add_script_tag(options.unwrap_or_default().into())
      .await
      .into_napi()?;
    Ok(crate::element_handle::ElementHandle::wrap(handle))
  }

  /// Playwright: `frame.addStyleTag(options?: { content?, path?, url? }):
  /// Promise<ElementHandle>`.
  #[napi(ts_args_type = "options?: { content?: string, path?: string, url?: string }")]
  pub async fn add_style_tag(
    &self,
    options: Option<crate::types::AddStyleTagOptions>,
  ) -> Result<crate::element_handle::ElementHandle> {
    let handle = self
      .inner
      .add_style_tag(options.unwrap_or_default().into())
      .await
      .into_napi()?;
    Ok(crate::element_handle::ElementHandle::wrap(handle))
  }

  /// Whether this frame has been detached from the page.
  /// Playwright: `frame.isDetached(): boolean` (sync).
  #[napi]
  pub fn is_detached(&self) -> bool {
    self.inner.is_detached()
  }

  // ── Additional locators ──────────────────────────────────────────────

  #[napi(ts_args_type = "text: string | RegExp, options?: TextOptions")]
  pub fn get_by_label(
    &self,
    text: napi::Either<String, crate::types::JsRegExpLike>,
    options: Option<crate::types::TextOptions>,
  ) -> Locator {
    let opts = options.map(ferridriver::options::TextOptions::from);
    Locator::wrap(
      self
        .inner
        .get_by_label(crate::types::getby_input_to_rust(text))
        .maybe_options(opts)
        .into_locator(),
    )
  }

  #[napi(ts_args_type = "text: string | RegExp, options?: TextOptions")]
  pub fn get_by_placeholder(
    &self,
    text: napi::Either<String, crate::types::JsRegExpLike>,
    options: Option<crate::types::TextOptions>,
  ) -> Locator {
    let opts = options.map(ferridriver::options::TextOptions::from);
    Locator::wrap(
      self
        .inner
        .get_by_placeholder(crate::types::getby_input_to_rust(text))
        .maybe_options(opts)
        .into_locator(),
    )
  }

  #[napi(ts_args_type = "text: string | RegExp, options?: TextOptions")]
  pub fn get_by_alt_text(
    &self,
    text: napi::Either<String, crate::types::JsRegExpLike>,
    options: Option<crate::types::TextOptions>,
  ) -> Locator {
    let opts = options.map(ferridriver::options::TextOptions::from);
    Locator::wrap(
      self
        .inner
        .get_by_alt_text(crate::types::getby_input_to_rust(text))
        .maybe_options(opts)
        .into_locator(),
    )
  }

  #[napi(ts_args_type = "text: string | RegExp, options?: TextOptions")]
  pub fn get_by_title(
    &self,
    text: napi::Either<String, crate::types::JsRegExpLike>,
    options: Option<crate::types::TextOptions>,
  ) -> Locator {
    let opts = options.map(ferridriver::options::TextOptions::from);
    Locator::wrap(
      self
        .inner
        .get_by_title(crate::types::getby_input_to_rust(text))
        .maybe_options(opts)
        .into_locator(),
    )
  }

  /// Playwright: `frame.frameLocator(selector?: string): FrameLocator`.
  #[napi(ts_args_type = "selector?: string")]
  pub fn frame_locator(&self, selector: Option<String>) -> crate::frame_locator::FrameLocator {
    crate::frame_locator::FrameLocator::wrap(self.inner.frame_locator(selector.as_deref()))
  }

  /// Playwright: `frame.page(): Page` — the page this frame belongs to.
  #[napi(ts_return_type = "Page")]
  pub fn page(&self) -> crate::identity::PageObject {
    crate::identity::PageObject(self.inner.page_arc().clone())
  }

  // ── Action methods (Playwright parity — task 3.9) ────────────────────
  //
  // Mirror the surface from
  // `/tmp/playwright/packages/playwright-core/src/client/frame.ts:296-447`.
  // Each method delegates to the Rust core's `Frame::<action>`, which
  // in turn delegates to the frame-scoped Locator. Option bags pick up
  // future extensions on the Locator surface.

  /// Click the first element matching `selector` in this frame. Accepts
  /// Playwright's full `FrameClickOptions` bag.
  #[napi]
  pub async fn click(&self, selector: String, options: Option<crate::types::ClickOptions>) -> Result<()> {
    self.inner.click(&selector).bag(options)?.await.into_napi()
  }

  /// Double-click the first element matching `selector` in this frame.
  /// Accepts Playwright's full `FrameDblClickOptions` bag.
  #[napi]
  pub async fn dblclick(&self, selector: String, options: Option<crate::types::DblClickOptions>) -> Result<()> {
    self.inner.dblclick(&selector).bag(options)?.await.into_napi()
  }

  /// Hover the first element matching `selector` in this frame.
  /// Accepts Playwright's full `FrameHoverOptions` bag.
  #[napi]
  pub async fn hover(&self, selector: String, options: Option<crate::types::HoverOptions>) -> Result<()> {
    self.inner.hover(&selector).bag(options)?.await.into_napi()
  }

  /// Tap the first element matching `selector` in this frame. Accepts
  /// Playwright's full `FrameTapOptions` bag.
  #[napi]
  pub async fn tap(&self, selector: String, options: Option<crate::types::TapOptions>) -> Result<()> {
    self.inner.tap(&selector).bag(options)?.await.into_napi()
  }

  #[napi]
  pub async fn focus(&self, selector: String) -> Result<()> {
    self.inner.focus(&selector).await.into_napi()
  }

  #[napi]
  pub async fn fill(&self, selector: String, value: String, options: Option<crate::types::FillOptions>) -> Result<()> {
    self.inner.fill(&selector, &value).bag(options)?.await.into_napi()
  }

  #[napi(js_name = "type")]
  pub async fn type_text(
    &self,
    selector: String,
    text: String,
    options: Option<crate::types::TypeOptions>,
  ) -> Result<()> {
    self.inner.r#type(&selector, &text).bag(options)?.await.into_napi()
  }

  #[napi]
  pub async fn press(&self, selector: String, key: String, options: Option<crate::types::PressOptions>) -> Result<()> {
    self.inner.press(&selector, &key).bag(options)?.await.into_napi()
  }

  #[napi]
  pub async fn check(&self, selector: String, options: Option<crate::types::CheckOptions>) -> Result<()> {
    self.inner.check(&selector).bag(options)?.await.into_napi()
  }

  #[napi]
  pub async fn uncheck(&self, selector: String, options: Option<crate::types::CheckOptions>) -> Result<()> {
    self.inner.uncheck(&selector).bag(options)?.await.into_napi()
  }

  #[napi]
  pub async fn set_checked(
    &self,
    selector: String,
    checked: bool,
    options: Option<crate::types::CheckOptions>,
  ) -> Result<()> {
    self
      .inner
      .set_checked(&selector, checked)
      .bag(options)?
      .await
      .into_napi()
  }

  #[napi]
  pub async fn select_option(
    &self,
    selector: String,
    values: crate::types::NapiSelectOptionInput,
    options: Option<crate::types::SelectOptionOptions>,
  ) -> Result<Vec<String>> {
    self
      .inner
      .select_option(&selector, values.0)
      .bag(options)?
      .await
      .into_napi()
  }

  #[napi(
    ts_args_type = "selector: string, files: string | string[] | FilePayload | FilePayload[], options?: SetInputFilesOptions"
  )]
  pub async fn set_input_files(
    &self,
    selector: String,
    files: crate::types::NapiInputFiles,
    options: Option<crate::types::SetInputFilesOptions>,
  ) -> Result<()> {
    self
      .inner
      .set_input_files(&selector, files.0)
      .bag(options)?
      .await
      .into_napi()
  }

  /// Drag from `source` to `target` selectors within this frame.
  /// Mirrors `frame.dragAndDrop(source, target, options?)`.
  #[napi]
  pub async fn drag_and_drop(
    &self,
    source: String,
    target: String,
    options: Option<crate::types::DragAndDropOptions>,
  ) -> Result<()> {
    self
      .inner
      .drag_and_drop(&source, &target)
      .bag(options)?
      .await
      .into_napi()
  }

  #[napi]
  pub async fn dispatch_event(
    &self,
    selector: String,
    event_type: String,
    event_init: Option<serde_json::Value>,
    options: Option<crate::types::DispatchEventOptions>,
  ) -> Result<()> {
    self
      .inner
      .dispatch_event(&selector, &event_type, event_init)
      .bag(options)?
      .await
      .into_napi()
  }

  #[napi]
  pub async fn text_content(
    &self,
    selector: String,
    options: Option<crate::types::SelectorReadOptions>,
  ) -> Result<Option<String>> {
    self.inner.text_content(&selector).bag(options)?.await.into_napi()
  }

  #[napi]
  pub async fn inner_text(
    &self,
    selector: String,
    options: Option<crate::types::SelectorReadOptions>,
  ) -> Result<String> {
    self.inner.inner_text(&selector).bag(options)?.await.into_napi()
  }

  #[napi(js_name = "innerHTML")]
  pub async fn inner_html(
    &self,
    selector: String,
    options: Option<crate::types::SelectorReadOptions>,
  ) -> Result<String> {
    self.inner.inner_html(&selector).bag(options)?.await.into_napi()
  }

  #[napi]
  pub async fn get_attribute(
    &self,
    selector: String,
    name: String,
    options: Option<crate::types::SelectorReadOptions>,
  ) -> Result<Option<String>> {
    self
      .inner
      .get_attribute(&selector, &name)
      .bag(options)?
      .await
      .into_napi()
  }

  #[napi]
  pub async fn input_value(
    &self,
    selector: String,
    options: Option<crate::types::SelectorReadOptions>,
  ) -> Result<String> {
    self.inner.input_value(&selector).bag(options)?.await.into_napi()
  }

  #[napi]
  pub async fn is_visible(&self, selector: String, options: Option<crate::types::SelectorReadOptions>) -> Result<bool> {
    self.inner.is_visible(&selector).bag(options)?.await.into_napi()
  }

  #[napi]
  pub async fn is_hidden(&self, selector: String, options: Option<crate::types::SelectorReadOptions>) -> Result<bool> {
    self.inner.is_hidden(&selector).bag(options)?.await.into_napi()
  }

  #[napi]
  pub async fn is_enabled(&self, selector: String, options: Option<crate::types::SelectorReadOptions>) -> Result<bool> {
    self.inner.is_enabled(&selector).bag(options)?.await.into_napi()
  }

  #[napi]
  pub async fn is_disabled(
    &self,
    selector: String,
    options: Option<crate::types::SelectorReadOptions>,
  ) -> Result<bool> {
    self.inner.is_disabled(&selector).bag(options)?.await.into_napi()
  }

  #[napi]
  pub async fn is_checked(&self, selector: String, options: Option<crate::types::SelectorReadOptions>) -> Result<bool> {
    self.inner.is_checked(&selector).bag(options)?.await.into_napi()
  }

  #[napi]
  pub async fn is_editable(
    &self,
    selector: String,
    options: Option<crate::types::SelectorReadOptions>,
  ) -> Result<bool> {
    self.inner.is_editable(&selector).bag(options)?.await.into_napi()
  }
}
