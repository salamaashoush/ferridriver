//! Page class -- NAPI binding for `ferridriver::Page`.

use crate::error::IntoNapi;
use crate::locator::Locator;
use crate::types::{
  DragAndDropOptions, GotoOptions, MetricData, ResponseData, RoleOptions, ScreenshotOptions, TextOptions,
  ViewportConfig, WaitOptions,
};
use napi::Result;
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;
use std::sync::{Arc, Mutex};

/// High-level page API, mirrors Playwright's Page interface.
#[napi]
pub struct Page {
  inner: Arc<ferridriver::Page>,
  mouse_position: Arc<Mutex<(f64, f64)>>,
}

impl Page {
  pub(crate) fn wrap(inner: Arc<ferridriver::Page>) -> Self {
    Self {
      inner,
      mouse_position: Arc::new(Mutex::new((0.0, 0.0))),
    }
  }

  #[allow(dead_code)]
  pub(crate) fn inner_ref(&self) -> &ferridriver::Page {
    &self.inner
  }
}

#[napi]
impl Page {
  #[napi(js_name = "context")]
  pub fn context(&self) -> Result<crate::context::BrowserContext> {
    let ctx = self
      .inner
      .context()
      .cloned()
      .ok_or_else(|| napi::Error::from_reason("page has no associated browser context"))?;
    Ok(crate::context::BrowserContext::wrap(ctx))
  }

  #[napi(getter)]
  pub fn keyboard(&self) -> Keyboard {
    Keyboard {
      page: Arc::clone(&self.inner),
    }
  }

  #[napi(getter)]
  pub fn mouse(&self) -> Mouse {
    Mouse {
      page: Arc::clone(&self.inner),
      position: Arc::clone(&self.mouse_position),
    }
  }

  /// Set the default timeout for all operations (milliseconds).
  #[napi]
  pub fn set_default_timeout(&self, ms: f64) {
    self.inner.set_default_timeout(crate::types::f64_to_u64(ms));
  }

  /// Set the default timeout for navigation-family operations
  /// (`goto`, `reload`, `goBack`, `goForward`, `waitForUrl`). Mirrors
  /// Playwright's `page.setDefaultNavigationTimeout(timeout)` — distinct
  /// from `setDefaultTimeout`, which applies to non-navigation actions.
  /// `0` = no timeout.
  #[napi]
  pub fn set_default_navigation_timeout(&self, ms: f64) {
    self.inner.set_default_navigation_timeout(crate::types::f64_to_u64(ms));
  }

  // ── Navigation ──────────────────────────────────────────────────────────

  #[napi]
  pub async fn goto(&self, url: String, options: Option<GotoOptions>) -> Result<()> {
    let opts = options.map(ferridriver::options::GotoOptions::from);
    self.inner.goto(&url, opts).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn go_back(&self, options: Option<GotoOptions>) -> Result<()> {
    let opts = options.map(ferridriver::options::GotoOptions::from);
    self.inner.go_back(opts).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn go_forward(&self, options: Option<GotoOptions>) -> Result<()> {
    let opts = options.map(ferridriver::options::GotoOptions::from);
    self.inner.go_forward(opts).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn reload(&self, options: Option<GotoOptions>) -> Result<()> {
    let opts = options.map(ferridriver::options::GotoOptions::from);
    self.inner.reload(opts).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn url(&self) -> Result<String> {
    self.inner.url().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn title(&self) -> Result<String> {
    self.inner.title().await.map_err(napi::Error::from_reason)
  }

  // ── Locators (lazy) ─────────────────────────────────────────────────────

  /// Playwright: `page.locator(selector, options?: LocatorOptions): Locator`
  /// (`/tmp/playwright/packages/playwright-core/src/client/frame.ts:324`).
  /// Thin delegator — Rust core's `Page::locator(selector, Option<FilterOptions>)`
  /// owns the filter-application logic. Page/Frame `.locator` accepts
  /// only selector strings; the `string | Locator` overload is on
  /// `Locator.locator`.
  #[napi]
  pub fn locator(&self, selector: String, options: Option<crate::types::FilterOptions>) -> Locator {
    let opts = options.map(ferridriver::options::FilterOptions::from);
    Locator::wrap(self.inner.locator(&selector, opts))
  }

  /// Playwright: `page.querySelector(selector): Promise<ElementHandle | null>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/page.ts`). The
  /// `$` alias is also exposed for parity.
  ///
  /// Returns the first element matching `selector`, pinned to the
  /// [`crate::element_handle::ElementHandle`] returned. `null` when no
  /// element matches. Unlike `page.locator()`, the returned handle
  /// does not re-resolve on each action — callers `dispose()` it when
  /// done.
  #[napi]
  pub async fn query_selector(&self, selector: String) -> Result<Option<crate::element_handle::ElementHandle>> {
    let inner = self.inner.query_selector(&selector).await.into_napi()?;
    Ok(inner.map(crate::element_handle::ElementHandle::wrap))
  }

  /// Alias for [`Self::query_selector`] matching Playwright's `$` shortcut.
  #[napi(js_name = "$")]
  pub async fn dollar(&self, selector: String) -> Result<Option<crate::element_handle::ElementHandle>> {
    self.query_selector(selector).await
  }

  #[napi]
  pub fn get_by_role(&self, role: String, options: Option<RoleOptions>) -> Locator {
    let opts: ferridriver::options::RoleOptions = options.map_or_else(Default::default, Into::into);
    Locator::wrap(self.inner.get_by_role(&role, &opts))
  }

  #[napi]
  pub fn get_by_text(&self, text: String, options: Option<TextOptions>) -> Locator {
    let opts: ferridriver::options::TextOptions = options.map_or_else(Default::default, Into::into);
    Locator::wrap(self.inner.get_by_text(&text, &opts))
  }

  #[napi]
  pub fn get_by_label(&self, text: String, options: Option<TextOptions>) -> Locator {
    let opts: ferridriver::options::TextOptions = options.map_or_else(Default::default, Into::into);
    Locator::wrap(self.inner.get_by_label(&text, &opts))
  }

  #[napi]
  pub fn get_by_placeholder(&self, text: String, options: Option<TextOptions>) -> Locator {
    let opts: ferridriver::options::TextOptions = options.map_or_else(Default::default, Into::into);
    Locator::wrap(self.inner.get_by_placeholder(&text, &opts))
  }

  #[napi]
  pub fn get_by_alt_text(&self, text: String, options: Option<TextOptions>) -> Locator {
    let opts: ferridriver::options::TextOptions = options.map_or_else(Default::default, Into::into);
    Locator::wrap(self.inner.get_by_alt_text(&text, &opts))
  }

  #[napi]
  pub fn get_by_title(&self, text: String, options: Option<TextOptions>) -> Locator {
    let opts: ferridriver::options::TextOptions = options.map_or_else(Default::default, Into::into);
    Locator::wrap(self.inner.get_by_title(&text, &opts))
  }

  #[napi]
  pub fn get_by_test_id(&self, test_id: String) -> Locator {
    Locator::wrap(self.inner.get_by_test_id(&test_id))
  }

  // ── Frames (sync, Playwright parity — task 3.8) ─────────────────────

  /// Main frame of this page. Playwright: `page.mainFrame(): Frame`
  /// (non-null). The frame cache is seeded inside `Page::new` /
  /// `Page::with_context` before the Page is handed out.
  #[napi]
  pub fn main_frame(&self) -> crate::frame::Frame {
    crate::frame::Frame::wrap(self.inner.main_frame())
  }

  /// All frames in the page (main frame + all iframes).
  /// Playwright: `page.frames(): Frame[]` (sync).
  #[napi]
  pub fn frames(&self) -> Vec<crate::frame::Frame> {
    self.inner.frames().into_iter().map(crate::frame::Frame::wrap).collect()
  }

  /// Find a frame by name or URL. Mirrors Playwright's
  /// `page.frame(string | { name?, url? }): Frame | null` (sync).
  /// The URL field is an exact-match string for now; task 3.12 extends
  /// it to the full `string | RegExp` union.
  #[napi(ts_args_type = "selector: string | { name?: string | null | undefined; url?: string | null | undefined }")]
  pub fn frame(&self, selector: crate::types::FrameSelectorArg) -> Option<crate::frame::Frame> {
    let core_sel: ferridriver::options::FrameSelector = match selector {
      napi::Either::A(name) => ferridriver::options::FrameSelector::by_name(name),
      napi::Either::B(bag) => bag.into(),
    };
    if core_sel.is_empty() {
      return None;
    }
    self.inner.frame(core_sel).map(crate::frame::Frame::wrap)
  }

  // ── Events (Playwright-compatible on/once/waitForEvent) ─────────────

  /// Register an event listener. Returns a listener ID for removal with `off()`.
  ///
  /// Supported events: 'console', 'response', 'request', 'dialog', 'download',
  /// 'frameattached', 'framedetached', 'framenavigated',
  /// 'load', 'domcontentloaded', 'close', 'pageerror'
  #[napi(
    ts_args_type = "event: 'console' | 'response' | 'request' | 'dialog' | 'download' | 'frameattached' | 'framedetached' | 'framenavigated' | 'load' | 'domcontentloaded' | 'close' | 'pageerror', listener: (data: { type: string; text: string } | ResponseData | { type: string; message: string; defaultValue: string } | Record<string, any>) => void"
  )]
  pub fn on(&self, event: String, listener: napi::bindgen_prelude::Function<'_, serde_json::Value, ()>) -> Result<f64> {
    let tsfn = listener
      .build_threadsafe_function()
      .callee_handled::<false>()
      .weak::<true>()
      .max_queue_size::<0>()
      .build()?;
    let event_name = event.clone();
    let callback: ferridriver::events::EventCallback = std::sync::Arc::new(move |ev| {
      if let Some(data) = event_to_js(&event_name, &ev) {
        tsfn.call(data, napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking);
      }
    });
    let id = self.inner.on(&event, callback);
    #[allow(clippy::cast_precision_loss)]
    Ok(id.0 as f64)
  }

  /// Register a one-time event listener. Auto-removed after first match.
  #[napi(
    ts_args_type = "event: 'console' | 'response' | 'request' | 'dialog' | 'download' | 'frameattached' | 'framedetached' | 'framenavigated' | 'load' | 'domcontentloaded' | 'close' | 'pageerror', listener: (data: { type: string; text: string } | ResponseData | { type: string; message: string; defaultValue: string } | Record<string, any>) => void"
  )]
  pub fn once(
    &self,
    event: String,
    listener: napi::bindgen_prelude::Function<'_, serde_json::Value, ()>,
  ) -> Result<f64> {
    let tsfn = listener
      .build_threadsafe_function()
      .callee_handled::<false>()
      .weak::<true>()
      .max_queue_size::<0>()
      .build()?;
    let event_name = event.clone();
    let callback: ferridriver::events::EventCallback = std::sync::Arc::new(move |ev| {
      if let Some(data) = event_to_js(&event_name, &ev) {
        tsfn.call(data, napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking);
      }
    });
    let id = self.inner.once(&event, callback);
    // ListenerId is a sequential counter; it will never exceed 2^53 (f64 mantissa precision).
    #[allow(clippy::cast_precision_loss)]
    Ok(id.0 as f64)
  }

  /// Remove an event listener by ID (returned from `on()` or `once()`).
  #[napi]
  pub fn off(&self, listener_id: f64) {
    // listener_id originates from on()/once() which returns a u64 counter
    // round-tripped through f64; the value is always non-negative and integral.
    self
      .inner
      .off(ferridriver::events::ListenerId(crate::types::f64_to_u64(listener_id)));
  }

  /// Remove all event listeners from this page.
  #[napi]
  pub fn remove_all_listeners(&self) {
    self.inner.remove_all_listeners();
  }

  /// Wait for a specific event. Playwright API: `page.waitForEvent(event)`.
  #[napi(
    ts_args_type = "event: 'console' | 'response' | 'request' | 'dialog' | 'load' | 'domcontentloaded' | 'close' | 'pageerror', timeoutMs?: number",
    ts_return_type = "Promise<{ type: string; text: string } | ResponseData | { type: string; message: string; defaultValue: string } | Record<string, any>>"
  )]
  pub async fn wait_for_event(&self, event: String, timeout_ms: Option<f64>) -> Result<serde_json::Value> {
    let timeout = crate::types::f64_to_u64(timeout_ms.unwrap_or(30000.0));
    let ev = self.inner.events().wait_for_event(&event, timeout).await.into_napi()?;
    Ok(page_event_to_value(&ev))
  }

  /// Wait for a network response matching a URL pattern.
  /// Playwright API: `page.waitForResponse(urlOrPredicate)`.
  /// `url` accepts a glob string or a native JS `RegExp`.
  #[napi(ts_args_type = "url: string | RegExp, timeoutMs?: number")]
  pub async fn wait_for_response(
    &self,
    url: napi::Either<String, crate::types::JsRegExpLike>,
    timeout_ms: Option<f64>,
  ) -> Result<ResponseData> {
    let matcher = crate::types::string_or_regex_to_rust(url)?;
    let r = self
      .inner
      .wait_for_response(matcher, timeout_ms.map(crate::types::f64_to_u64))
      .await
      .map_err(napi::Error::from_reason)?;
    Ok(ResponseData {
      url: r.url,
      status: i32::try_from(r.status).unwrap_or(i32::MAX),
      status_text: r.status_text,
    })
  }

  // ── Page-level actions ──────────────────────────────────────────────────

  /// Click the first element matching `selector`. Accepts Playwright's
  /// full `PageClickOptions` bag — see
  /// `/tmp/playwright/packages/playwright-core/types/types.d.ts:12986`.
  #[napi]
  pub async fn click(&self, selector: String, options: Option<crate::types::ClickOptions>) -> Result<()> {
    let opts = options.map(TryInto::try_into).transpose()?;
    self
      .inner
      .click(&selector, opts)
      .await
      .map_err(napi::Error::from_reason)
  }

  /// Double-click the first element matching `selector`. Accepts
  /// Playwright's full `PageDblClickOptions` bag.
  #[napi]
  pub async fn dblclick(&self, selector: String, options: Option<crate::types::DblClickOptions>) -> Result<()> {
    let opts = options.map(TryInto::try_into).transpose()?;
    self
      .inner
      .dblclick(&selector, opts)
      .await
      .map_err(napi::Error::from_reason)
  }

  /// Fill the first element matching `selector`. Accepts Playwright's
  /// full `PageFillOptions` bag.
  #[napi]
  pub async fn fill(&self, selector: String, value: String, options: Option<crate::types::FillOptions>) -> Result<()> {
    let opts = options.map(Into::into);
    self
      .inner
      .fill(&selector, &value, opts)
      .await
      .map_err(napi::Error::from_reason)
  }

  /// Type `text` into the first element matching `selector`. Accepts
  /// Playwright's full `PageTypeOptions` bag.
  #[napi]
  pub async fn type_text(
    &self,
    selector: String,
    text: String,
    options: Option<crate::types::TypeOptions>,
  ) -> Result<()> {
    let opts = options.map(Into::into);
    self
      .inner
      .r#type(&selector, &text, opts)
      .await
      .map_err(napi::Error::from_reason)
  }

  /// Press `key` on the first element matching `selector`. Accepts
  /// Playwright's full `PagePressOptions` bag.
  #[napi]
  pub async fn press(&self, selector: String, key: String, options: Option<crate::types::PressOptions>) -> Result<()> {
    let opts = options.map(Into::into);
    self
      .inner
      .press(&selector, &key, opts)
      .await
      .map_err(napi::Error::from_reason)
  }

  /// Hover the first element matching `selector`. Accepts Playwright's
  /// full `PageHoverOptions` bag.
  #[napi]
  pub async fn hover(&self, selector: String, options: Option<crate::types::HoverOptions>) -> Result<()> {
    let opts = options.map(TryInto::try_into).transpose()?;
    self
      .inner
      .hover(&selector, opts)
      .await
      .map_err(napi::Error::from_reason)
  }

  /// Select options on the `<select>` matching `selector`. Accepts
  /// Playwright's full value union plus the `PageSelectOptionOptions` bag.
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
      .select_option(&selector, values.0, opts)
      .await
      .map_err(napi::Error::from_reason)
  }

  /// Check a checkbox matching `selector`. Accepts Playwright's full
  /// `PageCheckOptions` bag.
  #[napi]
  pub async fn check(&self, selector: String, options: Option<crate::types::CheckOptions>) -> Result<()> {
    let opts = options.map(Into::into);
    self
      .inner
      .check(&selector, opts)
      .await
      .map_err(napi::Error::from_reason)
  }

  /// Uncheck a checkbox matching `selector`. Accepts Playwright's full
  /// `PageUncheckOptions` bag.
  #[napi]
  pub async fn uncheck(&self, selector: String, options: Option<crate::types::CheckOptions>) -> Result<()> {
    let opts = options.map(Into::into);
    self
      .inner
      .uncheck(&selector, opts)
      .await
      .map_err(napi::Error::from_reason)
  }

  /// Set the checked state of a checkbox or radio matching `selector`.
  /// Mirrors Playwright's `page.setChecked(selector, checked, options?)`.
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
      .set_checked(&selector, checked, opts)
      .await
      .map_err(napi::Error::from_reason)
  }

  /// Tap (touch) the element matched by `selector`. Mirrors Playwright's
  /// `page.tap(selector, options?)`. Accepts the full `PageTapOptions` bag.
  #[napi]
  pub async fn tap(&self, selector: String, options: Option<crate::types::TapOptions>) -> Result<()> {
    let opts = options.map(TryInto::try_into).transpose()?;
    self.inner.tap(&selector, opts).await.map_err(napi::Error::from_reason)
  }

  // ── Content ─────────────────────────────────────────────────────────────

  #[napi]
  pub async fn content(&self) -> Result<String> {
    self.inner.content().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn set_content(&self, html: String) -> Result<()> {
    self.inner.set_content(&html).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn markdown(&self) -> Result<String> {
    self.inner.markdown().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn text_content(&self, selector: String) -> Result<Option<String>> {
    self
      .inner
      .text_content(&selector)
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn inner_text(&self, selector: String) -> Result<String> {
    self.inner.inner_text(&selector).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn inner_html(&self, selector: String) -> Result<String> {
    self.inner.inner_html(&selector).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn get_attribute(&self, selector: String, name: String) -> Result<Option<String>> {
    self
      .inner
      .get_attribute(&selector, &name)
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn input_value(&self, selector: String) -> Result<String> {
    self
      .inner
      .input_value(&selector)
      .await
      .map_err(napi::Error::from_reason)
  }

  // ── State checks ────────────────────────────────────────────────────────

  #[napi]
  pub async fn is_visible(&self, selector: String) -> Result<bool> {
    self.inner.is_visible(&selector).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn is_hidden(&self, selector: String) -> Result<bool> {
    self.inner.is_hidden(&selector).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn is_enabled(&self, selector: String) -> Result<bool> {
    self.inner.is_enabled(&selector).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn is_disabled(&self, selector: String) -> Result<bool> {
    self
      .inner
      .is_disabled(&selector)
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn is_checked(&self, selector: String) -> Result<bool> {
    self.inner.is_checked(&selector).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn is_editable(&self, selector: String) -> Result<bool> {
    self
      .inner
      .is_editable(&selector)
      .await
      .map_err(napi::Error::from_reason)
  }

  // ── Evaluation ──────────────────────────────────────────────────────────

  #[napi]
  pub async fn evaluate(&self, expression: String) -> Result<Option<serde_json::Value>> {
    self.inner.evaluate(&expression).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn evaluate_str(&self, expression: String) -> Result<String> {
    self
      .inner
      .evaluate_str(&expression)
      .await
      .map_err(napi::Error::from_reason)
  }

  // ── Waiting ─────────────────────────────────────────────────────────────

  #[napi]
  pub async fn wait_for_selector(&self, selector: String, options: Option<WaitOptions>) -> Result<()> {
    let opts: ferridriver::options::WaitOptions = options.map_or_else(Default::default, Into::into);
    self
      .inner
      .wait_for_selector(&selector, opts)
      .await
      .map_err(napi::Error::from_reason)
  }

  /// Wait for the page URL to match. Accepts a glob string or a native JS `RegExp`.
  /// Playwright API: `page.waitForURL(url)`.
  #[napi(ts_args_type = "url: string | RegExp")]
  pub async fn wait_for_url(&self, url: napi::Either<String, crate::types::JsRegExpLike>) -> Result<()> {
    let matcher = crate::types::string_or_regex_to_rust(url)?;
    self.inner.wait_for_url(matcher).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn wait_for_timeout(&self, ms: f64) {
    self.inner.wait_for_timeout(crate::types::f64_to_u64(ms)).await;
  }

  #[napi]
  pub async fn wait_for_load_state(&self, state: Option<String>) -> Result<()> {
    self
      .inner
      .wait_for_load_state(state.as_deref())
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn wait_for_function(&self, expression: String, timeout_ms: Option<f64>) -> Result<serde_json::Value> {
    self
      .inner
      .wait_for_function(&expression, timeout_ms.map(crate::types::f64_to_u64))
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn wait_for_navigation(&self, timeout_ms: Option<f64>) -> Result<()> {
    self
      .inner
      .wait_for_navigation(timeout_ms.map(crate::types::f64_to_u64))
      .await
      .map_err(napi::Error::from_reason)
  }

  // ── Screenshots ─────────────────────────────────────────────────────────

  #[napi]
  pub async fn screenshot(&self, options: Option<ScreenshotOptions>) -> Result<Buffer> {
    let opts: ferridriver::options::ScreenshotOptions = options.map_or_else(Default::default, Into::into);
    let bytes = self.inner.screenshot(opts).await.map_err(napi::Error::from_reason)?;
    Ok(bytes.into())
  }

  #[napi]
  pub async fn screenshot_element(&self, selector: String) -> Result<Buffer> {
    let bytes = self
      .inner
      .screenshot_element(&selector)
      .await
      .map_err(napi::Error::from_reason)?;
    Ok(bytes.into())
  }

  /// Generate a PDF of the page (Chrome-family backends only).
  /// Playwright API: `page.pdf(options?)` — accepts the full `PDFOptions`
  /// shape (`format`, `path`, `scale`, `width`/`height` as `string|number`,
  /// `margin`, `headerTemplate`, `footerTemplate`, `pageRanges`, etc.).
  #[napi]
  pub async fn pdf(&self, options: Option<crate::types::PdfOptions>) -> Result<Buffer> {
    let rust_opts: ferridriver::options::PdfOptions = options.unwrap_or_default().try_into()?;
    let bytes = self
      .inner
      .pdf(rust_opts)
      .await
      .map_err(|e| napi::Error::from_reason(e.to_string()))?;
    Ok(bytes.into())
  }

  // ── Viewport ────────────────────────────────────────────────────────────

  #[napi]
  pub async fn set_viewport_size(&self, width: i32, height: i32) -> Result<()> {
    self
      .inner
      .set_viewport_size(i64::from(width), i64::from(height))
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn set_viewport(&self, config: ViewportConfig) -> Result<()> {
    self
      .inner
      .set_viewport(&ferridriver::options::ViewportConfig::from(config))
      .await
      .map_err(napi::Error::from_reason)
  }

  // ── Input devices ───────────────────────────────────────────────────────

  #[napi]
  pub async fn click_at(&self, x: f64, y: f64) -> Result<()> {
    self.inner.click_at(x, y).await.map_err(napi::Error::from_reason)?;
    *self.mouse_position.lock().expect("mouse position lock poisoned") = (x, y);
    Ok(())
  }

  /// Click at coordinates with specific button and click count.
  /// button: "left", "right", "middle", "back", "forward"
  #[napi]
  pub async fn click_at_opts(&self, x: f64, y: f64, button: String, click_count: Option<i32>) -> Result<()> {
    let count = u32::try_from(click_count.unwrap_or(1))
      .map_err(|_| napi::Error::from_reason("click_count must be non-negative"))?;
    self
      .inner
      .click_at_opts(x, y, &button, count)
      .await
      .map_err(napi::Error::from_reason)?;
    *self.mouse_position.lock().expect("mouse position lock poisoned") = (x, y);
    Ok(())
  }

  /// Move mouse to coordinates without clicking.
  #[napi]
  pub async fn move_mouse(&self, x: f64, y: f64) -> Result<()> {
    self
      .inner
      .mouse()
      .r#move(x, y, None)
      .await
      .map_err(napi::Error::from_reason)?;
    *self.mouse_position.lock().expect("mouse position lock poisoned") = (x, y);
    Ok(())
  }

  /// Move mouse smoothly with bezier easing.
  #[napi]
  pub async fn move_mouse_smooth(
    &self,
    from_x: f64,
    from_y: f64,
    to_x: f64,
    to_y: f64,
    steps: Option<i32>,
  ) -> Result<()> {
    let step_count =
      u32::try_from(steps.unwrap_or(10)).map_err(|_| napi::Error::from_reason("steps must be non-negative"))?;
    self
      .inner
      .move_mouse_smooth(from_x, from_y, to_x, to_y, step_count)
      .await
      .map_err(napi::Error::from_reason)?;
    *self.mouse_position.lock().expect("mouse position lock poisoned") = (to_x, to_y);
    Ok(())
  }

  /// Drag the element matching `source` onto the element matching
  /// `target`. Mirrors Playwright's
  /// `page.dragAndDrop(source, target, options?)` per
  /// `/tmp/playwright/packages/playwright-core/types/types.d.ts:2486`.
  #[napi]
  pub async fn drag_and_drop(&self, source: String, target: String, options: Option<DragAndDropOptions>) -> Result<()> {
    let opts = options.map(Into::into);
    self.inner.drag_and_drop(&source, &target, opts).await.into_napi()
  }

  #[napi]
  pub async fn type_str(&self, text: String) -> Result<()> {
    self
      .inner
      .keyboard()
      .r#type(&text)
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn press_key(&self, key: String) -> Result<()> {
    self
      .inner
      .keyboard()
      .press(&key)
      .await
      .map_err(napi::Error::from_reason)
  }

  // ── Emulation ───────────────────────────────────────────────────────────

  #[napi]
  pub async fn set_user_agent(&self, ua: String) -> Result<()> {
    self.inner.set_user_agent(&ua).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn set_geolocation(&self, lat: f64, lng: f64, accuracy: Option<f64>) -> Result<()> {
    self
      .inner
      .set_geolocation(lat, lng, accuracy.unwrap_or(1.0))
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn set_network_state(&self, offline: bool, latency: f64, download: f64, upload: f64) -> Result<()> {
    self
      .inner
      .set_network_state(offline, latency, download, upload)
      .await
      .map_err(napi::Error::from_reason)
  }

  /// Set the browser locale (navigator.language, Intl APIs).
  #[napi]
  pub async fn set_locale(&self, locale: String) -> Result<()> {
    self.inner.set_locale(&locale).await.map_err(napi::Error::from_reason)
  }

  /// Set the browser timezone (Date, Intl.DateTimeFormat).
  #[napi]
  pub async fn set_timezone(&self, timezone_id: String) -> Result<()> {
    self
      .inner
      .set_timezone(&timezone_id)
      .await
      .map_err(napi::Error::from_reason)
  }

  /// Emulate media features. Mirrors Playwright's
  /// `page.emulateMedia(options?: { media, colorScheme, reducedMotion, forcedColors, contrast })`
  /// per `/tmp/playwright/packages/playwright-core/types/types.d.ts:2580`.
  ///
  /// Every field accepts the enum values documented by Playwright, plus
  /// `null` to disable that specific emulation (mirrored in the JS binding
  /// via the option being absent or explicitly `null`).
  #[napi]
  pub async fn emulate_media(&self, options: Option<crate::types::EmulateMediaOptions>) -> Result<()> {
    let opts: ferridriver::options::EmulateMediaOptions = options.map(Into::into).unwrap_or_default();
    self.inner.emulate_media(&opts).await.map_err(napi::Error::from_reason)
  }

  /// Enable or disable JavaScript execution.
  #[napi]
  pub async fn set_javascript_enabled(&self, enabled: bool) -> Result<()> {
    self
      .inner
      .set_javascript_enabled(enabled)
      .await
      .map_err(napi::Error::from_reason)
  }

  // ── Focus / dispatch ────────────────────────────────────────────────────

  #[napi]
  pub async fn focus(&self, selector: String) -> Result<()> {
    self.inner.focus(&selector).await.map_err(napi::Error::from_reason)
  }

  /// Dispatch a DOM event of `type` on the element matching `selector`.
  /// Mirrors Playwright's `page.dispatchEvent(selector, type, eventInit?, options?)`.
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
      .dispatch_event(&selector, &event_type, event_init, options.map(Into::into))
      .await
      .map_err(napi::Error::from_reason)
  }

  // ── Tracing ─────────────────────────────────────────────────────────────

  #[napi]
  pub async fn start_tracing(&self) -> Result<()> {
    self.inner.start_tracing().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn stop_tracing(&self) -> Result<()> {
    self.inner.stop_tracing().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn metrics(&self) -> Result<Vec<MetricData>> {
    let metrics = self.inner.metrics().await.map_err(napi::Error::from_reason)?;
    Ok(metrics.iter().map(MetricData::from).collect())
  }

  // ── Misc ────────────────────────────────────────────────────────────────

  #[napi]
  pub async fn bring_to_front(&self) -> Result<()> {
    self.inner.bring_to_front().await.map_err(napi::Error::from_reason)
  }

  /// Close the page. Accepts the Playwright-identical
  /// `{ runBeforeUnload?, reason? }` options shape.
  #[napi]
  pub async fn close(&self, options: Option<crate::types::PageCloseOptions>) -> Result<()> {
    let opts: Option<ferridriver::options::PageCloseOptions> = options.map(Into::into);
    self
      .inner
      .close(opts)
      .await
      .map_err(|e| napi::Error::from_reason(e.to_string()))
  }

  #[napi]
  pub fn is_closed(&self) -> bool {
    self.inner.is_closed()
  }

  // ── Missing methods (batch add) ────────────────────────────────────────

  #[napi]
  pub async fn viewport_size(&self) -> Result<Vec<i32>> {
    let (w, h) = self.inner.viewport_size().await.map_err(napi::Error::from_reason)?;
    let w32 = i32::try_from(w).map_err(|_| napi::Error::from_reason(format!("viewport width {w} exceeds i32::MAX")))?;
    let h32 =
      i32::try_from(h).map_err(|_| napi::Error::from_reason(format!("viewport height {h} exceeds i32::MAX")))?;
    Ok(vec![w32, h32])
  }

  #[napi]
  pub async fn storage_state(&self) -> Result<serde_json::Value> {
    self.inner.storage_state().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn set_storage_state(&self, state: serde_json::Value) -> Result<()> {
    self
      .inner
      .set_storage_state(&state)
      .await
      .map_err(napi::Error::from_reason)
  }

  /// Register a JS snippet to run on every new document (main frame and
  /// iframes) before any page script executes. Mirrors Playwright's
  /// `page.addInitScript(script, arg)` — see
  /// `/tmp/playwright/packages/playwright-core/src/client/page.ts:520`.
  ///
  /// `script` is one of:
  /// - a `Function` — `.toString()`'d and wrapped as `(fn)(arg)` where `arg`
  ///   is `JSON.stringify`-serialised. `arg` defaults to `undefined`.
  /// - a `string` — used verbatim; passing `arg` rejects with
  ///   `"Cannot evaluate a string with arguments"`.
  /// - a `{ path?, content? }` object — `content` used verbatim, otherwise
  ///   `path` is read from disk; `arg` must be absent.
  ///
  /// All function/arg lowering lands in Rust core via
  /// [`ferridriver::options::evaluation_script`]; this method is a thin
  /// delegator.
  #[napi(ts_args_type = "script: Function | string | { path?: string, content?: string }, arg?: any")]
  pub async fn add_init_script(
    &self,
    script: crate::types::NapiInitScript,
    arg: crate::types::NapiInitScriptArg,
  ) -> Result<String> {
    self
      .inner
      .add_init_script(script.into(), arg.0)
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn remove_init_script(&self, identifier: String) -> Result<()> {
    self
      .inner
      .remove_init_script(&identifier)
      .await
      .map_err(napi::Error::from_reason)
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
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn add_style_tag(&self, url: Option<String>, content: Option<String>) -> Result<()> {
    self
      .inner
      .add_style_tag(url.as_deref(), content.as_deref())
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn set_extra_http_headers(&self, headers: std::collections::HashMap<String, String>) -> Result<()> {
    let mut fx = rustc_hash::FxHashMap::default();
    for (k, v) in headers {
      fx.insert(k, v);
    }
    self
      .inner
      .set_extra_http_headers(&fx)
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn grant_permissions(&self, permissions: Vec<String>, origin: Option<String>) -> Result<()> {
    self
      .inner
      .grant_permissions(&permissions, origin.as_deref())
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn reset_permissions(&self) -> Result<()> {
    self.inner.reset_permissions().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn set_focus_emulation_enabled(&self, enabled: bool) -> Result<()> {
    self
      .inner
      .set_focus_emulation_enabled(enabled)
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn mouse_wheel(&self, delta_x: f64, delta_y: f64) -> Result<()> {
    self
      .inner
      .mouse()
      .wheel(delta_x, delta_y)
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn mouse_down(&self, x: f64, y: f64, button: Option<String>) -> Result<()> {
    let mouse = self.inner.mouse();
    mouse.r#move(x, y, None).await.map_err(napi::Error::from_reason)?;
    let opts = ferridriver::page::MouseDownOptions {
      button,
      click_count: None,
    };
    mouse.down(Some(opts)).await.map_err(napi::Error::from_reason)?;
    *self.mouse_position.lock().expect("mouse position lock poisoned") = (x, y);
    Ok(())
  }

  #[napi]
  pub async fn mouse_up(&self, x: f64, y: f64, button: Option<String>) -> Result<()> {
    let mouse = self.inner.mouse();
    mouse.r#move(x, y, None).await.map_err(napi::Error::from_reason)?;
    let opts = ferridriver::page::MouseUpOptions {
      button,
      click_count: None,
    };
    mouse.up(Some(opts)).await.map_err(napi::Error::from_reason)?;
    *self.mouse_position.lock().expect("mouse position lock poisoned") = (x, y);
    Ok(())
  }

  /// Set files on the `<input type=file>` matching `selector`.
  /// Accepts Playwright's full value union + `PageSetInputFilesOptions`.
  #[napi]
  pub async fn set_input_files(
    &self,
    selector: String,
    files: crate::types::NapiInputFiles,
    options: Option<crate::types::SetInputFilesOptions>,
  ) -> Result<()> {
    let opts = options.map(Into::into);
    self
      .inner
      .set_input_files(&selector, files.0, opts)
      .await
      .map_err(napi::Error::from_reason)
  }

  /// Wait for a network request matching a URL pattern.
  /// Playwright API: `page.waitForRequest(urlOrPredicate)`.
  /// `url` accepts a glob string or a native JS `RegExp`.
  #[napi(ts_args_type = "url: string | RegExp, timeoutMs?: number")]
  pub async fn wait_for_request(
    &self,
    url: napi::Either<String, crate::types::JsRegExpLike>,
    timeout_ms: Option<f64>,
  ) -> Result<serde_json::Value> {
    let matcher = crate::types::string_or_regex_to_rust(url)?;
    let req = self
      .inner
      .wait_for_request(matcher, timeout_ms.map(crate::types::f64_to_u64))
      .await
      .map_err(napi::Error::from_reason)?;
    Ok(serde_json::json!({"url": req.url, "method": req.method, "resourceType": req.resource_type}))
  }

  #[napi]
  pub async fn wait_for_download(
    &self,
    url_pattern: Option<String>,
    timeout_ms: Option<f64>,
  ) -> Result<serde_json::Value> {
    let dl = self
      .inner
      .wait_for_download(url_pattern.as_deref(), timeout_ms.map(crate::types::f64_to_u64))
      .await
      .map_err(napi::Error::from_reason)?;
    Ok(serde_json::json!({"guid": dl.guid, "url": dl.url, "suggestedFilename": dl.suggested_filename}))
  }

  #[napi(getter)]
  pub fn default_timeout(&self) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
      self.inner.default_timeout() as f64
    }
  }

  // ── Network interception ─────────────────────────────────────────────

  /// Route network requests matching a glob pattern.
  ///
  /// The handler receives a `Route` object with request details and must call
  /// one of `route.fulfill()`, `route.continue()`, or `route.abort()`.
  ///
  /// ```js
  /// await page.route('**/api/*', (route) => {
  ///   if (route.url.includes('block')) {
  ///     route.abort();
  ///   } else {
  ///     route.fulfill({ status: 200, body: '{"ok":true}', contentType: 'application/json' });
  ///   }
  /// });
  /// ```
  /// Route network requests matching a glob pattern.
  ///
  /// The handler receives a `Route` object with request details and must call
  /// one of `route.fulfill()`, `route.continue()`, or `route.abort()`.
  ///
  /// ```js
  /// await page.route('**/api/*', (route) => {
  ///   if (route.url.includes('block')) {
  ///     route.abort();
  ///   } else {
  ///     route.fulfill({ status: 200, body: '{"ok":true}', contentType: 'application/json' });
  ///   }
  /// });
  /// ```
  #[napi(ts_args_type = "url: string | RegExp, handler: (route: Route) => void")]
  pub async fn route(
    &self,
    url: napi::Either<String, crate::types::JsRegExpLike>,
    handler: napi::threadsafe_function::ThreadsafeFunction<
      crate::route::Route,
      (),
      crate::route::Route,
      napi::Status,
      false,
      true,
      0,
    >,
  ) -> Result<()> {
    let matcher = crate::types::string_or_regex_to_rust(url)?;
    let rust_handler: ferridriver::route::RouteHandler = std::sync::Arc::new(move |route| {
      let napi_route = crate::route::Route::wrap(route);
      handler.call(
        napi_route,
        napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking,
      );
    });

    self
      .inner
      .route(matcher, rust_handler)
      .await
      .map_err(napi::Error::from_reason)
  }

  /// Remove all route handlers matching the given URL matcher.
  /// Accepts the same shape as `route()` — a glob string or a native JS `RegExp`.
  #[napi(ts_args_type = "url: string | RegExp")]
  pub async fn unroute(&self, url: napi::Either<String, crate::types::JsRegExpLike>) -> Result<()> {
    let matcher = crate::types::string_or_regex_to_rust(url)?;
    self.inner.unroute(&matcher).await.map_err(napi::Error::from_reason)
  }

  // ── Expect assertions (delegates to Rust core, all polling in Rust) ──

  #[napi]
  pub async fn expect_title(&self, expected: String, not: Option<bool>, timeout_ms: Option<f64>) -> Result<()> {
    let mut e = ferridriver_test::expect::expect(&self.inner);
    if not.unwrap_or(false) {
      e = e.not();
    }
    if let Some(t) = timeout_ms {
      e = e.with_timeout(std::time::Duration::from_millis(t as u64));
    }
    e.to_have_title(expected.as_str())
      .await
      .map_err(|e| napi::Error::from_reason(e.message))
  }

  #[napi]
  pub async fn expect_url(&self, expected: String, not: Option<bool>, timeout_ms: Option<f64>) -> Result<()> {
    let mut e = ferridriver_test::expect::expect(&self.inner);
    if not.unwrap_or(false) {
      e = e.not();
    }
    if let Some(t) = timeout_ms {
      e = e.with_timeout(std::time::Duration::from_millis(t as u64));
    }
    e.to_have_url(expected.as_str())
      .await
      .map_err(|e| napi::Error::from_reason(e.message))
  }
}

#[napi(object)]
pub struct MouseClickOptions {
  pub button: Option<String>,
  #[napi(js_name = "clickCount")]
  pub click_count: Option<i32>,
}

#[napi]
pub struct Keyboard {
  page: Arc<ferridriver::Page>,
}

#[napi]
impl Keyboard {
  #[napi]
  pub async fn down(&self, key: String) -> Result<()> {
    self.page.keyboard().down(&key).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn up(&self, key: String) -> Result<()> {
    self.page.keyboard().up(&key).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn press(&self, key: String) -> Result<()> {
    self.page.keyboard().press(&key).await.map_err(napi::Error::from_reason)
  }

  #[napi(js_name = "type")]
  pub async fn type_text(&self, text: String) -> Result<()> {
    self
      .page
      .keyboard()
      .r#type(&text)
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi(js_name = "insertText")]
  pub async fn insert_text(&self, text: String) -> Result<()> {
    self
      .page
      .keyboard()
      .insert_text(&text)
      .await
      .map_err(napi::Error::from_reason)
  }
}

#[napi]
pub struct Mouse {
  page: Arc<ferridriver::Page>,
  position: Arc<Mutex<(f64, f64)>>,
}

#[napi]
impl Mouse {
  #[napi]
  pub async fn click(&self, x: f64, y: f64, options: Option<MouseClickOptions>) -> Result<()> {
    let opts = ferridriver::page::MouseClickOptions {
      button: options.as_ref().and_then(|o| o.button.clone()),
      click_count: options
        .as_ref()
        .and_then(|o| o.click_count)
        .and_then(|n| u32::try_from(n).ok()),
    };
    self
      .page
      .mouse()
      .click(x, y, Some(opts))
      .await
      .map_err(napi::Error::from_reason)?;
    *self.position.lock().expect("mouse position lock poisoned") = (x, y);
    Ok(())
  }

  #[napi(js_name = "move")]
  pub async fn move_to(&self, x: f64, y: f64, steps: Option<i32>) -> Result<()> {
    let step_count = steps
      .map(|s| u32::try_from(s).map_err(|_| napi::Error::from_reason("steps must be non-negative")))
      .transpose()?;
    self
      .page
      .mouse()
      .r#move(x, y, step_count)
      .await
      .map_err(napi::Error::from_reason)?;
    *self.position.lock().expect("mouse position lock poisoned") = (x, y);
    Ok(())
  }

  #[napi]
  pub async fn dblclick(&self, x: f64, y: f64, options: Option<MouseClickOptions>) -> Result<()> {
    let opts = ferridriver::page::MouseClickOptions {
      button: options.as_ref().and_then(|o| o.button.clone()),
      click_count: None,
    };
    self
      .page
      .mouse()
      .dblclick(x, y, Some(opts))
      .await
      .map_err(napi::Error::from_reason)?;
    *self.position.lock().expect("mouse position lock poisoned") = (x, y);
    Ok(())
  }

  #[napi]
  pub async fn down(&self, button: Option<String>) -> Result<()> {
    let opts = ferridriver::page::MouseDownOptions {
      button,
      click_count: None,
    };
    self
      .page
      .mouse()
      .down(Some(opts))
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn up(&self, button: Option<String>) -> Result<()> {
    let opts = ferridriver::page::MouseUpOptions {
      button,
      click_count: None,
    };
    self.page.mouse().up(Some(opts)).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn wheel(&self, delta_x: f64, delta_y: f64) -> Result<()> {
    self
      .page
      .mouse()
      .wheel(delta_x, delta_y)
      .await
      .map_err(napi::Error::from_reason)
  }
}

// ── Event conversion helpers ─────────────────────────────────────────────

use ferridriver::events::PageEvent;

/// Convert a `PageEvent` to a JS-friendly `serde_json::Value`, filtered by event name.
/// Returns None if the event doesn't match the requested name.
#[allow(clippy::match_same_arms)] // arms differ by event name filter, bodies intentionally identical
/// Convert a named event to a JS value. Uses serde::Serialize on the event
/// structs directly — avoids the `json!()` macro's per-field string cloning.
fn event_to_js(event_name: &str, event: &PageEvent) -> Option<serde_json::Value> {
  match (event_name, event) {
    ("console", PageEvent::Console(msg)) => serde_json::to_value(msg).ok(),
    ("response", PageEvent::Response(r)) => serde_json::to_value(r).ok(),
    ("request", PageEvent::Request(r)) => serde_json::to_value(r).ok(),
    ("dialog", PageEvent::Dialog(d)) => serde_json::to_value(d).ok(),
    ("frameattached", PageEvent::FrameAttached(f)) | ("framenavigated", PageEvent::FrameNavigated(f)) => {
      serde_json::to_value(f).ok()
    },
    ("framedetached", PageEvent::FrameDetached { frame_id }) => Some(serde_json::json!({"frameId": frame_id})),
    ("download", PageEvent::Download(d)) => serde_json::to_value(d).ok(),
    ("load", PageEvent::Load) | ("domcontentloaded", PageEvent::DomContentLoaded) | ("close", PageEvent::Close) => {
      Some(serde_json::Value::Object(Default::default()))
    },
    ("pageerror", PageEvent::PageError(msg)) => Some(serde_json::json!({"message": msg})),
    _ => None,
  }
}

/// Convert any `PageEvent` to a JS value (for waitForEvent).
fn page_event_to_value(event: &PageEvent) -> serde_json::Value {
  match event {
    PageEvent::Console(msg) => serde_json::to_value(msg).unwrap_or_default(),
    PageEvent::Response(r) => serde_json::to_value(r).unwrap_or_default(),
    PageEvent::Request(r) => serde_json::to_value(r).unwrap_or_default(),
    PageEvent::Dialog(d) => serde_json::to_value(d).unwrap_or_default(),
    PageEvent::FrameAttached(f) | PageEvent::FrameNavigated(f) => serde_json::to_value(f).unwrap_or_default(),
    PageEvent::FrameDetached { frame_id } => serde_json::json!({"frameId": frame_id}),
    PageEvent::Download(d) => serde_json::to_value(d).unwrap_or_default(),
    PageEvent::Load => serde_json::json!({"type": "load"}),
    PageEvent::DomContentLoaded => serde_json::json!({"type": "domcontentloaded"}),
    PageEvent::Close => serde_json::json!({"type": "close"}),
    PageEvent::PageError(msg) => serde_json::json!({"message": msg}),
  }
}
