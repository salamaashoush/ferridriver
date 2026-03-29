//! Locator class -- NAPI binding for `ferridriver::Locator`.

use crate::types::{RoleOptions, TextOptions, FilterOptions, BoundingBox, WaitOptions};
use napi::bindgen_prelude::Buffer;
use napi::Result;
use napi_derive::napi;

/// A lazy element locator. Does not query the DOM until an action is called.
#[napi]
pub struct Locator {
  inner: ferridriver::Locator,
}

impl Locator {
  pub(crate) fn wrap(inner: ferridriver::Locator) -> Self {
    Self { inner }
  }
}

#[napi]
impl Locator {
  /// The selector string for this locator.
  #[napi(getter)]
  pub fn selector(&self) -> String {
    self.inner.selector().to_string()
  }

  // ── Sub-locators ────────────────────────────────────────────────────────

  #[napi]
  pub fn locator(&self, selector: String) -> Locator {
    Self::wrap(self.inner.locator(&selector))
  }

  #[napi]
  pub fn get_by_role(&self, role: String, options: Option<RoleOptions>) -> Locator {
    let opts: ferridriver::options::RoleOptions = options.as_ref().map_or_else(Default::default, Into::into);
    Self::wrap(self.inner.get_by_role(&role, &opts))
  }

  #[napi]
  pub fn get_by_text(&self, text: String, options: Option<TextOptions>) -> Locator {
    let opts: ferridriver::options::TextOptions = options.as_ref().map_or_else(Default::default, Into::into);
    Self::wrap(self.inner.get_by_text(&text, &opts))
  }

  #[napi]
  pub fn get_by_label(&self, text: String, options: Option<TextOptions>) -> Locator {
    let opts: ferridriver::options::TextOptions = options.as_ref().map_or_else(Default::default, Into::into);
    Self::wrap(self.inner.get_by_label(&text, &opts))
  }

  #[napi]
  pub fn get_by_placeholder(&self, text: String, options: Option<TextOptions>) -> Locator {
    let opts: ferridriver::options::TextOptions = options.as_ref().map_or_else(Default::default, Into::into);
    Self::wrap(self.inner.get_by_placeholder(&text, &opts))
  }

  #[napi]
  pub fn get_by_alt_text(&self, text: String, options: Option<TextOptions>) -> Locator {
    let opts: ferridriver::options::TextOptions = options.as_ref().map_or_else(Default::default, Into::into);
    Self::wrap(self.inner.get_by_alt_text(&text, &opts))
  }

  #[napi]
  pub fn get_by_title(&self, text: String, options: Option<TextOptions>) -> Locator {
    let opts: ferridriver::options::TextOptions = options.as_ref().map_or_else(Default::default, Into::into);
    Self::wrap(self.inner.get_by_title(&text, &opts))
  }

  #[napi]
  pub fn get_by_test_id(&self, test_id: String) -> Locator {
    Self::wrap(self.inner.get_by_test_id(&test_id))
  }

  #[napi]
  pub fn first(&self) -> Locator {
    Self::wrap(self.inner.first())
  }

  #[napi]
  pub fn last(&self) -> Locator {
    Self::wrap(self.inner.last())
  }

  #[napi]
  pub fn nth(&self, index: i32) -> Locator {
    Self::wrap(self.inner.nth(index))
  }

  #[napi]
  pub fn filter(&self, options: FilterOptions) -> Locator {
    Self::wrap(self.inner.filter(&ferridriver::options::FilterOptions::from(&options)))
  }

  // ── Actions ─────────────────────────────────────────────────────────────

  #[napi]
  pub async fn click(&self) -> Result<()> {
    self.inner.click().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn dblclick(&self) -> Result<()> {
    self.inner.dblclick().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn fill(&self, value: String) -> Result<()> {
    self.inner.fill(&value).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn clear(&self) -> Result<()> {
    self.inner.clear().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn type_text(&self, text: String) -> Result<()> {
    self.inner.type_text(&text).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn press(&self, key: String) -> Result<()> {
    self.inner.press(&key).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn hover(&self) -> Result<()> {
    self.inner.hover().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn focus(&self) -> Result<()> {
    self.inner.focus().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn blur(&self) -> Result<()> {
    self.inner.blur().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn check(&self) -> Result<()> {
    self.inner.check().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn uncheck(&self) -> Result<()> {
    self.inner.uncheck().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn select_option(&self, value: String) -> Result<Vec<String>> {
    self.inner.select_option(&value).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn scroll_into_view(&self) -> Result<()> {
    self.inner.scroll_into_view().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn dispatch_event(&self, event_type: String) -> Result<()> {
    self.inner.dispatch_event(&event_type).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn press_sequentially(&self, text: String, delay_ms: Option<f64>) -> Result<()> {
    self.inner.press_sequentially(&text, delay_ms.map(crate::types::f64_to_u64))
      .await
      .map_err(napi::Error::from_reason)
  }

  // ── Content & state ─────────────────────────────────────────────────────

  #[napi]
  pub async fn text_content(&self) -> Result<Option<String>> {
    self.inner.text_content().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn inner_text(&self) -> Result<String> {
    self.inner.inner_text().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn inner_html(&self) -> Result<String> {
    self.inner.inner_html().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn get_attribute(&self, name: String) -> Result<Option<String>> {
    self.inner.get_attribute(&name).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn input_value(&self) -> Result<String> {
    self.inner.input_value().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn is_visible(&self) -> Result<bool> {
    self.inner.is_visible().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn is_hidden(&self) -> Result<bool> {
    self.inner.is_hidden().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn is_enabled(&self) -> Result<bool> {
    self.inner.is_enabled().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn is_disabled(&self) -> Result<bool> {
    self.inner.is_disabled().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn is_checked(&self) -> Result<bool> {
    self.inner.is_checked().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn is_editable(&self) -> Result<bool> {
    self.inner.is_editable().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn count(&self) -> Result<i32> {
    let n = self.inner.count().await.map_err(napi::Error::from_reason)?;
    i32::try_from(n).map_err(|_| napi::Error::from_reason(format!("element count {n} exceeds i32::MAX")))
  }

  #[napi]
  pub async fn bounding_box(&self) -> Result<Option<BoundingBox>> {
    let bb = self.inner.bounding_box().await.map_err(napi::Error::from_reason)?;
    Ok(bb.map(|b| BoundingBox { x: b.x, y: b.y, width: b.width, height: b.height }))
  }

  // ── Waiting ─────────────────────────────────────────────────────────────

  #[napi]
  pub async fn wait_for(&self, options: Option<WaitOptions>) -> Result<()> {
    let opts: ferridriver::options::WaitOptions = options.as_ref().map_or_else(Default::default, Into::into);
    self.inner.wait_for(opts).await.map_err(napi::Error::from_reason)
  }

  // ── Screenshot ──────────────────────────────────────────────────────────

  #[napi]
  pub async fn screenshot(&self) -> Result<Buffer> {
    let bytes = self.inner.screenshot().await.map_err(napi::Error::from_reason)?;
    Ok(bytes.into())
  }

  // ── All matches ─────────────────────────────────────────────────────────

  #[napi]
  pub async fn all_text_contents(&self) -> Result<Vec<String>> {
    self.inner.all_text_contents().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn all_inner_texts(&self) -> Result<Vec<String>> {
    self.inner.all_inner_texts().await.map_err(napi::Error::from_reason)
  }

  // ── Missing methods ─────────────────────────────────────────────────────

  #[napi]
  pub async fn right_click(&self) -> Result<()> {
    self.inner.right_click().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn tap(&self) -> Result<()> {
    self.inner.tap().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn select_text(&self) -> Result<()> {
    self.inner.select_text().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn set_checked(&self, checked: bool) -> Result<()> {
    self.inner.set_checked(checked).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn set_input_files(&self, paths: Vec<String>) -> Result<()> {
    self.inner.set_input_files(&paths).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn is_attached(&self) -> Result<bool> {
    self.inner.is_attached().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn evaluate(&self, expression: String) -> Result<Option<serde_json::Value>> {
    self.inner.evaluate(&expression).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn evaluate_all(&self, expression: String) -> Result<Option<serde_json::Value>> {
    self.inner.evaluate_all(&expression).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub fn or_locator(&self, other: &Locator) -> Locator {
    Locator { inner: self.inner.or(&other.inner) }
  }

  #[napi]
  pub fn and_locator(&self, other: &Locator) -> Locator {
    Locator { inner: self.inner.and(&other.inner) }
  }

  #[napi]
  pub async fn all(&self) -> Result<Vec<Locator>> {
    let locators = self.inner.all().await.map_err(napi::Error::from_reason)?;
    Ok(locators.into_iter().map(|l| Locator { inner: l }).collect())
  }

}
