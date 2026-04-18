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

  #[napi]
  pub async fn evaluate(&self, expression: String) -> Result<Option<serde_json::Value>> {
    self.inner.evaluate(&expression).await.into_napi()
  }

  #[napi]
  pub async fn evaluate_str(&self, expression: String) -> Result<String> {
    self.inner.evaluate_str(&expression).await.into_napi()
  }

  // ── Locators ──────────────────────────────────────────────────────────

  /// Playwright: `frame.locator(selector, options?: LocatorOptions): Locator`.
  /// Thin delegator to Rust core's `Frame::locator`.
  #[napi]
  pub fn locator(&self, selector: String, options: Option<crate::types::FilterOptions>) -> Locator {
    let opts = options.map(ferridriver::options::FilterOptions::from);
    Locator::wrap(self.inner.locator(&selector, opts))
  }

  #[napi]
  pub fn get_by_role(&self, role: String, options: Option<crate::types::RoleOptions>) -> Locator {
    let opts: ferridriver::options::RoleOptions = options.map_or_else(Default::default, Into::into);
    Locator::wrap(self.inner.get_by_role(&role, &opts))
  }

  #[napi]
  pub fn get_by_text(&self, text: String, options: Option<crate::types::TextOptions>) -> Locator {
    let opts: ferridriver::options::TextOptions = options.map_or_else(Default::default, Into::into);
    Locator::wrap(self.inner.get_by_text(&text, &opts))
  }

  #[napi]
  pub fn get_by_test_id(&self, test_id: String) -> Locator {
    Locator::wrap(self.inner.get_by_test_id(&test_id))
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

  #[napi]
  pub async fn goto(&self, url: String) -> Result<()> {
    self.inner.goto(&url).await.into_napi()
  }

  // ── Waiting ───────────────────────────────────────────────────────────

  #[napi]
  pub async fn wait_for_load_state(&self) -> Result<()> {
    self.inner.wait_for_load_state().await.into_napi()
  }

  #[napi]
  pub async fn wait_for_selector(&self, selector: String, options: Option<crate::types::WaitOptions>) -> Result<()> {
    let opts: ferridriver::options::WaitOptions = options.map_or_else(Default::default, Into::into);
    self.inner.wait_for_selector(&selector, opts).await.into_napi()
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

  #[napi]
  pub fn get_by_label(&self, text: String, options: Option<crate::types::TextOptions>) -> Locator {
    let opts: ferridriver::options::TextOptions = options.map_or_else(Default::default, Into::into);
    Locator::wrap(self.inner.get_by_label(&text, &opts))
  }

  #[napi]
  pub fn get_by_placeholder(&self, text: String, options: Option<crate::types::TextOptions>) -> Locator {
    let opts: ferridriver::options::TextOptions = options.map_or_else(Default::default, Into::into);
    Locator::wrap(self.inner.get_by_placeholder(&text, &opts))
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
    self.inner.click(&selector, opts).await.into_napi()
  }

  #[napi]
  pub async fn dblclick(&self, selector: String) -> Result<()> {
    self.inner.dblclick(&selector).await.into_napi()
  }

  #[napi]
  pub async fn hover(&self, selector: String) -> Result<()> {
    self.inner.hover(&selector).await.into_napi()
  }

  #[napi]
  pub async fn tap(&self, selector: String) -> Result<()> {
    self.inner.tap(&selector).await.into_napi()
  }

  #[napi]
  pub async fn focus(&self, selector: String) -> Result<()> {
    self.inner.focus(&selector).await.into_napi()
  }

  #[napi]
  pub async fn fill(&self, selector: String, value: String) -> Result<()> {
    self.inner.fill(&selector, &value).await.into_napi()
  }

  #[napi(js_name = "type")]
  pub async fn type_text(&self, selector: String, text: String) -> Result<()> {
    self.inner.r#type(&selector, &text).await.into_napi()
  }

  #[napi]
  pub async fn press(&self, selector: String, key: String) -> Result<()> {
    self.inner.press(&selector, &key).await.into_napi()
  }

  #[napi]
  pub async fn check(&self, selector: String) -> Result<()> {
    self.inner.check(&selector).await.into_napi()
  }

  #[napi]
  pub async fn uncheck(&self, selector: String) -> Result<()> {
    self.inner.uncheck(&selector).await.into_napi()
  }

  #[napi]
  pub async fn set_checked(&self, selector: String, checked: bool) -> Result<()> {
    self.inner.set_checked(&selector, checked).await.into_napi()
  }

  #[napi]
  pub async fn select_option(&self, selector: String, value: String) -> Result<Vec<String>> {
    self.inner.select_option(&selector, &value).await.into_napi()
  }

  #[napi]
  pub async fn set_input_files(&self, selector: String, paths: Vec<String>) -> Result<()> {
    self.inner.set_input_files(&selector, &paths).await.into_napi()
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
    self.inner.drag_and_drop(&source, &target, opts).await.into_napi()
  }

  #[napi]
  pub async fn dispatch_event(&self, selector: String, event_type: String) -> Result<()> {
    self.inner.dispatch_event(&selector, &event_type).await.into_napi()
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
