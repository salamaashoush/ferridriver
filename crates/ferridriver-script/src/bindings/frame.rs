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
  pub async fn click(&self, selector: String) -> rquickjs::Result<()> {
    self.inner.click(&selector).await.into_js()
  }

  #[qjs(rename = "dblclick")]
  pub async fn dblclick(&self, selector: String) -> rquickjs::Result<()> {
    self.inner.dblclick(&selector).await.into_js()
  }

  #[qjs(rename = "hover")]
  pub async fn hover(&self, selector: String) -> rquickjs::Result<()> {
    self.inner.hover(&selector).await.into_js()
  }

  #[qjs(rename = "tap")]
  pub async fn tap(&self, selector: String) -> rquickjs::Result<()> {
    self.inner.tap(&selector).await.into_js()
  }

  #[qjs(rename = "focus")]
  pub async fn focus(&self, selector: String) -> rquickjs::Result<()> {
    self.inner.focus(&selector).await.into_js()
  }

  #[qjs(rename = "fill")]
  pub async fn fill(&self, selector: String, value: String) -> rquickjs::Result<()> {
    self.inner.fill(&selector, &value).await.into_js()
  }

  #[qjs(rename = "type")]
  pub async fn type_text(&self, selector: String, text: String) -> rquickjs::Result<()> {
    self.inner.r#type(&selector, &text).await.into_js()
  }

  #[qjs(rename = "press")]
  pub async fn press(&self, selector: String, key: String) -> rquickjs::Result<()> {
    self.inner.press(&selector, &key).await.into_js()
  }

  #[qjs(rename = "check")]
  pub async fn check(&self, selector: String) -> rquickjs::Result<()> {
    self.inner.check(&selector).await.into_js()
  }

  #[qjs(rename = "uncheck")]
  pub async fn uncheck(&self, selector: String) -> rquickjs::Result<()> {
    self.inner.uncheck(&selector).await.into_js()
  }

  #[qjs(rename = "setChecked")]
  pub async fn set_checked(&self, selector: String, checked: bool) -> rquickjs::Result<()> {
    self.inner.set_checked(&selector, checked).await.into_js()
  }

  #[qjs(rename = "selectOption")]
  pub async fn select_option(&self, selector: String, value: String) -> rquickjs::Result<Vec<String>> {
    self.inner.select_option(&selector, &value).await.into_js()
  }

  #[qjs(rename = "setInputFiles")]
  pub async fn set_input_files(&self, selector: String, paths: Vec<String>) -> rquickjs::Result<()> {
    self.inner.set_input_files(&selector, &paths).await.into_js()
  }

  /// Drag from `source` to `target` selectors within this frame.
  /// Options ride on Locator's drag option bag.
  #[qjs(rename = "dragAndDrop")]
  pub async fn drag_and_drop(&self, source: String, target: String) -> rquickjs::Result<()> {
    self.inner.drag_and_drop(&source, &target, None).await.into_js()
  }

  #[qjs(rename = "dispatchEvent")]
  pub async fn dispatch_event(&self, selector: String, event_type: String) -> rquickjs::Result<()> {
    self.inner.dispatch_event(&selector, &event_type).await.into_js()
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
