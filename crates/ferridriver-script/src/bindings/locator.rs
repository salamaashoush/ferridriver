//! `LocatorJs`: JS wrapper around `ferridriver::locator::Locator`.

use ferridriver::locator::Locator;
use rquickjs::JsLifetime;
use rquickjs::class::Trace;

use crate::bindings::convert::FerriResultExt;

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "Locator")]
pub struct LocatorJs {
  #[qjs(skip_trace)]
  inner: Locator,
}

impl LocatorJs {
  #[must_use]
  pub fn new(inner: Locator) -> Self {
    Self { inner }
  }
}

#[rquickjs::methods]
impl LocatorJs {
  // ── Chain/refine (return new Locator) ─────────────────────────────────────

  #[qjs(rename = "locator")]
  pub fn locator(&self, selector: String) -> LocatorJs {
    LocatorJs::new(self.inner.locator(&selector))
  }

  #[qjs(rename = "first")]
  pub fn first(&self) -> LocatorJs {
    LocatorJs::new(self.inner.first())
  }

  #[qjs(rename = "last")]
  pub fn last(&self) -> LocatorJs {
    LocatorJs::new(self.inner.last())
  }

  #[qjs(rename = "nth")]
  pub fn nth(&self, index: i32) -> LocatorJs {
    LocatorJs::new(self.inner.nth(index))
  }

  // ── Interaction ───────────────────────────────────────────────────────────

  #[qjs(rename = "click")]
  pub async fn click(&self) -> rquickjs::Result<()> {
    self.inner.click().await.into_js()
  }

  #[qjs(rename = "dblclick")]
  pub async fn dblclick(&self) -> rquickjs::Result<()> {
    self.inner.dblclick().await.into_js()
  }

  #[qjs(rename = "fill")]
  pub async fn fill(&self, value: String) -> rquickjs::Result<()> {
    self.inner.fill(&value).await.into_js()
  }

  #[qjs(rename = "clear")]
  pub async fn clear(&self) -> rquickjs::Result<()> {
    self.inner.clear().await.into_js()
  }

  #[qjs(rename = "type")]
  pub async fn type_(&self, text: String) -> rquickjs::Result<()> {
    self.inner.r#type(&text).await.into_js()
  }

  #[qjs(rename = "press")]
  pub async fn press(&self, key: String) -> rquickjs::Result<()> {
    self.inner.press(&key).await.into_js()
  }

  #[qjs(rename = "hover")]
  pub async fn hover(&self) -> rquickjs::Result<()> {
    self.inner.hover().await.into_js()
  }

  #[qjs(rename = "focus")]
  pub async fn focus(&self) -> rquickjs::Result<()> {
    self.inner.focus().await.into_js()
  }

  #[qjs(rename = "blur")]
  pub async fn blur(&self) -> rquickjs::Result<()> {
    self.inner.blur().await.into_js()
  }

  #[qjs(rename = "check")]
  pub async fn check(&self) -> rquickjs::Result<()> {
    self.inner.check().await.into_js()
  }

  #[qjs(rename = "uncheck")]
  pub async fn uncheck(&self) -> rquickjs::Result<()> {
    self.inner.uncheck().await.into_js()
  }

  #[qjs(rename = "setChecked")]
  pub async fn set_checked(&self, checked: bool) -> rquickjs::Result<()> {
    self.inner.set_checked(checked).await.into_js()
  }

  #[qjs(rename = "selectOption")]
  pub async fn select_option(&self, value: String) -> rquickjs::Result<Vec<String>> {
    self.inner.select_option(&value).await.into_js()
  }

  #[qjs(rename = "scrollIntoViewIfNeeded")]
  pub async fn scroll_into_view_if_needed(&self) -> rquickjs::Result<()> {
    self.inner.scroll_into_view_if_needed().await.into_js()
  }

  // ── Info ──────────────────────────────────────────────────────────────────

  #[qjs(rename = "count")]
  pub async fn count(&self) -> rquickjs::Result<i32> {
    self
      .inner
      .count()
      .await
      .into_js()
      .map(|c| i32::try_from(c).unwrap_or(i32::MAX))
  }

  #[qjs(rename = "textContent")]
  pub async fn text_content(&self) -> rquickjs::Result<Option<String>> {
    self.inner.text_content().await.into_js()
  }

  #[qjs(rename = "innerText")]
  pub async fn inner_text(&self) -> rquickjs::Result<String> {
    self.inner.inner_text().await.into_js()
  }

  #[qjs(rename = "innerHTML")]
  pub async fn inner_html(&self) -> rquickjs::Result<String> {
    self.inner.inner_html().await.into_js()
  }

  #[qjs(rename = "inputValue")]
  pub async fn input_value(&self) -> rquickjs::Result<String> {
    self.inner.input_value().await.into_js()
  }

  #[qjs(rename = "getAttribute")]
  pub async fn get_attribute(&self, name: String) -> rquickjs::Result<Option<String>> {
    self.inner.get_attribute(&name).await.into_js()
  }

  #[qjs(rename = "isVisible")]
  pub async fn is_visible(&self) -> rquickjs::Result<bool> {
    self.inner.is_visible().await.into_js()
  }

  #[qjs(rename = "isHidden")]
  pub async fn is_hidden(&self) -> rquickjs::Result<bool> {
    self.inner.is_hidden().await.into_js()
  }

  #[qjs(rename = "isEnabled")]
  pub async fn is_enabled(&self) -> rquickjs::Result<bool> {
    self.inner.is_enabled().await.into_js()
  }

  #[qjs(rename = "isDisabled")]
  pub async fn is_disabled(&self) -> rquickjs::Result<bool> {
    self.inner.is_disabled().await.into_js()
  }

  #[qjs(rename = "isChecked")]
  pub async fn is_checked(&self) -> rquickjs::Result<bool> {
    self.inner.is_checked().await.into_js()
  }

  #[qjs(rename = "isEditable")]
  pub async fn is_editable(&self) -> rquickjs::Result<bool> {
    self.inner.is_editable().await.into_js()
  }

  #[qjs(rename = "isAttached")]
  pub async fn is_attached(&self) -> rquickjs::Result<bool> {
    self.inner.is_attached().await.into_js()
  }

  // ── All variants ──────────────────────────────────────────────────────────

  #[qjs(rename = "allTextContents")]
  pub async fn all_text_contents(&self) -> rquickjs::Result<Vec<String>> {
    self.inner.all_text_contents().await.into_js()
  }

  #[qjs(rename = "allInnerTexts")]
  pub async fn all_inner_texts(&self) -> rquickjs::Result<Vec<String>> {
    self.inner.all_inner_texts().await.into_js()
  }

  // ── Evaluation ────────────────────────────────────────────────────────────

  /// Evaluate `expression` against this locator's first element. Returns the
  /// JSON-encoded result as a string (or `null`).
  ///
  /// Parity gap: core takes a string, not a function. See
  /// `PLAYWRIGHT_COMPAT.md` "Gaps surfaced by scripting bindings" item 8.
  #[qjs(rename = "evaluate")]
  pub async fn evaluate(&self, expression: String) -> rquickjs::Result<Option<String>> {
    let value = self.inner.evaluate(&expression).await.into_js()?;
    Ok(value.map(|v| serde_json::to_string(&v).unwrap_or_default()))
  }
}
