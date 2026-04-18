//! `ElementHandleJs`: QuickJS wrapper around `ferridriver::ElementHandle`.
//!
//! Phase-C surface covers lifecycle — `dispose`, `isDisposed`, `asJSHandle`
//! — enough to exercise the per-backend release paths from `run_script`.
//! Phase E bolts the ~25 Playwright DOM methods on top of this same class.

use ferridriver::ElementHandle;
use rquickjs::JsLifetime;
use rquickjs::class::Trace;

use crate::bindings::convert::{
  FerriResultExt, json_value_to_quickjs, quickjs_arg_to_serialized, serialized_value_to_quickjs,
};

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
}
