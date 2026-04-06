//! Page class -- NAPI binding for `ferridriver::Page`.

use crate::locator::Locator;
use crate::types::{
  GotoOptions, MetricData, ResponseData, RoleOptions, ScreenshotOptions, TextOptions, ViewportConfig,
  WaitOptions,
};
use napi::Result;
use napi::bindgen_prelude::Buffer;
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

  pub(crate) fn inner_ref(&self) -> &ferridriver::Page {
    &self.inner
  }
}

#[napi]
impl Page {
  /// Set the default timeout for all operations (milliseconds).
  #[napi]
  pub fn set_default_timeout(&mut self, ms: f64) {
    self.inner.set_default_timeout(crate::types::f64_to_u64(ms));
  }

  // ── Navigation ──────────────────────────────────────────────────────────

  #[napi]
  pub async fn goto(&self, url: String, options: Option<GotoOptions>) -> Result<()> {
    let opts = options.as_ref().map(ferridriver::options::GotoOptions::from);
    self.inner.goto(&url, opts).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn go_back(&self, options: Option<GotoOptions>) -> Result<()> {
    let opts = options.as_ref().map(ferridriver::options::GotoOptions::from);
    self.inner.go_back(opts).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn go_forward(&self, options: Option<GotoOptions>) -> Result<()> {
    let opts = options.as_ref().map(ferridriver::options::GotoOptions::from);
    self.inner.go_forward(opts).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn reload(&self, options: Option<GotoOptions>) -> Result<()> {
    let opts = options.as_ref().map(ferridriver::options::GotoOptions::from);
    self.inner.reload(opts).await.map_err(napi::Error::from_reason)
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
    let opts: ferridriver::options::RoleOptions = options.as_ref().map_or_else(Default::default, Into::into);
    Locator::wrap(self.inner.get_by_role(&role, &opts))
  }

  #[napi]
  pub fn get_by_text(&self, text: String, options: Option<TextOptions>) -> Locator {
    let opts: ferridriver::options::TextOptions = options.as_ref().map_or_else(Default::default, Into::into);
    Locator::wrap(self.inner.get_by_text(&text, &opts))
  }

  #[napi]
  pub fn get_by_label(&self, text: String, options: Option<TextOptions>) -> Locator {
    let opts: ferridriver::options::TextOptions = options.as_ref().map_or_else(Default::default, Into::into);
    Locator::wrap(self.inner.get_by_label(&text, &opts))
  }

  #[napi]
  pub fn get_by_placeholder(&self, text: String, options: Option<TextOptions>) -> Locator {
    let opts: ferridriver::options::TextOptions = options.as_ref().map_or_else(Default::default, Into::into);
    Locator::wrap(self.inner.get_by_placeholder(&text, &opts))
  }

  #[napi]
  pub fn get_by_alt_text(&self, text: String, options: Option<TextOptions>) -> Locator {
    let opts: ferridriver::options::TextOptions = options.as_ref().map_or_else(Default::default, Into::into);
    Locator::wrap(self.inner.get_by_alt_text(&text, &opts))
  }

  #[napi]
  pub fn get_by_title(&self, text: String, options: Option<TextOptions>) -> Locator {
    let opts: ferridriver::options::TextOptions = options.as_ref().map_or_else(Default::default, Into::into);
    Locator::wrap(self.inner.get_by_title(&text, &opts))
  }

  #[napi]
  pub fn get_by_test_id(&self, test_id: String) -> Locator {
    Locator::wrap(self.inner.get_by_test_id(&test_id))
  }

  // ── Frames ─────────────────────────────────────────────────────────────

  /// Get the main frame of this page.
  #[napi]
  pub async fn main_frame(&self) -> Result<crate::frame::Frame> {
    self
      .inner
      .main_frame()
      .await
      .map(crate::frame::Frame::wrap)
      .map_err(napi::Error::from_reason)
  }

  /// Get all frames in the page (main frame + all iframes).
  #[napi]
  pub async fn frames(&self) -> Result<Vec<crate::frame::Frame>> {
    self
      .inner
      .frames()
      .await
      .map(|f| f.into_iter().map(crate::frame::Frame::wrap).collect())
      .map_err(napi::Error::from_reason)
  }

  /// Find a frame by name or URL.
  #[napi]
  pub async fn frame(&self, name_or_url: String) -> Result<Option<crate::frame::Frame>> {
    self
      .inner
      .frame(&name_or_url)
      .await
      .map(|opt| opt.map(crate::frame::Frame::wrap))
      .map_err(napi::Error::from_reason)
  }

  // ── Events (Playwright-compatible on/once/waitForEvent) ─────────────

  /// Register an event listener. Returns a listener ID for removal with `off()`.
  ///
  /// Supported events: 'console', 'response', 'request', 'dialog', 'download',
  /// 'frameattached', 'framedetached', 'framenavigated',
  /// 'load', 'domcontentloaded', 'close', 'pageerror'
  #[napi(
    ts_args_type = "event: 'console' | 'response' | 'request' | 'dialog' | 'download' | 'frameattached' | 'framedetached' | 'framenavigated' | 'load' | 'domcontentloaded' | 'close' | 'pageerror', listener: (data: { type: string; text: string } | ResponseData | { type: string; message: string; defaultValue: string } | Record<string, any>) => void"
  )]
  pub fn on(&self, event: String, listener: napi::bindgen_prelude::Function<'_, serde_json::Value, ()>) -> Result<f64> {
    let tsfn = listener
      .build_threadsafe_function()
      .callee_handled::<false>()
      .weak::<true>()
      .max_queue_size::<0>()
      .build()?;
    let event_name = event.clone();
    let callback: ferridriver::events::EventCallback = std::sync::Arc::new(move |ev| {
      if let Some(data) = event_to_js(&event_name, &ev) {
        tsfn.call(data, napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking);
      }
    });
    let id = self.inner.on(&event, callback);
    #[allow(clippy::cast_precision_loss)]
    Ok(id.0 as f64)
  }

  /// Register a one-time event listener. Auto-removed after first match.
  #[napi(
    ts_args_type = "event: 'console' | 'response' | 'request' | 'dialog' | 'download' | 'frameattached' | 'framedetached' | 'framenavigated' | 'load' | 'domcontentloaded' | 'close' | 'pageerror', listener: (data: { type: string; text: string } | ResponseData | { type: string; message: string; defaultValue: string } | Record<string, any>) => void"
  )]
  pub fn once(
    &self,
    event: String,
    listener: napi::bindgen_prelude::Function<'_, serde_json::Value, ()>,
  ) -> Result<f64> {
    let tsfn = listener
      .build_threadsafe_function()
      .callee_handled::<false>()
      .weak::<true>()
      .max_queue_size::<0>()
      .build()?;
    let event_name = event.clone();
    let callback: ferridriver::events::EventCallback = std::sync::Arc::new(move |ev| {
      if let Some(data) = event_to_js(&event_name, &ev) {
        tsfn.call(data, napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking);
      }
    });
    let id = self.inner.once(&event, callback);
    // ListenerId is a sequential counter; it will never exceed 2^53 (f64 mantissa precision).
    #[allow(clippy::cast_precision_loss)]
    Ok(id.0 as f64)
  }

  /// Remove an event listener by ID (returned from `on()` or `once()`).
  #[napi]
  pub fn off(&self, listener_id: f64) {
    // listener_id originates from on()/once() which returns a u64 counter
    // round-tripped through f64; the value is always non-negative and integral.
    self
      .inner
      .off(ferridriver::events::ListenerId(crate::types::f64_to_u64(listener_id)));
  }

  /// Remove all event listeners from this page.
  #[napi]
  pub fn remove_all_listeners(&self) {
    self.inner.remove_all_listeners();
  }

  /// Wait for a specific event. Playwright API: `page.waitForEvent(event)`.
  #[napi(
    ts_args_type = "event: 'console' | 'response' | 'request' | 'dialog' | 'load' | 'domcontentloaded' | 'close' | 'pageerror', timeoutMs?: number",
    ts_return_type = "Promise<{ type: string; text: string } | ResponseData | { type: string; message: string; defaultValue: string } | Record<string, any>>"
  )]
  pub async fn wait_for_event(&self, event: String, timeout_ms: Option<f64>) -> Result<serde_json::Value> {
    let timeout = crate::types::f64_to_u64(timeout_ms.unwrap_or(30000.0));
    let ev = self
      .inner
      .events()
      .wait_for_event(&event, timeout)
      .await
      .map_err(napi::Error::from_reason)?;
    Ok(page_event_to_value(&ev))
  }

  /// Wait for a network response matching a URL pattern.
  /// Playwright API: `page.waitForResponse(urlOrPredicate)`.
  #[napi]
  pub async fn wait_for_response(&self, url_pattern: String, timeout_ms: Option<f64>) -> Result<ResponseData> {
    let r = self
      .inner
      .wait_for_response(&url_pattern, timeout_ms.map(crate::types::f64_to_u64))
      .await
      .map_err(napi::Error::from_reason)?;
    Ok(ResponseData {
      url: r.url,
      status: i32::try_from(r.status).unwrap_or(i32::MAX),
      status_text: r.status_text,
    })
  }

  // ── Page-level actions ──────────────────────────────────────────────────

  #[napi]
  pub async fn click(&self, selector: String) -> Result<()> {
    self.inner.click(&selector).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn fill(&self, selector: String, value: String) -> Result<()> {
    self
      .inner
      .fill(&selector, &value)
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn type_text(&self, selector: String, text: String) -> Result<()> {
    self
      .inner
      .type_text(&selector, &text)
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn press(&self, selector: String, key: String) -> Result<()> {
    self
      .inner
      .press(&selector, &key)
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn hover(&self, selector: String) -> Result<()> {
    self.inner.hover(&selector).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn select_option(&self, selector: String, value: String) -> Result<Vec<String>> {
    self
      .inner
      .select_option(&selector, &value)
      .await
      .map_err(napi::Error::from_reason)
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
    self
      .inner
      .text_content(&selector)
      .await
      .map_err(napi::Error::from_reason)
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
    self
      .inner
      .get_attribute(&selector, &name)
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn input_value(&self, selector: String) -> Result<String> {
    self
      .inner
      .input_value(&selector)
      .await
      .map_err(napi::Error::from_reason)
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
    self
      .inner
      .is_disabled(&selector)
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn is_checked(&self, selector: String) -> Result<bool> {
    self.inner.is_checked(&selector).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn is_editable(&self, selector: String) -> Result<bool> {
    self
      .inner
      .is_editable(&selector)
      .await
      .map_err(napi::Error::from_reason)
  }

  // ── Evaluation ──────────────────────────────────────────────────────────

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

  // ── Waiting ─────────────────────────────────────────────────────────────

  #[napi]
  pub async fn wait_for_selector(&self, selector: String, options: Option<WaitOptions>) -> Result<()> {
    let opts: ferridriver::options::WaitOptions = options.as_ref().map_or_else(Default::default, Into::into);
    self
      .inner
      .wait_for_selector(&selector, opts)
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn wait_for_url(&self, url_pattern: String) -> Result<()> {
    self
      .inner
      .wait_for_url(&url_pattern)
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn wait_for_timeout(&self, ms: f64) {
    self.inner.wait_for_timeout(crate::types::f64_to_u64(ms)).await;
  }

  #[napi]
  pub async fn wait_for_load_state(&self, state: Option<String>) -> Result<()> {
    self
      .inner
      .wait_for_load_state(state.as_deref())
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn wait_for_function(&self, expression: String, timeout_ms: Option<f64>) -> Result<serde_json::Value> {
    self
      .inner
      .wait_for_function(&expression, timeout_ms.map(crate::types::f64_to_u64))
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn wait_for_navigation(&self, timeout_ms: Option<f64>) -> Result<()> {
    self
      .inner
      .wait_for_navigation(timeout_ms.map(crate::types::f64_to_u64))
      .await
      .map_err(napi::Error::from_reason)
  }

  // ── Screenshots ─────────────────────────────────────────────────────────

  #[napi]
  pub async fn screenshot(&self, options: Option<ScreenshotOptions>) -> Result<Buffer> {
    let opts: ferridriver::options::ScreenshotOptions = options.as_ref().map_or_else(Default::default, Into::into);
    let bytes = self.inner.screenshot(opts).await.map_err(napi::Error::from_reason)?;
    Ok(bytes.into())
  }

  #[napi]
  pub async fn screenshot_element(&self, selector: String) -> Result<Buffer> {
    let bytes = self
      .inner
      .screenshot_element(&selector)
      .await
      .map_err(napi::Error::from_reason)?;
    Ok(bytes.into())
  }

  /// Generate PDF from the page (headless Chrome only).
  #[napi]
  pub async fn pdf(&self, landscape: Option<bool>, print_background: Option<bool>) -> Result<Buffer> {
    let bytes = self
      .inner
      .pdf(landscape.unwrap_or(false), print_background.unwrap_or(false))
      .await
      .map_err(napi::Error::from_reason)?;
    Ok(bytes.into())
  }

  // ── Viewport ────────────────────────────────────────────────────────────

  #[napi]
  pub async fn set_viewport_size(&self, width: i32, height: i32) -> Result<()> {
    self
      .inner
      .set_viewport_size(i64::from(width), i64::from(height))
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn set_viewport(&self, config: ViewportConfig) -> Result<()> {
    self
      .inner
      .set_viewport(&ferridriver::options::ViewportConfig::from(&config))
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
    let count = u32::try_from(click_count.unwrap_or(1))
      .map_err(|_| napi::Error::from_reason("click_count must be non-negative"))?;
    self
      .inner
      .click_at_opts(x, y, &button, count)
      .await
      .map_err(napi::Error::from_reason)
  }

  /// Move mouse to coordinates without clicking.
  #[napi]
  pub async fn move_mouse(&self, x: f64, y: f64) -> Result<()> {
    self.inner.move_mouse(x, y).await.map_err(napi::Error::from_reason)
  }

  /// Move mouse smoothly with bezier easing.
  #[napi]
  pub async fn move_mouse_smooth(
    &self,
    from_x: f64,
    from_y: f64,
    to_x: f64,
    to_y: f64,
    steps: Option<i32>,
  ) -> Result<()> {
    let step_count =
      u32::try_from(steps.unwrap_or(10)).map_err(|_| napi::Error::from_reason("steps must be non-negative"))?;
    self
      .inner
      .move_mouse_smooth(from_x, from_y, to_x, to_y, step_count)
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn drag_and_drop(&self, from_x: f64, from_y: f64, to_x: f64, to_y: f64) -> Result<()> {
    self
      .inner
      .drag_and_drop((from_x, from_y), (to_x, to_y))
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
    self
      .inner
      .set_geolocation(lat, lng, accuracy.unwrap_or(1.0))
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn set_network_state(&self, offline: bool, latency: f64, download: f64, upload: f64) -> Result<()> {
    self
      .inner
      .set_network_state(offline, latency, download, upload)
      .await
      .map_err(napi::Error::from_reason)
  }

  /// Set the browser locale (navigator.language, Intl APIs).
  #[napi]
  pub async fn set_locale(&self, locale: String) -> Result<()> {
    self.inner.set_locale(&locale).await.map_err(napi::Error::from_reason)
  }

  /// Set the browser timezone (Date, Intl.DateTimeFormat).
  #[napi]
  pub async fn set_timezone(&self, timezone_id: String) -> Result<()> {
    self
      .inner
      .set_timezone(&timezone_id)
      .await
      .map_err(napi::Error::from_reason)
  }

  /// Emulate media features (color scheme, reduced motion, media type, etc.).
  #[napi]
  pub async fn emulate_media(
    &self,
    media_type: Option<String>,
    color_scheme: Option<String>,
    reduced_motion: Option<String>,
    forced_colors: Option<String>,
    contrast: Option<String>,
  ) -> Result<()> {
    self
      .inner
      .emulate_media(&ferridriver::options::EmulateMediaOptions {
        media: media_type,
        color_scheme,
        reduced_motion,
        forced_colors,
        contrast,
      })
      .await
      .map_err(napi::Error::from_reason)
  }

  /// Enable or disable JavaScript execution.
  #[napi]
  pub async fn set_javascript_enabled(&self, enabled: bool) -> Result<()> {
    self
      .inner
      .set_javascript_enabled(enabled)
      .await
      .map_err(napi::Error::from_reason)
  }

  // ── Focus / dispatch ────────────────────────────────────────────────────

  #[napi]
  pub async fn focus(&self, selector: String) -> Result<()> {
    self.inner.focus(&selector).await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn dispatch_event(&self, selector: String, event_type: String) -> Result<()> {
    self
      .inner
      .dispatch_event(&selector, &event_type)
      .await
      .map_err(napi::Error::from_reason)
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

  #[napi]
  pub fn is_closed(&self) -> bool {
    self.inner.is_closed()
  }

  // ── Missing methods (batch add) ────────────────────────────────────────

  #[napi]
  pub async fn viewport_size(&self) -> Result<Vec<i32>> {
    let (w, h) = self.inner.viewport_size().await.map_err(napi::Error::from_reason)?;
    let w32 = i32::try_from(w).map_err(|_| napi::Error::from_reason(format!("viewport width {w} exceeds i32::MAX")))?;
    let h32 =
      i32::try_from(h).map_err(|_| napi::Error::from_reason(format!("viewport height {h} exceeds i32::MAX")))?;
    Ok(vec![w32, h32])
  }

  #[napi]
  pub async fn storage_state(&self) -> Result<serde_json::Value> {
    self.inner.storage_state().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn set_storage_state(&self, state: serde_json::Value) -> Result<()> {
    self
      .inner
      .set_storage_state(&state)
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn add_init_script(&self, source: String) -> Result<String> {
    self
      .inner
      .add_init_script(&source)
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn remove_init_script(&self, identifier: String) -> Result<()> {
    self
      .inner
      .remove_init_script(&identifier)
      .await
      .map_err(napi::Error::from_reason)
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
  pub async fn set_extra_http_headers(&self, headers: std::collections::HashMap<String, String>) -> Result<()> {
    let mut fx = rustc_hash::FxHashMap::default();
    for (k, v) in headers {
      fx.insert(k, v);
    }
    self
      .inner
      .set_extra_http_headers(&fx)
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn grant_permissions(&self, permissions: Vec<String>, origin: Option<String>) -> Result<()> {
    self
      .inner
      .grant_permissions(&permissions, origin.as_deref())
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn reset_permissions(&self) -> Result<()> {
    self.inner.reset_permissions().await.map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn set_focus_emulation_enabled(&self, enabled: bool) -> Result<()> {
    self
      .inner
      .set_focus_emulation_enabled(enabled)
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn mouse_wheel(&self, delta_x: f64, delta_y: f64) -> Result<()> {
    self
      .inner
      .mouse_wheel(delta_x, delta_y)
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn mouse_down(&self, x: f64, y: f64, button: Option<String>) -> Result<()> {
    self
      .inner
      .mouse_down(x, y, button.as_deref().unwrap_or("left"))
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn mouse_up(&self, x: f64, y: f64, button: Option<String>) -> Result<()> {
    self
      .inner
      .mouse_up(x, y, button.as_deref().unwrap_or("left"))
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn set_input_files(&self, selector: String, paths: Vec<String>) -> Result<()> {
    self
      .inner
      .set_input_files(&selector, &paths)
      .await
      .map_err(napi::Error::from_reason)
  }

  #[napi]
  pub async fn wait_for_request(&self, url_pattern: String, timeout_ms: Option<f64>) -> Result<serde_json::Value> {
    let req = self
      .inner
      .wait_for_request(&url_pattern, timeout_ms.map(crate::types::f64_to_u64))
      .await
      .map_err(napi::Error::from_reason)?;
    Ok(serde_json::json!({"url": req.url, "method": req.method, "resourceType": req.resource_type}))
  }

  #[napi]
  pub async fn wait_for_download(
    &self,
    url_pattern: Option<String>,
    timeout_ms: Option<f64>,
  ) -> Result<serde_json::Value> {
    let dl = self
      .inner
      .wait_for_download(url_pattern.as_deref(), timeout_ms.map(crate::types::f64_to_u64))
      .await
      .map_err(napi::Error::from_reason)?;
    Ok(serde_json::json!({"guid": dl.guid, "url": dl.url, "suggestedFilename": dl.suggested_filename}))
  }

  #[napi(getter)]
  pub fn default_timeout(&self) -> f64 {
    // Default timeout is a millisecond value; practical values never exceed 2^53 (f64 mantissa).
    #[allow(clippy::cast_precision_loss)]
    {
      self.inner.default_timeout() as f64
    }
  }
}

// ── Event conversion helpers ─────────────────────────────────────────────

use ferridriver::events::PageEvent;

/// Convert a `PageEvent` to a JS-friendly `serde_json::Value`, filtered by event name.
/// Returns None if the event doesn't match the requested name.
#[allow(clippy::match_same_arms)] // arms differ by event name filter, bodies intentionally identical
fn event_to_js(event_name: &str, event: &PageEvent) -> Option<serde_json::Value> {
  match (event_name, event) {
    ("console", PageEvent::Console(msg)) => Some(serde_json::json!({
        "type": msg.level,
        "text": msg.text,
    })),
    ("response", PageEvent::Response(r)) => Some(serde_json::json!({
        "url": r.url,
        "status": r.status,
        "statusText": r.status_text,
        "mimeType": r.mime_type,
        "headers": r.headers,
    })),
    ("request", PageEvent::Request(r)) => Some(serde_json::json!({
        "url": r.url,
        "method": r.method,
        "resourceType": r.resource_type,
        "headers": r.headers,
        "postData": r.post_data,
    })),
    ("dialog", PageEvent::Dialog(d)) => Some(serde_json::json!({
        "type": d.dialog_type,
        "message": d.message,
        "defaultValue": d.default_value,
    })),
    ("frameattached", PageEvent::FrameAttached(f)) => Some(serde_json::json!({
        "frameId": f.frame_id,
        "name": f.name,
        "url": f.url,
    })),
    ("framedetached", PageEvent::FrameDetached { frame_id }) => Some(serde_json::json!({
        "frameId": frame_id,
    })),
    ("framenavigated", PageEvent::FrameNavigated(f)) => Some(serde_json::json!({
        "frameId": f.frame_id,
        "name": f.name,
        "url": f.url,
    })),
    ("load", PageEvent::Load) => Some(serde_json::json!({})),
    ("domcontentloaded", PageEvent::DomContentLoaded) => Some(serde_json::json!({})),
    ("close", PageEvent::Close) => Some(serde_json::json!({})),
    ("pageerror", PageEvent::PageError(msg)) => Some(serde_json::json!({
        "message": msg,
    })),
    ("download", PageEvent::Download(d)) => Some(serde_json::json!({
        "guid": d.guid,
        "url": d.url,
        "suggestedFilename": d.suggested_filename,
    })),
    _ => None,
  }
}

/// Convert any `PageEvent` to a JS value (for waitForEvent).
fn page_event_to_value(event: &PageEvent) -> serde_json::Value {
  match event {
    PageEvent::Console(msg) => serde_json::json!({"type": msg.level, "text": msg.text}),
    PageEvent::Response(r) => {
      serde_json::json!({"url": r.url, "status": r.status, "statusText": r.status_text, "mimeType": r.mime_type, "headers": r.headers})
    },
    PageEvent::Request(r) => {
      serde_json::json!({"url": r.url, "method": r.method, "resourceType": r.resource_type, "headers": r.headers, "postData": r.post_data})
    },
    PageEvent::Dialog(d) => {
      serde_json::json!({"type": d.dialog_type, "message": d.message, "defaultValue": d.default_value})
    },
    PageEvent::FrameAttached(f) | PageEvent::FrameNavigated(f) => {
      serde_json::json!({"frameId": f.frame_id, "name": f.name, "url": f.url})
    },
    PageEvent::FrameDetached { frame_id } => serde_json::json!({"frameId": frame_id}),
    PageEvent::Load => serde_json::json!({"type": "load"}),
    PageEvent::DomContentLoaded => serde_json::json!({"type": "domcontentloaded"}),
    PageEvent::Close => serde_json::json!({"type": "close"}),
    PageEvent::PageError(msg) => serde_json::json!({"message": msg}),
    PageEvent::Download(d) => {
      serde_json::json!({"guid": d.guid, "url": d.url, "suggestedFilename": d.suggested_filename})
    },
  }
}
