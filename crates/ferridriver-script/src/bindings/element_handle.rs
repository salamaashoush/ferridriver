//! `ElementHandleJs`: QuickJS wrapper around `ferridriver::ElementHandle`.
//!
//! Phase-C surface covers lifecycle — `dispose`, `isDisposed`, `asJSHandle`
//! — enough to exercise the per-backend release paths from `run_script`.
//! Phase E bolts the ~25 Playwright DOM methods on top of this same class.

use ferridriver::ElementHandle;
use ferridriver::backend::ImageFormat;
use rquickjs::JsLifetime;
use rquickjs::class::Trace;

use crate::bindings::convert::{
  FerriResultExt, json_value_to_quickjs, quickjs_arg_to_serialized, serialized_value_to_quickjs,
};

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
  /// `true` once [`Self::dispose`] has run.
  #[qjs(get, rename = "isDisposed")]
  pub fn is_disposed(&self) -> bool {
    self.inner.is_disposed()
  }

  /// Release the underlying remote element. Idempotent.
  #[qjs(rename = "dispose")]
  pub async fn dispose(&self) -> rquickjs::Result<()> {
    self.inner.dispose().await.into_js()
  }

  /// Companion [`crate::bindings::js_handle::JSHandleJs`] sharing the
  /// same remote reference. Disposing either releases the remote and
  /// latches both into the disposed state.
  #[qjs(rename = "asJSHandle")]
  pub fn as_js_handle(&self) -> crate::bindings::js_handle::JSHandleJs {
    crate::bindings::js_handle::JSHandleJs::new(self.inner.as_js_handle().clone())
  }

  /// Playwright: `elementHandle.evaluate(fn, arg?)`.
  #[qjs(rename = "evaluateWithArg")]
  pub async fn evaluate_with_arg<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    fn_source: String,
    arg: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    let serialized = quickjs_arg_to_serialized(&ctx, arg.0)?;
    let result = self
      .inner
      .as_js_handle()
      .evaluate_with_arg(&fn_source, serialized, Some(true))
      .await
      .into_js()?;
    serialized_value_to_quickjs(&ctx, &result)
  }

  /// Raw isomorphic wire shape variant of [`Self::evaluateWithArg`].
  #[qjs(rename = "evaluateWithArgWire")]
  pub async fn evaluate_with_arg_wire<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    fn_source: String,
    arg: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    let serialized = quickjs_arg_to_serialized(&ctx, arg.0)?;
    let result = self
      .inner
      .as_js_handle()
      .evaluate_with_arg(&fn_source, serialized, Some(true))
      .await
      .into_js()?;
    let wire = serde_json::to_value(&result)
      .map_err(|e| rquickjs::Error::new_from_js_message("evaluateWithArgWire", "", &e.to_string()))?;
    json_value_to_quickjs(&ctx, &wire)
  }

  /// Playwright: `elementHandle.evaluateHandle(fn, arg?)`.
  #[qjs(rename = "evaluateHandleWithArg")]
  pub async fn evaluate_handle_with_arg<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    fn_source: String,
    arg: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<crate::bindings::js_handle::JSHandleJs> {
    let serialized = quickjs_arg_to_serialized(&ctx, arg.0)?;
    let handle = self
      .inner
      .as_js_handle()
      .evaluate_handle_with_arg(&fn_source, serialized, Some(true))
      .await
      .into_js()?;
    Ok(crate::bindings::js_handle::JSHandleJs::new(handle))
  }

  // ── Content reads (Phase E) ──────────────────────────────────────────

  #[qjs(rename = "innerHTML")]
  pub async fn inner_html(&self) -> rquickjs::Result<String> {
    self.inner.inner_html().await.into_js()
  }

  #[qjs(rename = "innerText")]
  pub async fn inner_text(&self) -> rquickjs::Result<String> {
    self.inner.inner_text().await.into_js()
  }

  #[qjs(rename = "textContent")]
  pub async fn text_content(&self) -> rquickjs::Result<Option<String>> {
    self.inner.text_content().await.into_js()
  }

  #[qjs(rename = "getAttribute")]
  pub async fn get_attribute(&self, name: String) -> rquickjs::Result<Option<String>> {
    self.inner.get_attribute(&name).await.into_js()
  }

  #[qjs(rename = "inputValue")]
  pub async fn input_value(&self) -> rquickjs::Result<String> {
    self.inner.input_value().await.into_js()
  }

  // ── State predicates (Phase E) ───────────────────────────────────────

  #[qjs(rename = "isVisible")]
  pub async fn is_visible(&self) -> rquickjs::Result<bool> {
    self.inner.is_visible().await.into_js()
  }

  #[qjs(rename = "isHidden")]
  pub async fn is_hidden(&self) -> rquickjs::Result<bool> {
    self.inner.is_hidden().await.into_js()
  }

  #[qjs(rename = "isDisabled")]
  pub async fn is_disabled(&self) -> rquickjs::Result<bool> {
    self.inner.is_disabled().await.into_js()
  }

  #[qjs(rename = "isEnabled")]
  pub async fn is_enabled(&self) -> rquickjs::Result<bool> {
    self.inner.is_enabled().await.into_js()
  }

  #[qjs(rename = "isChecked")]
  pub async fn is_checked(&self) -> rquickjs::Result<bool> {
    self.inner.is_checked().await.into_js()
  }

  #[qjs(rename = "isEditable")]
  pub async fn is_editable(&self) -> rquickjs::Result<bool> {
    self.inner.is_editable().await.into_js()
  }

  // ── Geometry (Phase E) ───────────────────────────────────────────────

  /// Playwright: `elementHandle.boundingBox()`. Returns a plain object
  /// `{x, y, width, height}` or `null`.
  #[qjs(rename = "boundingBox")]
  pub async fn bounding_box<'js>(&self, ctx: rquickjs::Ctx<'js>) -> rquickjs::Result<rquickjs::Value<'js>> {
    let bbox = self.inner.bounding_box().await.into_js()?;
    match bbox {
      None => Ok(rquickjs::Value::new_null(ctx)),
      Some(b) => {
        let json = serde_json::json!({"x": b.x, "y": b.y, "width": b.width, "height": b.height});
        json_value_to_quickjs(&ctx, &json)
      },
    }
  }

  // ── Actions (Phase E) ────────────────────────────────────────────────

  #[qjs(rename = "click")]
  pub async fn click(&self) -> rquickjs::Result<()> {
    self.inner.click().await.into_js()
  }

  #[qjs(rename = "dblclick")]
  pub async fn dblclick(&self) -> rquickjs::Result<()> {
    self.inner.dblclick().await.into_js()
  }

  #[qjs(rename = "hover")]
  pub async fn hover(&self) -> rquickjs::Result<()> {
    self.inner.hover().await.into_js()
  }

  #[qjs(rename = "type")]
  pub async fn type_str(&self, text: String) -> rquickjs::Result<()> {
    self.inner.type_str(&text).await.into_js()
  }

  #[qjs(rename = "focus")]
  pub async fn focus(&self) -> rquickjs::Result<()> {
    self.inner.focus().await.into_js()
  }

  #[qjs(rename = "scrollIntoViewIfNeeded")]
  pub async fn scroll_into_view_if_needed(&self) -> rquickjs::Result<()> {
    self.inner.scroll_into_view_if_needed().await.into_js()
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
    self.inner.screenshot(format).await.into_js()
  }

  // ── $eval / $$eval (Playwright parity) ───────────────────────────────

  /// Playwright: `elementHandle.$eval(selector, pageFunction, arg?)`.
  #[qjs(rename = "evalOnSelector")]
  pub async fn eval_on_selector<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    fn_source: String,
    arg: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    let serialized = quickjs_arg_to_serialized(&ctx, arg.0)?;
    let result = self
      .inner
      .eval_on_selector(&selector, &fn_source, serialized)
      .await
      .into_js()?;
    serialized_value_to_quickjs(&ctx, &result)
  }

  /// Playwright: `elementHandle.$$eval(selector, pageFunction, arg?)`.
  #[qjs(rename = "evalOnSelectorAll")]
  pub async fn eval_on_selector_all<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    fn_source: String,
    arg: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    let serialized = quickjs_arg_to_serialized(&ctx, arg.0)?;
    let result = self
      .inner
      .eval_on_selector_all(&selector, &fn_source, serialized)
      .await
      .into_js()?;
    serialized_value_to_quickjs(&ctx, &result)
  }

  // ── Frame accessors ──────────────────────────────────────────────────

  /// Playwright: `elementHandle.ownerFrame(): Promise<Frame | null>`.
  #[qjs(rename = "ownerFrame")]
  pub async fn owner_frame(&self) -> rquickjs::Result<Option<crate::bindings::frame::FrameJs>> {
    let maybe = self.inner.owner_frame().await.into_js()?;
    Ok(maybe.map(crate::bindings::frame::FrameJs::new))
  }

  /// Playwright: `elementHandle.contentFrame(): Promise<Frame | null>`.
  #[qjs(rename = "contentFrame")]
  pub async fn content_frame(&self) -> rquickjs::Result<Option<crate::bindings::frame::FrameJs>> {
    let maybe = self.inner.content_frame().await.into_js()?;
    Ok(maybe.map(crate::bindings::frame::FrameJs::new))
  }

  // ── Wait helpers ─────────────────────────────────────────────────────

  /// Playwright: `elementHandle.waitForElementState(state, options?)`.
  #[qjs(rename = "waitForElementState")]
  pub async fn wait_for_element_state(
    &self,
    state: String,
    timeout: rquickjs::function::Opt<f64>,
  ) -> rquickjs::Result<()> {
    let st = ferridriver::ElementState::parse(&state).into_js()?;
    let timeout_ms = timeout.0.map(|ms| ms as u64);
    self.inner.wait_for_element_state(st, timeout_ms).await.into_js()
  }

  /// Playwright: `elementHandle.waitForSelector(selector, options?)`.
  #[qjs(rename = "waitForSelector")]
  pub async fn wait_for_selector(
    &self,
    selector: String,
    timeout: rquickjs::function::Opt<f64>,
  ) -> rquickjs::Result<Option<ElementHandleJs>> {
    let timeout_ms = timeout.0.map(|ms| ms as u64);
    let maybe = self.inner.wait_for_selector(&selector, timeout_ms).await.into_js()?;
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
    self.inner.fill(&value, opts).await.into_js()
  }

  /// Playwright: `elementHandle.check(options?)`.
  #[qjs(rename = "check")]
  pub async fn check<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_check_options(&ctx, options)?;
    self.inner.check(opts).await.into_js()
  }

  /// Playwright: `elementHandle.uncheck(options?)`.
  #[qjs(rename = "uncheck")]
  pub async fn uncheck<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_check_options(&ctx, options)?;
    self.inner.uncheck(opts).await.into_js()
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
    self.inner.set_checked(checked, opts).await.into_js()
  }

  /// Playwright: `elementHandle.tap(options?)`.
  #[qjs(rename = "tap")]
  pub async fn tap<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_tap_options(&ctx, options)?;
    self.inner.tap(opts).await.into_js()
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
    self.inner.press(&key, opts).await.into_js()
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
    self.inner.dispatch_event(&event_type, init_json, opts).await.into_js()
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
    self.inner.select_option(values, opts).await.into_js()
  }

  /// Playwright: `elementHandle.selectText(options?)`.
  #[qjs(rename = "selectText")]
  pub async fn select_text(&self) -> rquickjs::Result<()> {
    self.inner.select_text().await.into_js()
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
    self.inner.set_input_files(files, opts).await.into_js()
  }
}
