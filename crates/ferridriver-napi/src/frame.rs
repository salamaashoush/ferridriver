//! Frame class -- NAPI binding for `ferridriver::Frame`.

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
  #[napi(getter)]
  pub fn name(&self) -> String {
    self.inner.name().to_string()
  }

  /// Frame URL.
  #[napi(getter)]
  pub fn url(&self) -> String {
    self.inner.url().to_string()
  }

  /// Whether this is the main (top-level) frame.
  #[napi]
  pub fn is_main_frame(&self) -> bool {
    self.inner.is_main_frame()
  }

  /// Get the parent frame. Returns null for the main frame.
  #[napi]
  pub async fn parent_frame(&self) -> Result<Option<Frame>> {
    self
      .inner
      .parent_frame()
      .await
      .map(|opt| opt.map(Frame::wrap))
      .map_err(napi::Error::from_reason)
  }

  /// Get child frames of this frame.
  #[napi]
  pub async fn child_frames(&self) -> Result<Vec<Frame>> {
    self
      .inner
      .child_frames()
      .await
      .map(|frames| frames.into_iter().map(Frame::wrap).collect())
      .map_err(napi::Error::from_reason)
  }

  // ── Evaluation ────────────────────────────────────────────────────────

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

  // ── Locators ──────────────────────────────────────────────────────────

  #[napi]
  pub fn locator(&self, selector: String) -> Locator {
    Locator::wrap(self.inner.locator(&selector))
  }

  #[napi]
  pub fn get_by_role(&self, role: String, options: Option<crate::types::RoleOptions>) -> Locator {
    let opts: ferridriver::options::RoleOptions = options.as_ref().map_or_else(Default::default, Into::into);
    Locator::wrap(self.inner.get_by_role(&role, &opts))
  }

  #[napi]
  pub fn get_by_text(&self, text: String, options: Option<crate::types::TextOptions>) -> Locator {
    let opts: ferridriver::options::TextOptions = options.as_ref().map_or_else(Default::default, Into::into);
    Locator::wrap(self.inner.get_by_text(&text, &opts))
  }

  #[napi]
  pub fn get_by_test_id(&self, test_id: String) -> Locator {
    Locator::wrap(self.inner.get_by_test_id(&test_id))
  }

  // ── Content ───────────────────────────────────────────────────────────

  #[napi]
  pub async fn content(&self) -> Result<String> {
    self.inner.content().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn title(&self) -> Result<String> {
    self.inner.title().await.map_err(napi::Error::from_reason)
  }

  // ── Navigation ────────────────────────────────────────────────────────

  #[napi]
  pub async fn goto(&self, url: String) -> Result<()> {
    self.inner.goto(&url).await.map_err(napi::Error::from_reason)
  }

  // ── Waiting ───────────────────────────────────────────────────────────

  #[napi]
  pub async fn wait_for_load_state(&self) -> Result<()> {
    self.inner.wait_for_load_state().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn wait_for_selector(&self, selector: String, options: Option<crate::types::WaitOptions>) -> Result<()> {
    let opts: ferridriver::options::WaitOptions = options.as_ref().map_or_else(Default::default, Into::into);
    self
      .inner
      .wait_for_selector(&selector, opts)
      .await
      .map_err(napi::Error::from_reason)
  }

  // ── Additional content methods ───────────────────────────────────────

  #[napi]
  pub async fn set_content(&self, html: String) -> Result<()> {
    self.inner.set_content(&html).await.map_err(napi::Error::from_reason)
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
  pub async fn is_detached(&self) -> Result<bool> {
    self.inner.is_detached().await.map_err(napi::Error::from_reason)
  }

  // ── Additional locators ──────────────────────────────────────────────

  #[napi]
  pub fn get_by_label(&self, text: String, options: Option<crate::types::TextOptions>) -> Locator {
    let opts: ferridriver::options::TextOptions = options.as_ref().map_or_else(Default::default, Into::into);
    Locator::wrap(self.inner.get_by_label(&text, &opts))
  }

  #[napi]
  pub fn get_by_placeholder(&self, text: String, options: Option<crate::types::TextOptions>) -> Locator {
    let opts: ferridriver::options::TextOptions = options.as_ref().map_or_else(Default::default, Into::into);
    Locator::wrap(self.inner.get_by_placeholder(&text, &opts))
  }
}
