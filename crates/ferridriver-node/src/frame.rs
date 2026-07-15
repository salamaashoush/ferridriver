//! Frame class -- NAPI binding for `ferridriver::Frame`.

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
  #[napi]
  pub fn parent_frame(&self) -> Option<Frame> {
    self.inner.parent_frame().map(Frame::wrap)
  }

  /// Child frames of this frame.
  /// Playwright: `frame.childFrames(): Frame[]` (sync).
  #[napi]
  pub fn child_frames(&self) -> Vec<Frame> {
    self.inner.child_frames().into_iter().map(Frame::wrap).collect()
  }

  // ── Evaluation ────────────────────────────────────────────────────────

  /// Playwright: `frame.evaluate(pageFunction, arg?): Promise<R>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/frame.ts:196`).
  #[napi(
    ts_args_type = "pageFunction: string | Function, arg?: unknown",
    ts_return_type = "Promise<unknown>"
  )]
  pub async fn evaluate(
    &self,
    page_function: crate::types::NapiPageFunction,
    arg: Option<crate::types::NapiEvaluateArg>,
  ) -> Result<crate::serialize_out::Evaluated> {
    let serialized = crate::page::build_serialized_argument(arg);
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
    let serialized = crate::page::build_serialized_argument(arg);
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
    let serialized = crate::page::build_serialized_argument(arg);
    let result = self
      .inner
      .eval_on_selector_all(&selector, &page_function.source, serialized, page_function.is_function)
      .await
      .into_napi()?;
    Ok(crate::serialize_out::Evaluated(result))
  }

  /// Playwright: `frame.evaluateHandle(pageFunction, arg?): Promise<JSHandle>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/frame.ts:190`).
  #[napi(ts_args_type = "pageFunction: string | Function, arg?: unknown")]
  pub async fn evaluate_handle(
    &self,
    page_function: crate::types::NapiPageFunction,
    arg: Option<crate::types::NapiEvaluateArg>,
  ) -> Result<crate::js_handle::JSHandle> {
    let serialized = crate::page::build_serialized_argument(arg);
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

  #[napi]
  pub async fn content(&self) -> Result<String> {
    self.inner.content().await.into_napi()
  }

  #[napi]
  pub async fn title(&self) -> Result<String> {
    self.inner.title().await.into_napi()
  }

  // ── Navigation ────────────────────────────────────────────────────────

  /// Navigate this frame to a URL. Returns the main-document
  /// `Response` when the frame is the main frame and the backend can
  /// observe it; `null` otherwise (child frame navigation / same-doc
  /// navigation / unobservable backend).
  #[napi(ts_return_type = "Promise<Response | null>")]
  pub async fn goto(&self, url: String) -> Result<Option<crate::network::Response>> {
    let resp = self.inner.goto(&url).await.into_napi()?;
    let page = self.inner.page_arc().clone();
    Ok(resp.map(|r| crate::network::Response::from_core_with_page(r, page)))
  }

  // ── Waiting ───────────────────────────────────────────────────────────

  #[napi]
  pub async fn wait_for_load_state(&self) -> Result<()> {
    self.inner.wait_for_load_state().await.into_napi()
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
    let opts = options.map(ferridriver::options::WaitOptions::try_from).transpose()?;
    let handle = self
      .inner
      .wait_for_selector(&selector)
      .maybe_options(opts)
      .await
      .into_napi()?;
    Ok(handle.map(crate::element_handle::ElementHandle::wrap))
  }

  // ── Additional content methods ───────────────────────────────────────

  #[napi]
  pub async fn set_content(&self, html: String) -> Result<()> {
    self.inner.set_content(&html).await.into_napi()
  }

  #[napi]
  pub async fn add_script_tag(
    &self,
    url: Option<String>,
    content: Option<String>,
    script_type: Option<String>,
  ) -> Result<()> {
    self
      .inner
      .add_script_tag(url.as_deref(), content.as_deref(), script_type.as_deref())
      .await
      .into_napi()
  }

  #[napi]
  pub async fn add_style_tag(&self, url: Option<String>, content: Option<String>) -> Result<()> {
    self
      .inner
      .add_style_tag(url.as_deref(), content.as_deref())
      .await
      .into_napi()
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

  /// Playwright: `frame.frameLocator(selector): FrameLocator`. Targets
  /// an `<iframe>` matching the selector within this frame.
  #[napi]
  pub fn frame_locator(&self, selector: String) -> crate::frame_locator::FrameLocator {
    crate::frame_locator::FrameLocator::wrap(self.inner.frame_locator(&selector))
  }

  /// Playwright: `frame.page(): Page` — the page this frame belongs to.
  #[napi]
  pub fn page(&self) -> crate::page::Page {
    crate::page::Page::wrap(self.inner.page_arc().clone())
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
    let opts = options.map(TryInto::try_into).transpose()?;
    self.inner.click(&selector).maybe_options(opts).await.into_napi()
  }

  /// Double-click the first element matching `selector` in this frame.
  /// Accepts Playwright's full `FrameDblClickOptions` bag.
  #[napi]
  pub async fn dblclick(&self, selector: String, options: Option<crate::types::DblClickOptions>) -> Result<()> {
    let opts = options.map(TryInto::try_into).transpose()?;
    self.inner.dblclick(&selector).maybe_options(opts).await.into_napi()
  }

  /// Hover the first element matching `selector` in this frame.
  /// Accepts Playwright's full `FrameHoverOptions` bag.
  #[napi]
  pub async fn hover(&self, selector: String, options: Option<crate::types::HoverOptions>) -> Result<()> {
    let opts = options.map(TryInto::try_into).transpose()?;
    self.inner.hover(&selector).maybe_options(opts).await.into_napi()
  }

  /// Tap the first element matching `selector` in this frame. Accepts
  /// Playwright's full `FrameTapOptions` bag.
  #[napi]
  pub async fn tap(&self, selector: String, options: Option<crate::types::TapOptions>) -> Result<()> {
    let opts = options.map(TryInto::try_into).transpose()?;
    self.inner.tap(&selector).maybe_options(opts).await.into_napi()
  }

  #[napi]
  pub async fn focus(&self, selector: String) -> Result<()> {
    self.inner.focus(&selector).await.into_napi()
  }

  #[napi]
  pub async fn fill(&self, selector: String, value: String, options: Option<crate::types::FillOptions>) -> Result<()> {
    let opts = options.map(Into::into);
    self.inner.fill(&selector, &value).maybe_options(opts).await.into_napi()
  }

  #[napi(js_name = "type")]
  pub async fn type_text(
    &self,
    selector: String,
    text: String,
    options: Option<crate::types::TypeOptions>,
  ) -> Result<()> {
    let opts = options.map(Into::into);
    self
      .inner
      .r#type(&selector, &text)
      .maybe_options(opts)
      .await
      .into_napi()
  }

  #[napi]
  pub async fn press(&self, selector: String, key: String, options: Option<crate::types::PressOptions>) -> Result<()> {
    let opts = options.map(Into::into);
    self.inner.press(&selector, &key).maybe_options(opts).await.into_napi()
  }

  #[napi]
  pub async fn check(&self, selector: String, options: Option<crate::types::CheckOptions>) -> Result<()> {
    let opts = options.map(Into::into);
    self.inner.check(&selector).maybe_options(opts).await.into_napi()
  }

  #[napi]
  pub async fn uncheck(&self, selector: String, options: Option<crate::types::CheckOptions>) -> Result<()> {
    let opts = options.map(Into::into);
    self.inner.uncheck(&selector).maybe_options(opts).await.into_napi()
  }

  #[napi]
  pub async fn set_checked(
    &self,
    selector: String,
    checked: bool,
    options: Option<crate::types::CheckOptions>,
  ) -> Result<()> {
    let opts = options.map(Into::into);
    self
      .inner
      .set_checked(&selector, checked)
      .maybe_options(opts)
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
    let opts = options.map(Into::into);
    self
      .inner
      .select_option(&selector, values.0)
      .maybe_options(opts)
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
    let opts = options.map(Into::into);
    self
      .inner
      .set_input_files(&selector, files.0)
      .maybe_options(opts)
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
    let opts = options.map(ferridriver::options::DragAndDropOptions::from);
    self
      .inner
      .drag_and_drop(&source, &target)
      .maybe_options(opts)
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
      .maybe_options(options.map(Into::into))
      .await
      .into_napi()
  }

  #[napi]
  pub async fn text_content(&self, selector: String) -> Result<Option<String>> {
    self.inner.text_content(&selector).await.into_napi()
  }

  #[napi]
  pub async fn inner_text(&self, selector: String) -> Result<String> {
    self.inner.inner_text(&selector).await.into_napi()
  }

  #[napi(js_name = "innerHTML")]
  pub async fn inner_html(&self, selector: String) -> Result<String> {
    self.inner.inner_html(&selector).await.into_napi()
  }

  #[napi]
  pub async fn get_attribute(&self, selector: String, name: String) -> Result<Option<String>> {
    self.inner.get_attribute(&selector, &name).await.into_napi()
  }

  #[napi]
  pub async fn input_value(&self, selector: String) -> Result<String> {
    self.inner.input_value(&selector).await.into_napi()
  }

  #[napi]
  pub async fn is_visible(&self, selector: String) -> Result<bool> {
    self.inner.is_visible(&selector).await.into_napi()
  }

  #[napi]
  pub async fn is_hidden(&self, selector: String) -> Result<bool> {
    self.inner.is_hidden(&selector).await.into_napi()
  }

  #[napi]
  pub async fn is_enabled(&self, selector: String) -> Result<bool> {
    self.inner.is_enabled(&selector).await.into_napi()
  }

  #[napi]
  pub async fn is_disabled(&self, selector: String) -> Result<bool> {
    self.inner.is_disabled(&selector).await.into_napi()
  }

  #[napi]
  pub async fn is_editable(&self, selector: String) -> Result<bool> {
    self.inner.is_editable(&selector).await.into_napi()
  }

  #[napi]
  pub async fn is_checked(&self, selector: String) -> Result<bool> {
    self.inner.is_checked(&selector).await.into_napi()
  }
}
