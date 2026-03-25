//! High-level Page API -- mirrors Playwright's Page interface.
//!
//! All interaction methods auto-wait for element actionability.
//! Locator methods are lazy (don't query DOM until action).

use crate::actions;
use crate::backend::{AnyPage, CookieData, ImageFormat, ScreenshotOpts};
use crate::locator::Locator;
use crate::options::*;
use crate::selectors;
use crate::snapshot;
use rustc_hash::FxHashMap as HashMap;

/// High-level page API, mirrors Playwright's Page interface.
#[derive(Clone)]
pub struct Page {
  pub(crate) inner: AnyPage,
  default_timeout: u64,
}

impl Page {
  /// Wrap a backend page.
  pub fn new(inner: AnyPage) -> Self {
    Self { inner, default_timeout: 30000 }
  }

  /// Access the underlying backend page (escape hatch).
  pub fn inner(&self) -> &AnyPage {
    &self.inner
  }

  /// Set the default timeout for all operations (milliseconds).
  pub fn set_default_timeout(&mut self, ms: u64) {
    self.default_timeout = ms;
  }

  // ── Navigation ──────────────────────────────────────────────────────────

  pub async fn goto(&self, url: &str) -> Result<(), String> {
    self.inner.goto(url).await
  }

  /// Navigate with empty-DOM health check and retry (for MCP server use).
  pub async fn goto_with_health_check(&self, url: &str) -> Result<(), String> {
    actions::navigate_with_health_check(&self.inner, url).await
  }

  pub async fn go_back(&self) -> Result<(), String> {
    self.inner.go_back().await
  }

  pub async fn go_forward(&self) -> Result<(), String> {
    self.inner.go_forward().await
  }

  pub async fn reload(&self) -> Result<(), String> {
    self.inner.reload().await
  }

  pub async fn url(&self) -> Result<String, String> {
    self.inner.url().await.map(|v| v.unwrap_or_default())
  }

  pub async fn title(&self) -> Result<String, String> {
    self.inner.title().await.map(|v| v.unwrap_or_default())
  }

  // ── Locators (lazy) ─────────────────────────────────────────────────────

  pub fn locator(&self, selector: &str) -> Locator {
    Locator { page: self.clone(), selector: selector.to_string() }
  }

  pub fn get_by_role(&self, role: &str, opts: RoleOptions) -> Locator {
    Locator { page: self.clone(), selector: crate::locator::build_role_selector(role, &opts) }
  }

  pub fn get_by_text(&self, text: &str, opts: TextOptions) -> Locator {
    let sel = if opts.exact == Some(true) { format!("text=\"{text}\"") } else { format!("text={text}") };
    Locator { page: self.clone(), selector: sel }
  }

  pub fn get_by_label(&self, text: &str, opts: TextOptions) -> Locator {
    let sel = if opts.exact == Some(true) { format!("label=\"{text}\"") } else { format!("label={text}") };
    Locator { page: self.clone(), selector: sel }
  }

  pub fn get_by_placeholder(&self, text: &str, opts: TextOptions) -> Locator {
    let sel = if opts.exact == Some(true) { format!("placeholder=\"{text}\"") } else { format!("placeholder={text}") };
    Locator { page: self.clone(), selector: sel }
  }

  pub fn get_by_alt_text(&self, text: &str, opts: TextOptions) -> Locator {
    let sel = if opts.exact == Some(true) { format!("alt=\"{text}\"") } else { format!("alt={text}") };
    Locator { page: self.clone(), selector: sel }
  }

  pub fn get_by_title(&self, text: &str, opts: TextOptions) -> Locator {
    let sel = if opts.exact == Some(true) { format!("title=\"{text}\"") } else { format!("title={text}") };
    Locator { page: self.clone(), selector: sel }
  }

  pub fn get_by_test_id(&self, test_id: &str) -> Locator {
    Locator { page: self.clone(), selector: format!("testid={test_id}") }
  }

  // ── Page-level actions (convenience, delegate to locator) ───────────────

  pub async fn click(&self, selector: &str) -> Result<(), String> {
    self.locator(selector).click().await
  }

  pub async fn fill(&self, selector: &str, value: &str) -> Result<(), String> {
    self.locator(selector).fill(value).await
  }

  pub async fn type_text(&self, selector: &str, text: &str) -> Result<(), String> {
    self.locator(selector).type_text(text).await
  }

  pub async fn press(&self, selector: &str, key: &str) -> Result<(), String> {
    self.locator(selector).press(key).await
  }

  pub async fn hover(&self, selector: &str) -> Result<(), String> {
    self.locator(selector).hover().await
  }

  pub async fn select_option(&self, selector: &str, value: &str) -> Result<Vec<String>, String> {
    self.locator(selector).select_option(value).await
  }

  pub async fn set_input_files(&self, selector: &str, paths: &[String]) -> Result<(), String> {
    self.locator(selector).set_input_files(paths).await
  }

  pub async fn check(&self, selector: &str) -> Result<(), String> {
    self.locator(selector).check().await
  }

  pub async fn uncheck(&self, selector: &str) -> Result<(), String> {
    self.locator(selector).uncheck().await
  }

  // ── Content ─────────────────────────────────────────────────────────────

  pub async fn content(&self) -> Result<String, String> {
    self.inner.content().await
  }

  pub async fn set_content(&self, html: &str) -> Result<(), String> {
    self.inner.set_content(html).await
  }

  pub async fn markdown(&self) -> Result<String, String> {
    actions::extract_markdown(&self.inner).await
  }

  pub async fn text_content(&self, selector: &str) -> Result<Option<String>, String> {
    self.locator(selector).text_content().await
  }

  pub async fn inner_text(&self, selector: &str) -> Result<String, String> {
    self.locator(selector).inner_text().await
  }

  pub async fn inner_html(&self, selector: &str) -> Result<String, String> {
    self.locator(selector).inner_html().await
  }

  pub async fn get_attribute(&self, selector: &str, name: &str) -> Result<Option<String>, String> {
    self.locator(selector).get_attribute(name).await
  }

  pub async fn input_value(&self, selector: &str) -> Result<String, String> {
    self.locator(selector).input_value().await
  }

  // ── State checks ────────────────────────────────────────────────────────

  pub async fn is_visible(&self, selector: &str) -> Result<bool, String> {
    self.locator(selector).is_visible().await
  }

  pub async fn is_hidden(&self, selector: &str) -> Result<bool, String> {
    self.locator(selector).is_hidden().await
  }

  pub async fn is_enabled(&self, selector: &str) -> Result<bool, String> {
    self.locator(selector).is_enabled().await
  }

  pub async fn is_disabled(&self, selector: &str) -> Result<bool, String> {
    self.locator(selector).is_disabled().await
  }

  pub async fn is_checked(&self, selector: &str) -> Result<bool, String> {
    self.locator(selector).is_checked().await
  }

  // ── Evaluation ──────────────────────────────────────────────────────────

  pub async fn evaluate(&self, expression: &str) -> Result<Option<serde_json::Value>, String> {
    self.inner.evaluate(expression).await
  }

  pub async fn evaluate_str(&self, expression: &str) -> Result<String, String> {
    self.inner.evaluate(expression).await.map(|v| {
      v.map(|val| {
        if let Some(s) = val.as_str() { s.to_string() } else { val.to_string() }
      }).unwrap_or_default()
    })
  }

  // ── Waiting ─────────────────────────────────────────────────────────────

  pub async fn wait_for_selector(&self, selector: &str, opts: WaitOptions) -> Result<(), String> {
    self.locator(selector).wait_for(opts).await
  }

  pub async fn wait_for_url(&self, url_pattern: &str) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(self.default_timeout);
    loop {
      if tokio::time::Instant::now() >= deadline {
        return Err(format!("Timeout waiting for URL matching '{url_pattern}'"));
      }
      let current = self.url().await.unwrap_or_default();
      if current.contains(url_pattern) {
        return Ok(());
      }
      tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
  }

  pub async fn wait_for_timeout(&self, ms: u64) {
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
  }

  pub async fn wait_for_load_state(&self) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(self.default_timeout);
    loop {
      if tokio::time::Instant::now() >= deadline {
        return Err("Timeout waiting for load state".into());
      }
      if let Ok(Some(v)) = self.inner.evaluate("document.readyState").await {
        if v.as_str() == Some("complete") {
          return Ok(());
        }
      }
      tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
  }

  // ── Screenshots ─────────────────────────────────────────────────────────

  pub async fn screenshot(&self, opts: ScreenshotOptions) -> Result<Vec<u8>, String> {
    let format = match opts.format.as_deref() {
      Some("jpeg") | Some("jpg") => ImageFormat::Jpeg,
      Some("webp") => ImageFormat::Webp,
      _ => ImageFormat::Png,
    };
    self.inner.screenshot(ScreenshotOpts {
      format,
      quality: opts.quality,
      full_page: opts.full_page.unwrap_or(false),
    }).await
  }

  pub async fn screenshot_element(&self, selector: &str) -> Result<Vec<u8>, String> {
    self.locator(selector).screenshot().await
  }

  // ── PDF ─────────────────────────────────────────────────────────────────

  /// Generate PDF from the page (headless Chrome only).
  pub async fn pdf(&self, landscape: bool, print_background: bool) -> Result<Vec<u8>, String> {
    self.inner.pdf(landscape, print_background).await
  }

  // ── Accessibility ───────────────────────────────────────────────────────

  pub async fn accessibility_snapshot(&self) -> Result<(String, HashMap<String, i64>), String> {
    snapshot::page_context_with_snapshot(&self.inner).await
  }

  // ── Viewport ────────────────────────────────────────────────────────────

  pub async fn set_viewport_size(&self, width: i64, height: i64) -> Result<(), String> {
    self.inner.emulate_viewport(&crate::options::ViewportConfig {
      width, height, ..Default::default()
    }).await
  }

  // ── Input devices ───────────────────────────────────────────────────────

  pub async fn click_at(&self, x: f64, y: f64) -> Result<(), String> {
    self.inner.click_at(x, y).await
  }

  pub async fn click_at_opts(&self, x: f64, y: f64, button: &str, click_count: u32) -> Result<(), String> {
    self.inner.click_at_opts(x, y, button, click_count).await
  }

  pub async fn move_mouse(&self, x: f64, y: f64) -> Result<(), String> {
    self.inner.move_mouse(x, y).await
  }

  pub async fn move_mouse_smooth(&self, from_x: f64, from_y: f64, to_x: f64, to_y: f64, steps: u32) -> Result<(), String> {
    self.inner.move_mouse_smooth(from_x, from_y, to_x, to_y, steps).await
  }

  pub async fn drag_and_drop(&self, from: (f64, f64), to: (f64, f64)) -> Result<(), String> {
    self.inner.click_and_drag(from, to).await
  }

  /// Type text character-by-character into the focused element.
  pub async fn type_str(&self, text: &str) -> Result<(), String> {
    self.inner.type_str(text).await
  }

  /// Press a key or combo (e.g., "Enter", "Control+a").
  pub async fn press_key(&self, key: &str) -> Result<(), String> {
    self.inner.press_key(key).await
  }

  /// Find element by CSS selector (raw backend access).
  pub async fn find_element(&self, selector: &str) -> Result<crate::backend::AnyElement, String> {
    self.inner.find_element(selector).await
  }

  // ── Emulation ───────────────────────────────────────────────────────────

  /// Set viewport with full configuration (matches Playwright's viewport options).
  pub async fn set_viewport(&self, config: &crate::options::ViewportConfig) -> Result<(), String> {
    self.inner.emulate_viewport(config).await
  }

  pub async fn set_user_agent(&self, ua: &str) -> Result<(), String> {
    self.inner.set_user_agent(ua).await
  }

  pub async fn set_geolocation(&self, lat: f64, lng: f64, accuracy: f64) -> Result<(), String> {
    self.inner.set_geolocation(lat, lng, accuracy).await
  }

  pub async fn set_network_state(&self, offline: bool, latency: f64, download: f64, upload: f64) -> Result<(), String> {
    self.inner.set_network_state(offline, latency, download, upload).await
  }

  /// Set the browser locale (affects navigator.language and Intl APIs).
  pub async fn set_locale(&self, locale: &str) -> Result<(), String> {
    self.inner.set_locale(locale).await
  }

  /// Set the browser timezone (affects Date and Intl.DateTimeFormat).
  pub async fn set_timezone(&self, timezone_id: &str) -> Result<(), String> {
    self.inner.set_timezone(timezone_id).await
  }

  /// Emulate media features (color scheme, reduced motion, media type, etc.).
  /// Matches Playwright's page.emulateMedia().
  pub async fn emulate_media(&self, opts: &crate::options::EmulateMediaOptions) -> Result<(), String> {
    self.inner.emulate_media(opts).await
  }

  /// Enable or disable JavaScript execution.
  pub async fn set_javascript_enabled(&self, enabled: bool) -> Result<(), String> {
    self.inner.set_javascript_enabled(enabled).await
  }

  /// Set extra HTTP headers that will be sent with every request.
  pub async fn set_extra_http_headers(&self, headers: &rustc_hash::FxHashMap<String, String>) -> Result<(), String> {
    self.inner.set_extra_http_headers(headers).await
  }

  /// Grant browser permissions (geolocation, notifications, camera, etc.).
  pub async fn grant_permissions(&self, permissions: &[String], origin: Option<&str>) -> Result<(), String> {
    self.inner.grant_permissions(permissions, origin).await
  }

  /// Reset all granted permissions.
  pub async fn reset_permissions(&self) -> Result<(), String> {
    self.inner.reset_permissions().await
  }

  /// Emulate focus state (page always appears focused even when not).
  pub async fn set_focus_emulation_enabled(&self, enabled: bool) -> Result<(), String> {
    self.inner.set_focus_emulation_enabled(enabled).await
  }

  // ── Tracing ─────────────────────────────────────────────────────────────

  pub async fn start_tracing(&self) -> Result<(), String> {
    self.inner.start_tracing().await
  }

  pub async fn stop_tracing(&self) -> Result<(), String> {
    self.inner.stop_tracing().await
  }

  pub async fn metrics(&self) -> Result<Vec<crate::backend::MetricData>, String> {
    self.inner.metrics().await
  }

  // ── Cookie delete ───────────────────────────────────────────────────────

  /// Delete cookie(s) by name and optional domain.
  ///
  /// Uses the Playwright approach: get all cookies, clear all, re-add
  /// non-matching ones. This avoids CDP `Network.deleteCookies` edge cases
  /// with exact domain matching and secure cookie handling.
  pub async fn delete_cookie(&self, name: &str, domain: Option<&str>) -> Result<(), String> {
    let cookies = self.inner.get_cookies().await?;
    self.inner.clear_cookies().await?;
    for cookie in cookies {
      let name_matches = cookie.name == name;
      let domain_matches = domain.map_or(true, |d| cookie.domain == d);
      if !(name_matches && domain_matches) {
        self.inner.set_cookie(cookie).await?;
      }
    }
    Ok(())
  }

  // ── Cookies ─────────────────────────────────────────────────────────────

  pub async fn cookies(&self) -> Result<Vec<CookieData>, String> {
    self.inner.get_cookies().await
  }

  pub async fn set_cookie(&self, cookie: CookieData) -> Result<(), String> {
    self.inner.set_cookie(cookie).await
  }

  pub async fn clear_cookies(&self) -> Result<(), String> {
    self.inner.clear_cookies().await
  }

  // ── Focus / dispatch ─────────────────────────────────────────────────

  /// Focus an element by selector.
  pub async fn focus(&self, selector: &str) -> Result<(), String> {
    self.locator(selector).focus().await
  }

  /// Dispatch an event on an element by selector.
  pub async fn dispatch_event(&self, selector: &str, event_type: &str) -> Result<(), String> {
    self.locator(selector).dispatch_event(event_type).await
  }

  /// Check if an element is editable (not disabled, not readonly).
  pub async fn is_editable(&self, selector: &str) -> Result<bool, String> {
    self.locator(selector).is_editable().await
  }

  // ── Waiting (additional) ────────────────────────────────────────────────

  /// Wait for a JS function/expression to return a truthy value.
  pub async fn wait_for_function(&self, expression: &str, timeout_ms: Option<u64>) -> Result<serde_json::Value, String> {
    let timeout = timeout_ms.unwrap_or(self.default_timeout);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout);
    loop {
      if tokio::time::Instant::now() >= deadline {
        return Err(format!("Timeout waiting for function: {expression}"));
      }
      if let Ok(Some(val)) = self.inner.evaluate(expression).await {
        let truthy = match &val {
          serde_json::Value::Bool(b) => *b,
          serde_json::Value::Number(n) => n.as_f64().unwrap_or(0.0) != 0.0,
          serde_json::Value::String(s) => !s.is_empty(),
          serde_json::Value::Null => false,
          _ => true,
        };
        if truthy {
          return Ok(val);
        }
      }
      tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
  }

  /// Wait for the page to navigate to a URL matching the pattern.
  pub async fn wait_for_navigation(&self, timeout_ms: Option<u64>) -> Result<(), String> {
    let timeout = timeout_ms.unwrap_or(self.default_timeout);
    let current = self.url().await.unwrap_or_default();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout);
    loop {
      if tokio::time::Instant::now() >= deadline {
        return Err("Timeout waiting for navigation".into());
      }
      let now = self.url().await.unwrap_or_default();
      if now != current {
        return Ok(());
      }
      tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
  }

  // ── Mouse (low-level) ──────────────────────────────────────────────────

  /// Scroll via mouse wheel event.
  pub async fn mouse_wheel(&self, delta_x: f64, delta_y: f64) -> Result<(), String> {
    self.inner.mouse_wheel(delta_x, delta_y).await
  }

  /// Mouse button down (without up). For custom drag sequences.
  pub async fn mouse_down(&self, x: f64, y: f64, button: &str) -> Result<(), String> {
    self.inner.mouse_down(x, y, button).await
  }

  /// Mouse button up.
  pub async fn mouse_up(&self, x: f64, y: f64, button: &str) -> Result<(), String> {
    self.inner.mouse_up(x, y, button).await
  }

  /// Bring this page to front (focus).
  pub async fn bring_to_front(&self) -> Result<(), String> {
    let _ = self.inner.evaluate("window.focus()").await;
    Ok(())
  }

  // ── ARIA snapshot alias ─────────────────────────────────────────────────

  /// Alias for accessibility_snapshot (Playwright naming).
  pub async fn aria_snapshot(&self) -> Result<(String, HashMap<String, i64>), String> {
    self.accessibility_snapshot().await
  }

  // ── Lifecycle ───────────────────────────────────────────────────────────

  pub async fn close(&self) -> Result<(), String> {
    Ok(())
  }
}

impl std::fmt::Debug for Page {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Page").finish()
  }
}
