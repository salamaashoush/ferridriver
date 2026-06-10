//! `ElementHandleJs`: QuickJS wrapper around `ferridriver::ElementHandle`.
//!
//! Phase-C surface covers lifecycle — `dispose`, `isDisposed`, `asJSHandle`
//! — enough to exercise the per-backend release paths from `run_script`.
//! Phase E bolts the ~25 Playwright DOM methods on top of this same class.

use ferridriver::ElementHandle;
use ferridriver::backend::ImageFormat;
use rquickjs::JsLifetime;
use rquickjs::class::Trace;

use crate::bindings::convert::FerriResultCtxExt;
use crate::bindings::convert::{extract_page_function, quickjs_arg_to_serialized, serialized_value_to_quickjs};

/// Extract `{ type?: 'png' | 'jpeg' | 'webp' }` from a user-supplied
/// screenshot options bag. Defaults to PNG when the caller omits the
/// bag or the `type` field. Matches Playwright's
/// `elementHandle.screenshot(options?)` surface.
fn parse_screenshot_format<'js>(
  _ctx: &rquickjs::Ctx<'js>,
  options: rquickjs::function::Opt<rquickjs::Value<'js>>,
) -> rquickjs::Result<ImageFormat> {
  let Some(opts_val) = options.0 else {
    return Ok(ImageFormat::Png);
  };
  if opts_val.is_undefined() || opts_val.is_null() {
    return Ok(ImageFormat::Png);
  }
  let Some(obj) = opts_val.as_object() else {
    return Ok(ImageFormat::Png);
  };
  let type_field: Option<String> = obj.get("type").ok();
  match type_field.as_deref() {
    None | Some("" | "png") => Ok(ImageFormat::Png),
    Some("jpeg" | "jpg") => Ok(ImageFormat::Jpeg),
    Some("webp") => Ok(ImageFormat::Webp),
    Some(other) => Err(rquickjs::Error::new_from_js_message(
      "screenshot",
      "invalid format",
      &format!("unsupported screenshot type {other:?}; expected 'png' | 'jpeg' | 'webp'"),
    )),
  }
}

/// QuickJS-visible wrapper around a core [`ElementHandle`].
#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "ElementHandle")]
pub struct ElementHandleJs {
  #[qjs(skip_trace)]
  inner: ElementHandle,
}

impl ElementHandleJs {
  #[must_use]
  pub fn new(inner: ElementHandle) -> Self {
    Self { inner }
  }

  #[must_use]
  pub fn inner(&self) -> &ElementHandle {
    &self.inner
  }
}

#[rquickjs::methods]
impl ElementHandleJs {
  /// Playwright `elementHandle.isDisposed(): boolean` — METHOD (not a
  /// property). LLM-generated code calls `eh.isDisposed()` with parens.
  #[qjs(rename = "isDisposed")]
  pub fn is_disposed(&self) -> bool {
    self.inner.is_disposed()
  }

  /// Release the underlying remote element. Idempotent.
  #[qjs(rename = "dispose")]
  pub async fn dispose(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<()> {
    self.inner.dispose().await.into_js_with(&ctx)
  }

  /// Companion [`crate::bindings::js_handle::JSHandleJs`] sharing the
  /// same remote reference. Disposing either releases the remote and
  /// latches both into the disposed state.
  #[qjs(rename = "asJSHandle")]
  pub fn as_js_handle(&self) -> crate::bindings::js_handle::JSHandleJs {
    crate::bindings::js_handle::JSHandleJs::new(self.inner.as_js_handle().clone())
  }

  /// Playwright: `elementHandle.evaluate(pageFunction, arg?): Promise<R>`.
  #[qjs(rename = "evaluate")]
  pub async fn evaluate<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    page_function: rquickjs::Value<'js>,
    arg: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    let (source, is_fn) = extract_page_function(&ctx, page_function)?;
    let serialized = quickjs_arg_to_serialized(&ctx, arg.0)?;
    let result = self
      .inner
      .as_js_handle()
      .evaluate(&source, serialized, is_fn)
      .await
      .into_js_with(&ctx)?;
    serialized_value_to_quickjs(&ctx, &result)
  }

  /// Playwright: `elementHandle.evaluateHandle(pageFunction, arg?): Promise<JSHandle>`.
  #[qjs(rename = "evaluateHandle")]
  pub async fn evaluate_handle<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    page_function: rquickjs::Value<'js>,
    arg: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<crate::bindings::js_handle::JSHandleJs> {
    let (source, is_fn) = extract_page_function(&ctx, page_function)?;
    let serialized = quickjs_arg_to_serialized(&ctx, arg.0)?;
    let handle = self
      .inner
      .as_js_handle()
      .evaluate_handle(&source, serialized, is_fn)
      .await
      .into_js_with(&ctx)?;
    Ok(crate::bindings::js_handle::JSHandleJs::new(handle))
  }

  // ── Content reads (Phase E) ──────────────────────────────────────────

  #[qjs(rename = "innerHTML")]
  pub async fn inner_html(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<String> {
    self.inner.inner_html().await.into_js_with(&ctx)
  }

  #[qjs(rename = "innerText")]
  pub async fn inner_text(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<String> {
    self.inner.inner_text().await.into_js_with(&ctx)
  }

  #[qjs(rename = "textContent")]
  pub async fn text_content(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<Option<String>> {
    self.inner.text_content().await.into_js_with(&ctx)
  }

  #[qjs(rename = "getAttribute")]
  pub async fn get_attribute(&self, ctx: rquickjs::Ctx<'_>, name: String) -> rquickjs::Result<Option<String>> {
    self.inner.get_attribute(&name).await.into_js_with(&ctx)
  }

  #[qjs(rename = "inputValue")]
  pub async fn input_value(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<String> {
    self.inner.input_value().await.into_js_with(&ctx)
  }

  // ── State predicates (Phase E) ───────────────────────────────────────

  #[qjs(rename = "isVisible")]
  pub async fn is_visible(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<bool> {
    self.inner.is_visible().await.into_js_with(&ctx)
  }

  #[qjs(rename = "isHidden")]
  pub async fn is_hidden(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<bool> {
    self.inner.is_hidden().await.into_js_with(&ctx)
  }

  #[qjs(rename = "isDisabled")]
  pub async fn is_disabled(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<bool> {
    self.inner.is_disabled().await.into_js_with(&ctx)
  }

  #[qjs(rename = "isEnabled")]
  pub async fn is_enabled(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<bool> {
    self.inner.is_enabled().await.into_js_with(&ctx)
  }

  #[qjs(rename = "isChecked")]
  pub async fn is_checked(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<bool> {
    self.inner.is_checked().await.into_js_with(&ctx)
  }

  #[qjs(rename = "isEditable")]
  pub async fn is_editable(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<bool> {
    self.inner.is_editable().await.into_js_with(&ctx)
  }

  // ── Geometry (Phase E) ───────────────────────────────────────────────

  /// Playwright: `elementHandle.boundingBox()`. Returns a plain object
  /// `{x, y, width, height}` or `null`.
  #[qjs(rename = "boundingBox")]
  pub async fn bounding_box<'js>(&self, ctx: rquickjs::Ctx<'js>) -> rquickjs::Result<rquickjs::Value<'js>> {
    let bbox = self.inner.bounding_box().await.into_js_with(&ctx)?;
    match bbox {
      None => Ok(rquickjs::Value::new_null(ctx)),
      Some(b) => {
        let obj = rquickjs::Object::new(ctx.clone())?;
        obj.set("x", b.x)?;
        obj.set("y", b.y)?;
        obj.set("width", b.width)?;
        obj.set("height", b.height)?;
        Ok(obj.into_value())
      },
    }
  }

  // ── Actions (Phase E) ────────────────────────────────────────────────

  #[qjs(rename = "click")]
  pub async fn click<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_click_options(&ctx, options)?;
    self.inner.click(opts).await.into_js_with(&ctx)
  }

  #[qjs(rename = "dblclick")]
  pub async fn dblclick<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_dblclick_options(&ctx, options)?;
    self.inner.dblclick(opts).await.into_js_with(&ctx)
  }

  #[qjs(rename = "hover")]
  pub async fn hover<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_hover_options(&ctx, options)?;
    self.inner.hover(opts).await.into_js_with(&ctx)
  }

  #[qjs(rename = "type")]
  pub async fn type_str<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    text: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_type_options(&ctx, options)?;
    self.inner.type_str(&text, opts).await.into_js_with(&ctx)
  }

  #[qjs(rename = "focus")]
  pub async fn focus(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<()> {
    self.inner.focus().await.into_js_with(&ctx)
  }

  #[qjs(rename = "scrollIntoViewIfNeeded")]
  pub async fn scroll_into_view_if_needed(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<()> {
    self.inner.scroll_into_view_if_needed().await.into_js_with(&ctx)
  }

  /// Playwright: `elementHandle.screenshot(opts?)`. Today accepts
  /// `{ type?: 'png'|'jpeg'|'webp' }` via the `opts.type` field;
  /// additional `ScreenshotOpts` fields are carried at the core layer
  /// and take effect once the locator-level screenshot gets the full
  /// bag.
  #[qjs(rename = "screenshot")]
  pub async fn screenshot<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<Vec<u8>> {
    let format = parse_screenshot_format(&ctx, options)?;
    self.inner.screenshot(format).await.into_js_with(&ctx)
  }

  // ── $eval / $$eval (Playwright parity) ───────────────────────────────

  /// Playwright: `elementHandle.$eval(selector, pageFunction, arg?)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:215`).
  #[qjs(rename = "$eval")]
  pub async fn dollar_eval<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    page_function: rquickjs::Value<'js>,
    arg: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    let (source, _is_fn) = extract_page_function(&ctx, page_function)?;
    let serialized = quickjs_arg_to_serialized(&ctx, arg.0)?;
    let result = self
      .inner
      .eval_on_selector(&selector, &source, serialized)
      .await
      .into_js_with(&ctx)?;
    serialized_value_to_quickjs(&ctx, &result)
  }

  /// Playwright: `elementHandle.$$eval(selector, pageFunction, arg?)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:220`).
  #[qjs(rename = "$$eval")]
  pub async fn dollar_dollar_eval<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    page_function: rquickjs::Value<'js>,
    arg: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    let (source, _is_fn) = extract_page_function(&ctx, page_function)?;
    let serialized = quickjs_arg_to_serialized(&ctx, arg.0)?;
    let result = self
      .inner
      .eval_on_selector_all(&selector, &source, serialized)
      .await
      .into_js_with(&ctx)?;
    serialized_value_to_quickjs(&ctx, &result)
  }

  // ── $ / $$ (query shortcuts — Playwright parity) ─────────────────────

  /// Playwright: `elementHandle.$(selector): Promise<ElementHandle | null>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:206`).
  #[qjs(rename = "$")]
  pub async fn query_selector(
    &self,
    ctx: rquickjs::Ctx<'_>,
    selector: String,
  ) -> rquickjs::Result<Option<ElementHandleJs>> {
    let maybe = self.inner.query_selector(&selector).await.into_js_with(&ctx)?;
    Ok(maybe.map(ElementHandleJs::new))
  }

  /// Playwright: `elementHandle.$$(selector): Promise<ElementHandle[]>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:210`).
  #[qjs(rename = "$$")]
  pub async fn query_selector_all(
    &self,
    ctx: rquickjs::Ctx<'_>,
    selector: String,
  ) -> rquickjs::Result<Vec<ElementHandleJs>> {
    let handles = self.inner.query_selector_all(&selector).await.into_js_with(&ctx)?;
    Ok(handles.into_iter().map(ElementHandleJs::new).collect())
  }

  // ── Frame accessors ──────────────────────────────────────────────────

  /// Playwright: `elementHandle.ownerFrame(): Promise<Frame | null>`.
  #[qjs(rename = "ownerFrame")]
  pub async fn owner_frame(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<Option<crate::bindings::frame::FrameJs>> {
    let maybe = self.inner.owner_frame().await.into_js_with(&ctx)?;
    Ok(maybe.map(crate::bindings::frame::FrameJs::new))
  }

  /// Playwright: `elementHandle.contentFrame(): Promise<Frame | null>`.
  #[qjs(rename = "contentFrame")]
  pub async fn content_frame(
    &self,
    ctx: rquickjs::Ctx<'_>,
  ) -> rquickjs::Result<Option<crate::bindings::frame::FrameJs>> {
    let maybe = self.inner.content_frame().await.into_js_with(&ctx)?;
    Ok(maybe.map(crate::bindings::frame::FrameJs::new))
  }

  // ── Wait helpers ─────────────────────────────────────────────────────

  /// Playwright: `elementHandle.waitForElementState(state, options?)`.
  #[qjs(rename = "waitForElementState")]
  pub async fn wait_for_element_state(
    &self,
    ctx: rquickjs::Ctx<'_>,
    state: String,
    timeout: rquickjs::function::Opt<f64>,
  ) -> rquickjs::Result<()> {
    let st = ferridriver::ElementState::parse(&state).into_js_with(&ctx)?;
    let timeout_ms = timeout.0.map(|ms| ms as u64);
    self
      .inner
      .wait_for_element_state(st, timeout_ms)
      .await
      .into_js_with(&ctx)
  }

  /// Playwright: `elementHandle.waitForSelector(selector, options?)`.
  #[qjs(rename = "waitForSelector")]
  pub async fn wait_for_selector(
    &self,
    ctx: rquickjs::Ctx<'_>,
    selector: String,
    timeout: rquickjs::function::Opt<f64>,
  ) -> rquickjs::Result<Option<ElementHandleJs>> {
    let timeout_ms = timeout.0.map(|ms| ms as u64);
    let maybe = self
      .inner
      .wait_for_selector(&selector, timeout_ms)
      .await
      .into_js_with(&ctx)?;
    Ok(maybe.map(ElementHandleJs::new))
  }

  // ── Action methods (temp-tag bridge) ─────────────────────────────────

  /// Playwright: `elementHandle.fill(value, options?)`.
  #[qjs(rename = "fill")]
  pub async fn fill<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    value: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_fill_options(&ctx, options)?;
    self.inner.fill(&value, opts).await.into_js_with(&ctx)
  }

  /// Playwright: `elementHandle.check(options?)`.
  #[qjs(rename = "check")]
  pub async fn check<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_check_options(&ctx, options)?;
    self.inner.check(opts).await.into_js_with(&ctx)
  }

  /// Playwright: `elementHandle.uncheck(options?)`.
  #[qjs(rename = "uncheck")]
  pub async fn uncheck<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_check_options(&ctx, options)?;
    self.inner.uncheck(opts).await.into_js_with(&ctx)
  }

  /// Playwright: `elementHandle.setChecked(checked, options?)`.
  #[qjs(rename = "setChecked")]
  pub async fn set_checked<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    checked: bool,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_check_options(&ctx, options)?;
    self.inner.set_checked(checked, opts).await.into_js_with(&ctx)
  }

  /// Playwright: `elementHandle.tap(options?)`.
  #[qjs(rename = "tap")]
  pub async fn tap<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_tap_options(&ctx, options)?;
    self.inner.tap(opts).await.into_js_with(&ctx)
  }

  /// Playwright: `elementHandle.press(key, options?)`.
  #[qjs(rename = "press")]
  pub async fn press<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    key: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_press_options(&ctx, options)?;
    self.inner.press(&key, opts).await.into_js_with(&ctx)
  }

  /// Playwright: `elementHandle.dispatchEvent(type, eventInit?)`.
  #[qjs(rename = "dispatchEvent")]
  pub async fn dispatch_event<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    event_type: String,
    event_init: rquickjs::function::Opt<rquickjs::Value<'js>>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let init_json = match event_init.0 {
      Some(v) if !v.is_undefined() && !v.is_null() => {
        Some(crate::bindings::convert::serde_from_js::<serde_json::Value>(&ctx, v)?)
      },
      _ => None,
    };
    let opts = crate::bindings::convert::parse_dispatch_event_options(&ctx, options)?;
    self
      .inner
      .dispatch_event(&event_type, init_json, opts)
      .await
      .into_js_with(&ctx)
  }

  /// Playwright: `elementHandle.selectOption(values, options?)`.
  #[qjs(rename = "selectOption")]
  pub async fn select_option<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    values: rquickjs::Value<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<Vec<String>> {
    let values = crate::bindings::convert::parse_select_option_values(&ctx, values)?;
    let opts = crate::bindings::convert::parse_select_option_options(&ctx, options)?;
    self.inner.select_option(values, opts).await.into_js_with(&ctx)
  }

  /// Playwright: `elementHandle.selectText(options?)`.
  #[qjs(rename = "selectText")]
  pub async fn select_text(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<()> {
    self.inner.select_text().await.into_js_with(&ctx)
  }

  /// Playwright: `elementHandle.setInputFiles(files, options?)`.
  #[qjs(rename = "setInputFiles")]
  pub async fn set_input_files<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    files: rquickjs::Value<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let files = crate::bindings::convert::parse_input_files(&ctx, files)?;
    let opts = crate::bindings::convert::parse_set_input_files_options(&ctx, options)?;
    self.inner.set_input_files(files, opts).await.into_js_with(&ctx)
  }
}
