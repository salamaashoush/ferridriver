//! Page class -- NAPI binding for ferridriver::Page.

use crate::locator::Locator;
use crate::types::*;
use napi::bindgen_prelude::Buffer;
use napi::Result;
use napi_derive::napi;

/// High-level page API, mirrors Playwright's Page interface.
#[napi]
pub struct Page {
  inner: ferridriver::Page,
}

impl Page {
  pub(crate) fn wrap(inner: ferridriver::Page) -> Self {
    Self { inner }
  }
}

#[napi]
impl Page {
  /// Set the default timeout for all operations (milliseconds).
  #[napi]
  pub fn set_default_timeout(&mut self, ms: f64) {
    self.inner.set_default_timeout(ms as u64);
  }

  // ── Navigation ──────────────────────────────────────────────────────────

  #[napi]
  pub async fn goto(&self, url: String) -> Result<()> {
    self.inner.goto(&url).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn go_back(&self) -> Result<()> {
    self.inner.go_back().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn go_forward(&self) -> Result<()> {
    self.inner.go_forward().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn reload(&self) -> Result<()> {
    self.inner.reload().await.map_err(napi::Error::from_reason)
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

  #[napi]
  pub fn locator(&self, selector: String) -> Locator {
    Locator::wrap(self.inner.locator(&selector))
  }

  #[napi]
  pub fn get_by_role(&self, role: String, options: Option<RoleOptions>) -> Locator {
    let opts = options.as_ref().map_or_else(Default::default, Into::into);
    Locator::wrap(self.inner.get_by_role(&role, opts))
  }

  #[napi]
  pub fn get_by_text(&self, text: String, options: Option<TextOptions>) -> Locator {
    let opts = options.as_ref().map_or_else(Default::default, Into::into);
    Locator::wrap(self.inner.get_by_text(&text, opts))
  }

  #[napi]
  pub fn get_by_label(&self, text: String, options: Option<TextOptions>) -> Locator {
    let opts = options.as_ref().map_or_else(Default::default, Into::into);
    Locator::wrap(self.inner.get_by_label(&text, opts))
  }

  #[napi]
  pub fn get_by_placeholder(&self, text: String, options: Option<TextOptions>) -> Locator {
    let opts = options.as_ref().map_or_else(Default::default, Into::into);
    Locator::wrap(self.inner.get_by_placeholder(&text, opts))
  }

  #[napi]
  pub fn get_by_alt_text(&self, text: String, options: Option<TextOptions>) -> Locator {
    let opts = options.as_ref().map_or_else(Default::default, Into::into);
    Locator::wrap(self.inner.get_by_alt_text(&text, opts))
  }

  #[napi]
  pub fn get_by_title(&self, text: String, options: Option<TextOptions>) -> Locator {
    let opts = options.as_ref().map_or_else(Default::default, Into::into);
    Locator::wrap(self.inner.get_by_title(&text, opts))
  }

  #[napi]
  pub fn get_by_test_id(&self, test_id: String) -> Locator {
    Locator::wrap(self.inner.get_by_test_id(&test_id))
  }

  // ── Page-level actions ──────────────────────────────────────────────────

  #[napi]
  pub async fn click(&self, selector: String) -> Result<()> {
    self.inner.click(&selector).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn fill(&self, selector: String, value: String) -> Result<()> {
    self.inner.fill(&selector, &value).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn type_text(&self, selector: String, text: String) -> Result<()> {
    self.inner.type_text(&selector, &text).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn press(&self, selector: String, key: String) -> Result<()> {
    self.inner.press(&selector, &key).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn hover(&self, selector: String) -> Result<()> {
    self.inner.hover(&selector).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn select_option(&self, selector: String, value: String) -> Result<Vec<String>> {
    self.inner.select_option(&selector, &value).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn check(&self, selector: String) -> Result<()> {
    self.inner.check(&selector).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn uncheck(&self, selector: String) -> Result<()> {
    self.inner.uncheck(&selector).await.map_err(napi::Error::from_reason)
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
    self.inner.text_content(&selector).await.map_err(napi::Error::from_reason)
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
    self.inner.get_attribute(&selector, &name).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn input_value(&self, selector: String) -> Result<String> {
    self.inner.input_value(&selector).await.map_err(napi::Error::from_reason)
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
    self.inner.is_disabled(&selector).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn is_checked(&self, selector: String) -> Result<bool> {
    self.inner.is_checked(&selector).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn is_editable(&self, selector: String) -> Result<bool> {
    self.inner.is_editable(&selector).await.map_err(napi::Error::from_reason)
  }

  // ── Evaluation ──────────────────────────────────────────────────────────

  #[napi]
  pub async fn evaluate(&self, expression: String) -> Result<Option<serde_json::Value>> {
    self.inner.evaluate(&expression).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn evaluate_str(&self, expression: String) -> Result<String> {
    self.inner.evaluate_str(&expression).await.map_err(napi::Error::from_reason)
  }

  // ── Waiting ─────────────────────────────────────────────────────────────

  #[napi]
  pub async fn wait_for_selector(&self, selector: String, options: Option<WaitOptions>) -> Result<()> {
    let opts = options.as_ref().map_or_else(Default::default, Into::into);
    self.inner.wait_for_selector(&selector, opts).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn wait_for_url(&self, url_pattern: String) -> Result<()> {
    self.inner.wait_for_url(&url_pattern).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn wait_for_timeout(&self, ms: f64) {
    self.inner.wait_for_timeout(ms as u64).await;
  }

  #[napi]
  pub async fn wait_for_load_state(&self) -> Result<()> {
    self.inner.wait_for_load_state().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn wait_for_function(&self, expression: String, timeout_ms: Option<f64>) -> Result<serde_json::Value> {
    self.inner.wait_for_function(&expression, timeout_ms.map(|v| v as u64))
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn wait_for_navigation(&self, timeout_ms: Option<f64>) -> Result<()> {
    self.inner.wait_for_navigation(timeout_ms.map(|v| v as u64))
      .await
      .map_err(napi::Error::from_reason)
  }

  // ── Screenshots ─────────────────────────────────────────────────────────

  #[napi]
  pub async fn screenshot(&self, options: Option<ScreenshotOptions>) -> Result<Buffer> {
    let opts = options.as_ref().map_or_else(Default::default, Into::into);
    let bytes = self.inner.screenshot(opts).await.map_err(napi::Error::from_reason)?;
    Ok(bytes.into())
  }

  #[napi]
  pub async fn screenshot_element(&self, selector: String) -> Result<Buffer> {
    let bytes = self.inner.screenshot_element(&selector).await.map_err(napi::Error::from_reason)?;
    Ok(bytes.into())
  }

  /// Generate PDF from the page (headless Chrome only).
  #[napi]
  pub async fn pdf(&self, landscape: Option<bool>, print_background: Option<bool>) -> Result<Buffer> {
    let bytes = self.inner.pdf(
      landscape.unwrap_or(false),
      print_background.unwrap_or(false),
    ).await.map_err(napi::Error::from_reason)?;
    Ok(bytes.into())
  }

  // ── Viewport ────────────────────────────────────────────────────────────

  #[napi]
  pub async fn set_viewport_size(&self, width: i32, height: i32) -> Result<()> {
    self.inner.set_viewport_size(i64::from(width), i64::from(height))
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn set_viewport(&self, config: ViewportConfig) -> Result<()> {
    self.inner.set_viewport(&ferridriver::options::ViewportConfig::from(&config))
      .await
      .map_err(napi::Error::from_reason)
  }

  // ── Input devices ───────────────────────────────────────────────────────

  #[napi]
  pub async fn click_at(&self, x: f64, y: f64) -> Result<()> {
    self.inner.click_at(x, y).await.map_err(napi::Error::from_reason)
  }

  /// Click at coordinates with specific button and click count.
  /// button: "left", "right", "middle", "back", "forward"
  #[napi]
  pub async fn click_at_opts(&self, x: f64, y: f64, button: String, click_count: Option<i32>) -> Result<()> {
    self.inner.click_at_opts(x, y, &button, click_count.unwrap_or(1) as u32)
      .await.map_err(napi::Error::from_reason)
  }

  /// Move mouse to coordinates without clicking.
  #[napi]
  pub async fn move_mouse(&self, x: f64, y: f64) -> Result<()> {
    self.inner.move_mouse(x, y).await.map_err(napi::Error::from_reason)
  }

  /// Move mouse smoothly with bezier easing.
  #[napi]
  pub async fn move_mouse_smooth(&self, from_x: f64, from_y: f64, to_x: f64, to_y: f64, steps: Option<i32>) -> Result<()> {
    self.inner.move_mouse_smooth(from_x, from_y, to_x, to_y, steps.unwrap_or(10) as u32)
      .await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn drag_and_drop(&self, from_x: f64, from_y: f64, to_x: f64, to_y: f64) -> Result<()> {
    self.inner.drag_and_drop((from_x, from_y), (to_x, to_y))
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn type_str(&self, text: String) -> Result<()> {
    self.inner.type_str(&text).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn press_key(&self, key: String) -> Result<()> {
    self.inner.press_key(&key).await.map_err(napi::Error::from_reason)
  }

  // ── Emulation ───────────────────────────────────────────────────────────

  #[napi]
  pub async fn set_user_agent(&self, ua: String) -> Result<()> {
    self.inner.set_user_agent(&ua).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn set_geolocation(&self, lat: f64, lng: f64, accuracy: Option<f64>) -> Result<()> {
    self.inner.set_geolocation(lat, lng, accuracy.unwrap_or(1.0))
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn set_network_state(&self, offline: bool, latency: f64, download: f64, upload: f64) -> Result<()> {
    self.inner.set_network_state(offline, latency, download, upload)
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn emulate_media(
    &self,
    media_type: Option<String>,
    color_scheme: Option<String>,
    reduced_motion: Option<String>,
  ) -> Result<()> {
    self.inner.emulate_media(
      media_type.as_deref(),
      color_scheme.as_deref(),
      reduced_motion.as_deref(),
    ).await.map_err(napi::Error::from_reason)
  }

  // ── Cookies ─────────────────────────────────────────────────────────────

  #[napi]
  pub async fn cookies(&self) -> Result<Vec<CookieData>> {
    let cookies = self.inner.cookies().await.map_err(napi::Error::from_reason)?;
    Ok(cookies.iter().map(CookieData::from).collect())
  }

  #[napi]
  pub async fn set_cookie(&self, cookie: CookieData) -> Result<()> {
    self.inner.set_cookie(ferridriver::backend::CookieData::from(&cookie))
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn delete_cookie(&self, name: String, domain: Option<String>) -> Result<()> {
    self.inner.delete_cookie(&name, domain.as_deref())
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn clear_cookies(&self) -> Result<()> {
    self.inner.clear_cookies().await.map_err(napi::Error::from_reason)
  }

  // ── Focus / dispatch ────────────────────────────────────────────────────

  #[napi]
  pub async fn focus(&self, selector: String) -> Result<()> {
    self.inner.focus(&selector).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn dispatch_event(&self, selector: String, event_type: String) -> Result<()> {
    self.inner.dispatch_event(&selector, &event_type).await.map_err(napi::Error::from_reason)
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

  #[napi]
  pub async fn close(&self) -> Result<()> {
    self.inner.close().await.map_err(napi::Error::from_reason)
  }
}
