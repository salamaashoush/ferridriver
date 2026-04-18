//! `FrameJs`: JS wrapper around `ferridriver::Frame`.
//!
//! Mirrors Playwright's
//! [Frame](https://playwright.dev/docs/api/class-frame) sync navigation
//! tree API — `name()`, `url()`, `isMainFrame()`, `parentFrame()`,
//! `childFrames()`, `isDetached()` — plus the small set of async
//! accessors needed for writing scripts that deal with iframes
//! (evaluate / title / content, locator).
//!
//! Action methods (`click`, `fill`, `hover`, etc.) and the full
//! getBy* option surface ship in **task 3.9** (Frame action methods).
//!
//! The underlying `ferridriver::Frame` is a cheap `(Arc<Page>, Arc<str>)`
//! handle — cloning it is free. All name/url/parent/children/detached
//! state is read live from the page-owned frame cache (see
//! `crate::frame_cache::FrameCache`) seeded at `Page::init_frame_cache`.

use ferridriver::Frame;
use rquickjs::JsLifetime;
use rquickjs::class::Trace;

use crate::bindings::convert::FerriResultExt;
use crate::bindings::locator::LocatorJs;

/// JS-visible wrapper around [`ferridriver::Frame`]. Constructed only by
/// `PageJs` / other `FrameJs` instances (`mainFrame`, `frames`, `frame`,
/// `parentFrame`, `childFrames`).
#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "Frame")]
pub struct FrameJs {
  #[qjs(skip_trace)]
  inner: Frame,
}

impl FrameJs {
  #[must_use]
  pub(crate) fn new(inner: Frame) -> Self {
    Self { inner }
  }
}

#[rquickjs::methods]
impl FrameJs {
  // ── Sync frame-tree accessors (Playwright parity, task 3.8) ────────

  /// Frame name (from the `<iframe name=...>` attribute). Sync.
  #[qjs(rename = "name")]
  pub fn name(&self) -> String {
    self.inner.name()
  }

  /// Frame URL. Sync.
  #[qjs(rename = "url")]
  pub fn url(&self) -> String {
    self.inner.url()
  }

  /// True when this is the top-level page frame. Sync.
  #[qjs(rename = "isMainFrame")]
  pub fn is_main_frame(&self) -> bool {
    self.inner.is_main_frame()
  }

  /// Parent frame (null for the main frame). Sync.
  #[qjs(rename = "parentFrame")]
  pub fn parent_frame(&self) -> Option<FrameJs> {
    self.inner.parent_frame().map(FrameJs::new)
  }

  /// Child frames of this frame. Sync.
  #[qjs(rename = "childFrames")]
  pub fn child_frames(&self) -> Vec<FrameJs> {
    self.inner.child_frames().into_iter().map(FrameJs::new).collect()
  }

  /// Whether this frame has been detached from the page. Sync.
  #[qjs(rename = "isDetached")]
  pub fn is_detached(&self) -> bool {
    self.inner.is_detached()
  }

  // ── Evaluation (frame-scoped) ──────────────────────────────────────

  #[qjs(rename = "evaluate")]
  pub async fn evaluate(&self, expression: String) -> rquickjs::Result<Option<String>> {
    self
      .inner
      .evaluate(&expression)
      .await
      .map(|opt| opt.map(|v| v.to_string()))
      .into_js()
  }

  #[qjs(rename = "evaluateStr")]
  pub async fn evaluate_str(&self, expression: String) -> rquickjs::Result<String> {
    self.inner.evaluate_str(&expression).await.into_js()
  }

  #[qjs(rename = "title")]
  pub async fn title(&self) -> rquickjs::Result<String> {
    self.inner.title().await.into_js()
  }

  #[qjs(rename = "content")]
  pub async fn content(&self) -> rquickjs::Result<String> {
    self.inner.content().await.into_js()
  }

  // ── Locator (frame-scoped) ─────────────────────────────────────────

  /// Create a locator scoped to this frame.
  #[qjs(rename = "locator")]
  pub fn locator(&self, selector: String) -> LocatorJs {
    LocatorJs::new(self.inner.locator(&selector, None))
  }

  // ── Action methods (Playwright parity — task 3.9) ──────────────────
  //
  // Mirror the surface from
  // `/tmp/playwright/packages/playwright-core/src/client/frame.ts:296-447`.
  // Each delegates to the Rust core's `Frame::<action>`, which scopes
  // to this frame's execution context via `Frame::locator`.

  #[qjs(rename = "click")]
  pub async fn click<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_click_options(&ctx, options)?;
    self.inner.click(&selector, opts).await.into_js()
  }

  #[qjs(rename = "dblclick")]
  pub async fn dblclick<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_dblclick_options(&ctx, options)?;
    self.inner.dblclick(&selector, opts).await.into_js()
  }

  #[qjs(rename = "hover")]
  pub async fn hover<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_hover_options(&ctx, options)?;
    self.inner.hover(&selector, opts).await.into_js()
  }

  #[qjs(rename = "tap")]
  pub async fn tap<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_tap_options(&ctx, options)?;
    self.inner.tap(&selector, opts).await.into_js()
  }

  #[qjs(rename = "focus")]
  pub async fn focus(&self, selector: String) -> rquickjs::Result<()> {
    self.inner.focus(&selector).await.into_js()
  }

  #[qjs(rename = "fill")]
  pub async fn fill<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    value: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_fill_options(&ctx, options)?;
    self.inner.fill(&selector, &value, opts).await.into_js()
  }

  #[qjs(rename = "type")]
  pub async fn type_text<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    text: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_type_options(&ctx, options)?;
    self.inner.r#type(&selector, &text, opts).await.into_js()
  }

  #[qjs(rename = "press")]
  pub async fn press<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    key: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_press_options(&ctx, options)?;
    self.inner.press(&selector, &key, opts).await.into_js()
  }

  #[qjs(rename = "check")]
  pub async fn check<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_check_options(&ctx, options)?;
    self.inner.check(&selector, opts).await.into_js()
  }

  #[qjs(rename = "uncheck")]
  pub async fn uncheck<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_check_options(&ctx, options)?;
    self.inner.uncheck(&selector, opts).await.into_js()
  }

  #[qjs(rename = "setChecked")]
  pub async fn set_checked<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    checked: bool,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_check_options(&ctx, options)?;
    self.inner.set_checked(&selector, checked, opts).await.into_js()
  }

  #[qjs(rename = "selectOption")]
  pub async fn select_option<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    values: rquickjs::Value<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<Vec<String>> {
    let values = crate::bindings::convert::parse_select_option_values(&ctx, values)?;
    let opts = crate::bindings::convert::parse_select_option_options(&ctx, options)?;
    self.inner.select_option(&selector, values, opts).await.into_js()
  }

  #[qjs(rename = "setInputFiles")]
  pub async fn set_input_files<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    files: rquickjs::Value<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let files = crate::bindings::convert::parse_input_files(&ctx, files)?;
    let opts = crate::bindings::convert::parse_set_input_files_options(&ctx, options)?;
    self.inner.set_input_files(&selector, files, opts).await.into_js()
  }

  /// Drag from `source` to `target` selectors within this frame.
  /// Options ride on Locator's drag option bag.
  #[qjs(rename = "dragAndDrop")]
  pub async fn drag_and_drop(&self, source: String, target: String) -> rquickjs::Result<()> {
    self.inner.drag_and_drop(&source, &target, None).await.into_js()
  }

  #[qjs(rename = "dispatchEvent")]
  pub async fn dispatch_event<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
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
      .dispatch_event(&selector, &event_type, init_json, opts)
      .await
      .into_js()
  }

  #[qjs(rename = "textContent")]
  pub async fn text_content(&self, selector: String) -> rquickjs::Result<Option<String>> {
    self.inner.text_content(&selector).await.into_js()
  }

  #[qjs(rename = "innerText")]
  pub async fn inner_text(&self, selector: String) -> rquickjs::Result<String> {
    self.inner.inner_text(&selector).await.into_js()
  }

  #[qjs(rename = "innerHTML")]
  pub async fn inner_html(&self, selector: String) -> rquickjs::Result<String> {
    self.inner.inner_html(&selector).await.into_js()
  }

  #[qjs(rename = "getAttribute")]
  pub async fn get_attribute(&self, selector: String, name: String) -> rquickjs::Result<Option<String>> {
    self.inner.get_attribute(&selector, &name).await.into_js()
  }

  #[qjs(rename = "inputValue")]
  pub async fn input_value(&self, selector: String) -> rquickjs::Result<String> {
    self.inner.input_value(&selector).await.into_js()
  }

  #[qjs(rename = "isVisible")]
  pub async fn is_visible(&self, selector: String) -> rquickjs::Result<bool> {
    self.inner.is_visible(&selector).await.into_js()
  }

  #[qjs(rename = "isHidden")]
  pub async fn is_hidden(&self, selector: String) -> rquickjs::Result<bool> {
    self.inner.is_hidden(&selector).await.into_js()
  }

  #[qjs(rename = "isEnabled")]
  pub async fn is_enabled(&self, selector: String) -> rquickjs::Result<bool> {
    self.inner.is_enabled(&selector).await.into_js()
  }

  #[qjs(rename = "isDisabled")]
  pub async fn is_disabled(&self, selector: String) -> rquickjs::Result<bool> {
    self.inner.is_disabled(&selector).await.into_js()
  }

  #[qjs(rename = "isEditable")]
  pub async fn is_editable(&self, selector: String) -> rquickjs::Result<bool> {
    self.inner.is_editable(&selector).await.into_js()
  }

  #[qjs(rename = "isChecked")]
  pub async fn is_checked(&self, selector: String) -> rquickjs::Result<bool> {
    self.inner.is_checked(&selector).await.into_js()
  }
}
