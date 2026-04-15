//! High-level Page API -- mirrors Playwright's Page interface.
//!
//! All interaction methods auto-wait for element actionability.
//! Locator methods are lazy (don't query DOM until action).

use crate::actions;
use crate::backend::{AnyPage, CookieData, ImageFormat, ScreenshotOpts};
use crate::events::{EventEmitter, PageEvent};
use crate::frame::Frame;
use crate::locator::Locator;
use crate::options::{GotoOptions, RoleOptions, ScreenshotOptions, TextOptions, WaitOptions};
use crate::snapshot;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::Mutex as AsyncMutex;

/// High-level page API, mirrors Playwright's Page interface.
/// Always constructed behind `Arc<Page>` — locators, frames, and consumers
/// hold Arc refs. No cloning of the Page struct itself.
pub struct Page {
  pub(crate) inner: AnyPage,
  default_timeout: AtomicU64,
  snapshot_tracker: Arc<AsyncMutex<snapshot::SnapshotTracker>>,
  mouse_position: Mutex<(f64, f64)>,
  context_ref: Option<crate::context::ContextRef>,
}

impl Page {
  /// Create a new Page behind Arc. The only construction path.
  #[must_use]
  pub fn new(inner: AnyPage) -> Arc<Self> {
    Arc::new(Self {
      inner,
      default_timeout: AtomicU64::new(30000),
      snapshot_tracker: Arc::new(AsyncMutex::new(snapshot::SnapshotTracker::new())),
      mouse_position: Mutex::new((0.0, 0.0)),
      context_ref: None,
    })
  }

  /// Create a new Page with context reference, behind Arc.
  #[must_use]
  pub fn with_context(inner: AnyPage, context: crate::context::ContextRef) -> Arc<Self> {
    Arc::new(Self {
      inner,
      default_timeout: AtomicU64::new(30000),
      snapshot_tracker: Arc::new(AsyncMutex::new(snapshot::SnapshotTracker::new())),
      mouse_position: Mutex::new((0.0, 0.0)),
      context_ref: Some(context),
    })
  }

  /// Get the `BrowserContext` this page belongs to (matches Playwright's `page.context()`).
  #[must_use]
  pub fn context(&self) -> Option<&crate::context::ContextRef> {
    self.context_ref.as_ref()
  }

  /// Access the underlying backend page (escape hatch).
  #[must_use]
  pub fn inner(&self) -> &AnyPage {
    &self.inner
  }

  /// Set the default timeout for all operations (milliseconds).
  pub fn set_default_timeout(&self, ms: u64) {
    self.default_timeout.store(ms, Ordering::Relaxed);
  }

  /// Get the default timeout (milliseconds).
  #[must_use]
  pub fn default_timeout(&self) -> u64 {
    self.default_timeout.load(Ordering::Relaxed)
  }

  /// Get the current viewport size by querying the browser.
  ///
  /// # Errors
  ///
  /// Returns an error if the JavaScript evaluation fails.
  pub async fn viewport_size(&self) -> Result<(i64, i64), String> {
    let r = self
      .inner
      .evaluate("JSON.stringify({w:window.innerWidth,h:window.innerHeight})")
      .await?;
    let s = r
      .and_then(|v| v.as_str().map(std::string::ToString::to_string))
      .unwrap_or_default();
    let parsed: serde_json::Value = serde_json::from_str(&s).unwrap_or_default();
    let w = parsed.get("w").and_then(serde_json::Value::as_i64).unwrap_or(0);
    let h = parsed.get("h").and_then(serde_json::Value::as_i64).unwrap_or(0);
    Ok((w, h))
  }

  // ── Navigation ──────────────────────────────────────────────────────────

  /// Navigate to a URL with optional options (waitUntil, timeout).
  ///
  /// # Errors
  ///
  /// Returns an error if the navigation fails or the wait condition times out.
  #[tracing::instrument(skip(self, opts), fields(url))]
  pub async fn goto(&self, url: &str, opts: Option<GotoOptions>) -> Result<(), String> {
    tracing::debug!(target: "ferridriver::action", action = "goto", url, "page.goto");
    let (lifecycle, timeout) = Self::resolve_nav_opts(opts.as_ref(), self.default_timeout());
    self.inner.goto(url, lifecycle, timeout).await
  }

  /// Navigate back in history.
  ///
  /// # Errors
  ///
  /// Returns an error if the navigation fails or the wait condition times out.
  pub async fn go_back(&self, opts: Option<GotoOptions>) -> Result<(), String> {
    let (lifecycle, timeout) = Self::resolve_nav_opts(opts.as_ref(), self.default_timeout());
    self.inner.go_back(lifecycle, timeout).await
  }

  /// Navigate forward in history.
  ///
  /// # Errors
  ///
  /// Returns an error if the navigation fails or the wait condition times out.
  pub async fn go_forward(&self, opts: Option<GotoOptions>) -> Result<(), String> {
    let (lifecycle, timeout) = Self::resolve_nav_opts(opts.as_ref(), self.default_timeout());
    self.inner.go_forward(lifecycle, timeout).await
  }

  /// Reload the current page.
  ///
  /// # Errors
  ///
  /// Returns an error if the reload fails or the wait condition times out.
  pub async fn reload(&self, opts: Option<GotoOptions>) -> Result<(), String> {
    let (lifecycle, timeout) = Self::resolve_nav_opts(opts.as_ref(), self.default_timeout());
    self.inner.reload(lifecycle, timeout).await
  }

  /// Parse `GotoOptions` into backend `NavLifecycle` + timeout.
  fn resolve_nav_opts(opts: Option<&GotoOptions>, default_timeout: u64) -> (crate::backend::NavLifecycle, u64) {
    let wait_until = opts.and_then(|o| o.wait_until.as_deref()).unwrap_or("load");
    let timeout = opts.and_then(|o| o.timeout).unwrap_or(default_timeout);
    (crate::backend::NavLifecycle::parse_lifecycle(wait_until), timeout)
  }

  /// Get the current page URL.
  ///
  /// # Errors
  ///
  /// Returns an error if the URL cannot be retrieved from the backend.
  pub async fn url(&self) -> Result<String, String> {
    self.inner.url().await.map(std::option::Option::unwrap_or_default)
  }

  /// Get the current page title.
  ///
  /// # Errors
  ///
  /// Returns an error if the title cannot be retrieved from the backend.
  pub async fn title(&self) -> Result<String, String> {
    self.inner.title().await.map(std::option::Option::unwrap_or_default)
  }

  // ── Locators (lazy) ─────────────────────────────────────────────────────

  #[must_use]
  pub fn locator(self: &Arc<Self>, selector: &str) -> Locator {
    Locator {
      page: Arc::clone(self),
      frame_id: None,
      selector: selector.to_string(),
    }
  }

  #[must_use]
  pub fn get_by_role(self: &Arc<Self>, role: &str, opts: &RoleOptions) -> Locator {
    Locator {
      page: Arc::clone(self),
      frame_id: None,
      selector: crate::locator::build_role_selector(role, opts),
    }
  }

  #[must_use]
  pub fn get_by_text(self: &Arc<Self>, text: &str, opts: &TextOptions) -> Locator {
    let sel = if opts.exact == Some(true) {
      format!("text=\"{text}\"")
    } else {
      format!("text={text}")
    };
    Locator {
      page: Arc::clone(self),
      frame_id: None,
      selector: sel,
    }
  }

  #[must_use]
  pub fn get_by_label(self: &Arc<Self>, text: &str, opts: &TextOptions) -> Locator {
    let sel = if opts.exact == Some(true) {
      format!("label=\"{text}\"")
    } else {
      format!("label={text}")
    };
    Locator {
      page: Arc::clone(self),
      frame_id: None,
      selector: sel,
    }
  }

  #[must_use]
  pub fn get_by_placeholder(self: &Arc<Self>, text: &str, opts: &TextOptions) -> Locator {
    let sel = if opts.exact == Some(true) {
      format!("placeholder=\"{text}\"")
    } else {
      format!("placeholder={text}")
    };
    Locator {
      page: Arc::clone(self),
      frame_id: None,
      selector: sel,
    }
  }

  #[must_use]
  pub fn get_by_alt_text(self: &Arc<Self>, text: &str, opts: &TextOptions) -> Locator {
    let sel = if opts.exact == Some(true) {
      format!("alt=\"{text}\"")
    } else {
      format!("alt={text}")
    };
    Locator {
      page: Arc::clone(self),
      frame_id: None,
      selector: sel,
    }
  }

  #[must_use]
  pub fn get_by_title(self: &Arc<Self>, text: &str, opts: &TextOptions) -> Locator {
    let sel = if opts.exact == Some(true) {
      format!("title=\"{text}\"")
    } else {
      format!("title={text}")
    };
    Locator {
      page: Arc::clone(self),
      frame_id: None,
      selector: sel,
    }
  }

  #[must_use]
  pub fn get_by_test_id(self: &Arc<Self>, test_id: &str) -> Locator {
    Locator {
      page: Arc::clone(self),
      frame_id: None,
      selector: format!("testid={test_id}"),
    }
  }

  /// Create a `FrameLocator` for an `<iframe>` matching the selector.
  ///
  /// Equivalent to Playwright's `page.frameLocator(selector)`.
  #[must_use]
  pub fn frame_locator(self: &Arc<Self>, selector: &str) -> crate::locator::FrameLocator {
    self.locator(selector).content_frame()
  }

  // ── Page-level actions (convenience, delegate to locator) ───────────────

  /// Click on an element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the click fails.
  pub async fn click(self: &Arc<Self>, selector: &str) -> Result<(), String> {
    tracing::debug!(target: "ferridriver::action", action = "click", selector, "page.click");
    self.locator(selector).click().await
  }

  /// Double-click an element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the double-click fails.
  pub async fn dblclick(self: &Arc<Self>, selector: &str) -> Result<(), String> {
    tracing::debug!(target: "ferridriver::action", action = "dblclick", selector, "page.dblclick");
    self.locator(selector).dblclick().await
  }

  /// Fill an input element matching the selector with a value.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or is not fillable.
  pub async fn fill(self: &Arc<Self>, selector: &str, value: &str) -> Result<(), String> {
    tracing::debug!(target: "ferridriver::action", action = "fill", selector, "page.fill");
    self.locator(selector).fill(value).await
  }

  /// Type text character-by-character into an element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or typing fails.
  pub async fn r#type(self: &Arc<Self>, selector: &str, text: &str) -> Result<(), String> {
    self.locator(selector).r#type(text).await
  }

  /// Press a key on an element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the key press fails.
  pub async fn press(self: &Arc<Self>, selector: &str, key: &str) -> Result<(), String> {
    self.locator(selector).press(key).await
  }

  /// Hover over an element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the hover fails.
  pub async fn hover(self: &Arc<Self>, selector: &str) -> Result<(), String> {
    self.locator(selector).hover().await
  }

  /// Select an option in a `<select>` element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the option cannot be selected.
  pub async fn select_option(self: &Arc<Self>, selector: &str, value: &str) -> Result<Vec<String>, String> {
    self.locator(selector).select_option(value).await
  }

  /// Set input files on a file input element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or file setting fails.
  pub async fn set_input_files(self: &Arc<Self>, selector: &str, paths: &[String]) -> Result<(), String> {
    self.locator(selector).set_input_files(paths).await
  }

  /// Check a checkbox or radio button matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or is not checkable.
  pub async fn check(self: &Arc<Self>, selector: &str) -> Result<(), String> {
    self.locator(selector).check().await
  }

  /// Uncheck a checkbox matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or is not uncheckable.
  pub async fn uncheck(self: &Arc<Self>, selector: &str) -> Result<(), String> {
    self.locator(selector).uncheck().await
  }

  // ── Content ─────────────────────────────────────────────────────────────

  /// Get the full HTML content of the page.
  ///
  /// # Errors
  ///
  /// Returns an error if the content cannot be retrieved.
  pub async fn content(&self) -> Result<String, String> {
    self.inner.content().await
  }

  /// Set the page's HTML content.
  ///
  /// # Errors
  ///
  /// Returns an error if the content cannot be set.
  pub async fn set_content(&self, html: &str) -> Result<(), String> {
    self.inner.set_content(html).await
  }

  /// Extract the page content as markdown.
  ///
  /// # Errors
  ///
  /// Returns an error if the markdown extraction fails.
  pub async fn markdown(&self) -> Result<String, String> {
    actions::extract_markdown(&self.inner).await
  }

  /// Get the text content of an element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn text_content(self: &Arc<Self>, selector: &str) -> Result<Option<String>, String> {
    self.locator(selector).text_content().await
  }

  /// Get the inner text of an element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn inner_text(self: &Arc<Self>, selector: &str) -> Result<String, String> {
    self.locator(selector).inner_text().await
  }

  /// Get the inner HTML of an element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn inner_html(self: &Arc<Self>, selector: &str) -> Result<String, String> {
    self.locator(selector).inner_html().await
  }

  /// Get an attribute value from an element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn get_attribute(self: &Arc<Self>, selector: &str, name: &str) -> Result<Option<String>, String> {
    self.locator(selector).get_attribute(name).await
  }

  /// Get the input value of a form element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn input_value(self: &Arc<Self>, selector: &str) -> Result<String, String> {
    self.locator(selector).input_value().await
  }

  // ── State checks ────────────────────────────────────────────────────────

  /// Check if an element matching the selector is visible.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn is_visible(self: &Arc<Self>, selector: &str) -> Result<bool, String> {
    self.locator(selector).is_visible().await
  }

  /// Check if an element matching the selector is hidden.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn is_hidden(self: &Arc<Self>, selector: &str) -> Result<bool, String> {
    self.locator(selector).is_hidden().await
  }

  /// Check if an element matching the selector is enabled.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn is_enabled(self: &Arc<Self>, selector: &str) -> Result<bool, String> {
    self.locator(selector).is_enabled().await
  }

  /// Check if an element matching the selector is disabled.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn is_disabled(self: &Arc<Self>, selector: &str) -> Result<bool, String> {
    self.locator(selector).is_disabled().await
  }

  /// Check if a checkbox or radio button matching the selector is checked.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn is_checked(self: &Arc<Self>, selector: &str) -> Result<bool, String> {
    self.locator(selector).is_checked().await
  }

  // ── Evaluation ──────────────────────────────────────────────────────────

  /// Evaluate a JavaScript expression in the page context.
  ///
  /// # Errors
  ///
  /// Returns an error if the JavaScript evaluation fails.
  pub async fn evaluate(&self, expression: &str) -> Result<Option<serde_json::Value>, String> {
    self.inner.evaluate(expression).await
  }

  /// Evaluate a JavaScript expression and return the result as a string.
  ///
  /// # Errors
  ///
  /// Returns an error if the JavaScript evaluation fails.
  pub async fn evaluate_str(&self, expression: &str) -> Result<String, String> {
    self.inner.evaluate(expression).await.map(|v| {
      v.map(|val| {
        if let Some(s) = val.as_str() {
          s.to_string()
        } else {
          val.to_string()
        }
      })
      .unwrap_or_default()
    })
  }

  // ── Waiting ─────────────────────────────────────────────────────────────

  /// Wait for an element matching the selector to satisfy the wait condition.
  ///
  /// # Errors
  ///
  /// Returns an error if the wait times out.
  pub async fn wait_for_selector(self: &Arc<Self>, selector: &str, opts: WaitOptions) -> Result<(), String> {
    self.locator(selector).wait_for(opts).await
  }

  /// Wait for the page URL to contain the given pattern.
  ///
  /// # Errors
  ///
  /// Returns an error if the wait times out.
  pub async fn wait_for_url(&self, url_pattern: &str) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(self.default_timeout());
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

  /// Wait for a specific load state. Supported states:
  /// - `"load"` (default) - wait for `document.readyState === "complete"`
  /// - `"domcontentloaded"` - wait for `document.readyState !== "loading"`
  /// - `"networkidle"` - wait for no network activity for 500ms
  ///
  /// # Errors
  ///
  /// Returns an error if the wait times out before the load state is reached.
  pub async fn wait_for_load_state(&self, state: Option<&str>) -> Result<(), String> {
    let state = state.unwrap_or("load");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(self.default_timeout());

    match state {
      "domcontentloaded" => loop {
        if tokio::time::Instant::now() >= deadline {
          return Err("Timeout waiting for domcontentloaded".into());
        }
        if let Ok(Some(v)) = self.inner.evaluate("document.readyState").await {
          let s = v.as_str().unwrap_or("loading");
          if s == "interactive" || s == "complete" {
            return Ok(());
          }
        }
        tokio::time::sleep(std::time::Duration::from_millis(16)).await;
      },
      "networkidle" => {
        // Wait for no pending network requests for 500ms.
        // Uses Performance API to detect network activity.
        let mut idle_since = tokio::time::Instant::now();
        let idle_threshold = std::time::Duration::from_millis(500);
        loop {
          if tokio::time::Instant::now() >= deadline {
            return Err("Timeout waiting for networkidle".into());
          }
          // Check if there are pending resource loads
          let has_pending = self
            .inner
            .evaluate(
              "(function(){var p=performance.getEntriesByType('resource');\
             var now=performance.now();\
             return p.some(function(e){return e.responseEnd===0 || (now - e.responseEnd) < 100})})()",
            )
            .await
            .ok()
            .flatten();
          if has_pending == Some(serde_json::Value::Bool(true)) {
            idle_since = tokio::time::Instant::now();
          } else if tokio::time::Instant::now() - idle_since >= idle_threshold {
            return Ok(());
          }
          tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
      },
      _ => {
        // "load" -- wait for document.readyState === "complete"
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
      },
    }
  }

  // ── Screenshots ─────────────────────────────────────────────────────────

  /// Take a screenshot of the page.
  ///
  /// # Errors
  ///
  /// Returns an error if the screenshot capture fails.
  pub async fn screenshot(&self, opts: ScreenshotOptions) -> Result<Vec<u8>, String> {
    let format = match opts.format.as_deref() {
      Some("jpeg" | "jpg") => ImageFormat::Jpeg,
      Some("webp") => ImageFormat::Webp,
      _ => ImageFormat::Png,
    };
    self
      .inner
      .screenshot(ScreenshotOpts {
        format,
        quality: opts.quality,
        full_page: opts.full_page.unwrap_or(false),
      })
      .await
  }

  /// Take a screenshot of a specific element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or screenshot capture fails.
  pub async fn screenshot_element(self: &Arc<Self>, selector: &str) -> Result<Vec<u8>, String> {
    self.locator(selector).screenshot().await
  }

  // ── PDF ─────────────────────────────────────────────────────────────────

  /// Generate PDF from the page (headless Chrome only).
  ///
  /// # Errors
  ///
  /// Returns an error if PDF generation fails or is not supported by the backend.
  pub async fn pdf(&self, landscape: bool, print_background: bool) -> Result<Vec<u8>, String> {
    self.inner.pdf(landscape, print_background).await
  }

  // ── Snapshot ────────────────────────────────────────────────────────────

  /// LLM-optimized accessibility snapshot with page context header, optional
  /// depth limiting, and incremental change tracking.
  ///
  /// Returns `SnapshotForAI`:
  /// - `full`: page header (URL, title, scroll, console errors) + accessibility tree
  /// - `incremental`: only changed/new nodes since last call with same `track` key
  /// - `ref_map`: element refs (e.g. "e5") to backend DOM node IDs
  ///
  /// Options:
  /// - `depth`: limits accessibility tree depth (-1 or None = unlimited).
  ///   Uses native CDP depth param on Chrome, `NSAccessibility` depth on `WebKit`.
  /// - `track`: enables incremental tracking per key.
  ///
  /// # Errors
  ///
  /// Returns an error if the accessibility snapshot cannot be built.
  pub async fn snapshot_for_ai(&self, opts: snapshot::SnapshotOptions) -> Result<snapshot::SnapshotForAI, String> {
    let mut tracker = self.snapshot_tracker.lock().await;
    snapshot::build_snapshot_for_ai(&self.inner, &opts, &mut tracker).await
  }

  // ── Viewport ────────────────────────────────────────────────────────────

  /// Set the viewport size by width and height.
  ///
  /// # Errors
  ///
  /// Returns an error if the viewport emulation fails.
  pub async fn set_viewport_size(&self, width: i64, height: i64) -> Result<(), String> {
    self
      .inner
      .emulate_viewport(&crate::options::ViewportConfig {
        width,
        height,
        ..Default::default()
      })
      .await
  }

  // ── Input devices ───────────────────────────────────────────────────────

  /// Click at specific coordinates.
  ///
  /// # Errors
  ///
  /// Returns an error if the click dispatch fails.
  pub async fn click_at(&self, x: f64, y: f64) -> Result<(), String> {
    self.inner.click_at(x, y).await?;
    *self
      .mouse_position
      .lock()
      .map_err(|e| format!("mouse position lock poisoned: {e}"))? = (x, y);
    Ok(())
  }

  /// Click at specific coordinates with button and click count options.
  ///
  /// # Errors
  ///
  /// Returns an error if the click dispatch fails.
  pub async fn click_at_opts(&self, x: f64, y: f64, button: &str, click_count: u32) -> Result<(), String> {
    self.inner.click_at_opts(x, y, button, click_count).await?;
    *self
      .mouse_position
      .lock()
      .map_err(|e| format!("mouse position lock poisoned: {e}"))? = (x, y);
    Ok(())
  }

  /// Move the mouse to specific coordinates.
  ///
  /// # Errors
  ///
  /// Returns an error if the mouse move dispatch fails.
  pub(crate) async fn move_mouse(&self, x: f64, y: f64) -> Result<(), String> {
    self.inner.move_mouse(x, y).await?;
    *self
      .mouse_position
      .lock()
      .map_err(|e| format!("mouse position lock poisoned: {e}"))? = (x, y);
    Ok(())
  }

  /// Move the mouse smoothly from one point to another over multiple steps.
  ///
  /// # Errors
  ///
  /// Returns an error if the mouse move dispatch fails.
  pub async fn move_mouse_smooth(
    &self,
    from_x: f64,
    from_y: f64,
    to_x: f64,
    to_y: f64,
    steps: u32,
  ) -> Result<(), String> {
    self.inner.move_mouse_smooth(from_x, from_y, to_x, to_y, steps).await?;
    *self
      .mouse_position
      .lock()
      .map_err(|e| format!("mouse position lock poisoned: {e}"))? = (to_x, to_y);
    Ok(())
  }

  /// Drag an element matching `source_selector` onto an element matching `target_selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if either element cannot be found or the drag-and-drop operation fails.
  pub async fn drag_and_drop(self: &Arc<Self>, source_selector: &str, target_selector: &str) -> Result<(), String> {
    self
      .locator(source_selector)
      .drag_to(&self.locator(target_selector))
      .await
  }

  /// Dispatch a keyDown event for a single key (does NOT release it).
  ///
  /// # Errors
  ///
  /// Returns an error if the key down dispatch fails.
  pub(crate) async fn key_down(&self, key: &str) -> Result<(), String> {
    self.inner.key_down(key).await
  }

  /// Dispatch a keyUp event for a single key.
  ///
  /// # Errors
  ///
  /// Returns an error if the key up dispatch fails.
  pub(crate) async fn key_up(&self, key: &str) -> Result<(), String> {
    self.inner.key_up(key).await
  }

  /// Press a key or combo (e.g., "Enter", "Control+a").
  ///
  /// # Errors
  ///
  /// Returns an error if the key press dispatch fails.
  pub(crate) async fn press_key(&self, key: &str) -> Result<(), String> {
    self.inner.press_key(key).await
  }

  /// Find element by CSS selector (raw backend access).
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn find_element(&self, selector: &str) -> Result<crate::backend::AnyElement, String> {
    self.inner.find_element(selector).await
  }

  // ── Emulation ───────────────────────────────────────────────────────────

  /// Set viewport with full configuration (matches Playwright's viewport options).
  ///
  /// # Errors
  ///
  /// Returns an error if the viewport emulation fails.
  pub async fn set_viewport(&self, config: &crate::options::ViewportConfig) -> Result<(), String> {
    self.inner.emulate_viewport(config).await
  }

  /// Set the user agent string.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend rejects the user agent change.
  pub async fn set_user_agent(&self, ua: &str) -> Result<(), String> {
    self.inner.set_user_agent(ua).await
  }

  /// Set the geolocation override.
  ///
  /// # Errors
  ///
  /// Returns an error if the geolocation emulation fails.
  pub async fn set_geolocation(&self, lat: f64, lng: f64, accuracy: f64) -> Result<(), String> {
    self.inner.set_geolocation(lat, lng, accuracy).await
  }

  /// Set network conditions (offline, latency, throughput).
  ///
  /// # Errors
  ///
  /// Returns an error if the network emulation fails.
  pub async fn set_network_state(&self, offline: bool, latency: f64, download: f64, upload: f64) -> Result<(), String> {
    self.inner.set_network_state(offline, latency, download, upload).await
  }

  /// Set the browser locale (affects navigator.language and Intl APIs).
  ///
  /// # Errors
  ///
  /// Returns an error if the locale emulation fails.
  pub async fn set_locale(&self, locale: &str) -> Result<(), String> {
    self.inner.set_locale(locale).await
  }

  /// Set the browser timezone (affects Date and Intl.DateTimeFormat).
  ///
  /// # Errors
  ///
  /// Returns an error if the timezone emulation fails.
  pub async fn set_timezone(&self, timezone_id: &str) -> Result<(), String> {
    self.inner.set_timezone(timezone_id).await
  }

  /// Emulate media features (color scheme, reduced motion, media type, etc.).
  /// Matches Playwright's `page.emulateMedia()`.
  ///
  /// # Errors
  ///
  /// Returns an error if the media emulation fails.
  pub async fn emulate_media(&self, opts: &crate::options::EmulateMediaOptions) -> Result<(), String> {
    self.inner.emulate_media(opts).await
  }

  /// Enable or disable JavaScript execution.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend rejects the change.
  pub async fn set_javascript_enabled(&self, enabled: bool) -> Result<(), String> {
    self.inner.set_javascript_enabled(enabled).await
  }

  /// Set extra HTTP headers that will be sent with every request.
  ///
  /// # Errors
  ///
  /// Returns an error if the headers cannot be set.
  pub async fn set_extra_http_headers(&self, headers: &rustc_hash::FxHashMap<String, String>) -> Result<(), String> {
    self.inner.set_extra_http_headers(headers).await
  }

  /// Grant browser permissions (geolocation, notifications, camera, etc.).
  ///
  /// # Errors
  ///
  /// Returns an error if the permission grant fails.
  pub async fn grant_permissions(&self, permissions: &[String], origin: Option<&str>) -> Result<(), String> {
    self.inner.grant_permissions(permissions, origin).await
  }

  /// Reset all granted permissions.
  ///
  /// # Errors
  ///
  /// Returns an error if the permission reset fails.
  pub async fn reset_permissions(&self) -> Result<(), String> {
    self.inner.reset_permissions().await
  }

  /// Bypass Content Security Policy. Must be called before any navigation.
  /// Matches Playwright's `bypassCSP` context option.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend rejects the change.
  pub async fn set_bypass_csp(&self, enabled: bool) -> Result<(), String> {
    self.inner.set_bypass_csp(enabled).await
  }

  /// Ignore HTTPS certificate errors for this page.
  /// Matches Playwright's `ignoreHTTPSErrors` option.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend rejects the change.
  pub async fn set_ignore_certificate_errors(&self, ignore: bool) -> Result<(), String> {
    self.inner.set_ignore_certificate_errors(ignore).await
  }

  /// Configure download behavior (allow/deny, download directory).
  /// Matches Playwright's `acceptDownloads` option.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend rejects the change.
  pub async fn set_download_behavior(&self, behavior: &str, download_path: &str) -> Result<(), String> {
    self.inner.set_download_behavior(behavior, download_path).await
  }

  /// Set HTTP credentials for basic/digest auth.
  /// Matches Playwright's `httpCredentials` option.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend rejects the change.
  pub async fn set_http_credentials(&self, username: &str, password: &str) -> Result<(), String> {
    self.inner.set_http_credentials(username, password).await
  }

  /// Block service worker registration.
  /// Matches Playwright's `serviceWorkers: "block"` option.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend rejects the change.
  pub async fn set_service_workers_blocked(&self, blocked: bool) -> Result<(), String> {
    self.inner.set_service_workers_blocked(blocked).await
  }

  /// Emulate focus state (page always appears focused even when not).
  ///
  /// # Errors
  ///
  /// Returns an error if the focus emulation fails.
  pub async fn set_focus_emulation_enabled(&self, enabled: bool) -> Result<(), String> {
    self.inner.set_focus_emulation_enabled(enabled).await
  }

  // ── Tracing ─────────────────────────────────────────────────────────────

  /// Start performance tracing.
  ///
  /// # Errors
  ///
  /// Returns an error if tracing cannot be started.
  pub async fn start_tracing(&self) -> Result<(), String> {
    self.inner.start_tracing().await
  }

  /// Stop performance tracing.
  ///
  /// # Errors
  ///
  /// Returns an error if tracing cannot be stopped.
  pub async fn stop_tracing(&self) -> Result<(), String> {
    self.inner.stop_tracing().await
  }

  /// Get performance metrics from the page.
  ///
  /// # Errors
  ///
  /// Returns an error if metrics cannot be retrieved.
  pub async fn metrics(&self) -> Result<Vec<crate::backend::MetricData>, String> {
    self.inner.metrics().await
  }

  // ── Storage State ──────────────────────────────────────────────────────

  /// Serialize the current page's storage state (cookies + localStorage) to JSON.
  ///
  /// Returns Playwright-compatible format:
  /// ```json
  /// {
  ///   "cookies": [{ "name": "...", "value": "...", "domain": "...", ... }],
  ///   "origins": [{ "origin": "https://...", "localStorage": [{ "name": "...", "value": "..." }] }]
  /// }
  /// ```
  ///
  /// Can be saved to a file and loaded later with `set_storage_state` or via
  /// test config `storage_state: "auth.json"`.
  ///
  /// # Errors
  ///
  /// Returns an error if cookies or localStorage cannot be retrieved.
  pub async fn storage_state(&self) -> Result<serde_json::Value, String> {
    let cookies = self.inner.get_cookies().await?;
    let cookies_json: Vec<serde_json::Value> = cookies
      .iter()
      .map(|c| {
        let mut obj = serde_json::json!({
          "name": c.name, "value": c.value, "domain": c.domain, "path": c.path,
          "secure": c.secure, "httpOnly": c.http_only
        });
        if let Some(expires) = c.expires {
          obj["expires"] = serde_json::json!(expires);
        }
        if let Some(same_site) = c.same_site {
          obj["sameSite"] = serde_json::json!(same_site.as_str());
        }
        obj
      })
      .collect();

    // Get the current origin for localStorage grouping.
    let origin = self
      .inner
      .evaluate("location.origin")
      .await
      .ok()
      .flatten()
      .and_then(|v| v.as_str().map(str::to_string))
      .unwrap_or_default();

    // Dump localStorage as array of { name, value } pairs (Playwright format).
    let storage_js = r"JSON.stringify(
      Object.keys(localStorage).map(k => ({ name: k, value: localStorage.getItem(k) }))
    )";
    let storage_r = self.inner.evaluate(storage_js).await.ok().flatten();
    let local_storage: Vec<serde_json::Value> = storage_r
      .and_then(|v| v.as_str().and_then(|s| serde_json::from_str(s).ok()))
      .unwrap_or_default();

    let mut origins = Vec::new();
    if !local_storage.is_empty() && !origin.is_empty() {
      origins.push(serde_json::json!({
        "origin": origin,
        "localStorage": local_storage,
      }));
    }

    Ok(serde_json::json!({
      "cookies": cookies_json,
      "origins": origins,
    }))
  }

  /// Restore a previously saved storage state (cookies + localStorage).
  ///
  /// Accepts Playwright-compatible format with `origins[].localStorage[]` (name/value pairs).
  ///
  /// # Errors
  ///
  /// Returns an error if cookies or localStorage cannot be restored.
  pub async fn set_storage_state(&self, state: &serde_json::Value) -> Result<(), String> {
    // Restore cookies.
    if let Some(cookies) = state.get("cookies").and_then(|v| v.as_array()) {
      for c in cookies {
        let cookie = CookieData {
          name: c.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
          value: c.get("value").and_then(|v| v.as_str()).unwrap_or("").to_string(),
          domain: c.get("domain").and_then(|v| v.as_str()).unwrap_or("").to_string(),
          path: c.get("path").and_then(|v| v.as_str()).unwrap_or("/").to_string(),
          secure: c.get("secure").and_then(serde_json::Value::as_bool).unwrap_or(false),
          http_only: c.get("httpOnly").and_then(serde_json::Value::as_bool).unwrap_or(false),
          expires: c.get("expires").and_then(serde_json::Value::as_f64),
          same_site: c
            .get("sameSite")
            .and_then(|v| v.as_str())
            .and_then(|v| v.parse::<crate::backend::SameSite>().ok()),
        };
        self.inner.set_cookie(cookie).await?;
      }
    }

    // Restore per-origin localStorage (Playwright format).
    if let Some(origins) = state.get("origins").and_then(|v| v.as_array()) {
      for origin_entry in origins {
        let origin = origin_entry.get("origin").and_then(|v| v.as_str()).unwrap_or("");
        if let Some(items) = origin_entry.get("localStorage").and_then(|v| v.as_array()) {
          // Navigate to the origin so localStorage.setItem works in the right scope.
          // Only navigate if the current page isn't already on this origin.
          let current_origin = self
            .inner
            .evaluate("location.origin")
            .await
            .ok()
            .flatten()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
          if !origin.is_empty() && current_origin != origin {
            let _ = self
              .inner
              .goto(origin, crate::backend::NavLifecycle::Load, 10_000)
              .await;
          }
          for item in items {
            let key = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let val = item.get("value").and_then(|v| v.as_str()).unwrap_or("");
            self
              .inner
              .evaluate(&format!(
                "localStorage.setItem('{}', '{}')",
                crate::steps::js_escape(key),
                crate::steps::js_escape(val)
              ))
              .await?;
          }
        }
      }
    }

    Ok(())
  }

  // ── Focus / dispatch ─────────────────────────────────────────────────

  /// Focus an element by selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn focus(self: &Arc<Self>, selector: &str) -> Result<(), String> {
    self.locator(selector).focus().await
  }

  /// Dispatch an event on an element by selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the event dispatch fails.
  pub async fn dispatch_event(self: &Arc<Self>, selector: &str, event_type: &str) -> Result<(), String> {
    self.locator(selector).dispatch_event(event_type).await
  }

  /// Check if an element is editable (not disabled, not readonly).
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn is_editable(self: &Arc<Self>, selector: &str) -> Result<bool, String> {
    self.locator(selector).is_editable().await
  }

  // ── Waiting (additional) ────────────────────────────────────────────────

  /// Wait for a JS function/expression to return a truthy value.
  ///
  /// # Errors
  ///
  /// Returns an error if the wait times out.
  pub async fn wait_for_function(
    &self,
    expression: &str,
    timeout_ms: Option<u64>,
  ) -> Result<serde_json::Value, String> {
    let timeout = timeout_ms.unwrap_or(self.default_timeout());
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
  ///
  /// # Errors
  ///
  /// Returns an error if the wait times out.
  pub async fn wait_for_navigation(&self, timeout_ms: Option<u64>) -> Result<(), String> {
    let timeout = timeout_ms.unwrap_or(self.default_timeout());
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
  ///
  /// # Errors
  ///
  /// Returns an error if the wheel event dispatch fails.
  pub(crate) async fn mouse_wheel(&self, delta_x: f64, delta_y: f64) -> Result<(), String> {
    self.inner.mouse_wheel(delta_x, delta_y).await
  }

  /// Mouse button down (without up). For custom drag sequences.
  ///
  /// # Errors
  ///
  /// Returns an error if the mouse down dispatch fails.
  pub(crate) async fn mouse_down(&self, x: f64, y: f64, button: &str) -> Result<(), String> {
    self.inner.mouse_down(x, y, button).await?;
    *self
      .mouse_position
      .lock()
      .map_err(|e| format!("mouse position lock poisoned: {e}"))? = (x, y);
    Ok(())
  }

  /// Mouse button up.
  ///
  /// # Errors
  ///
  /// Returns an error if the mouse up dispatch fails.
  pub(crate) async fn mouse_up(&self, x: f64, y: f64, button: &str) -> Result<(), String> {
    self.inner.mouse_up(x, y, button).await?;
    *self
      .mouse_position
      .lock()
      .map_err(|e| format!("mouse position lock poisoned: {e}"))? = (x, y);
    Ok(())
  }

  /// Bring this page to front (focus).
  ///
  /// # Errors
  ///
  /// Returns an error if the page cannot be focused.
  pub async fn bring_to_front(&self) -> Result<(), String> {
    let _ = self.inner.evaluate("window.focus()").await;
    Ok(())
  }

  // ── Frames ─────────────────────────────────────────────────────────────

  /// Get the main frame of this page.
  ///
  /// # Errors
  ///
  /// Returns an error if the frame tree cannot be retrieved or no main frame exists.
  pub async fn main_frame(self: &Arc<Self>) -> Result<Frame, String> {
    let frames = self.inner.get_frame_tree().await?;
    let main = frames
      .into_iter()
      .find(|f| f.parent_frame_id.is_none())
      .ok_or("No main frame found")?;
    Ok(Frame::from_info(Arc::clone(self), main))
  }

  /// Get all frames in the page (main frame + all iframes).
  ///
  /// # Errors
  ///
  /// Returns an error if the frame tree cannot be retrieved.
  pub async fn frames(self: &Arc<Self>) -> Result<Vec<Frame>, String> {
    let infos = self.inner.get_frame_tree().await?;
    Ok(
      infos
        .into_iter()
        .map(|info| Frame::from_info(Arc::clone(self), info))
        .collect(),
    )
  }

  /// Find a frame by name or URL.
  ///
  /// # Errors
  ///
  /// Returns an error if the frame tree cannot be retrieved.
  pub async fn frame(self: &Arc<Self>, name_or_url: &str) -> Result<Option<Frame>, String> {
    let frames = self.frames().await?;
    Ok(
      frames
        .into_iter()
        .find(|f| f.name() == name_or_url || f.url() == name_or_url),
    )
  }

  // ── Events ────────────────────────────────────────────────────────────

  /// Get the event emitter for subscribing to page events.
  #[must_use]
  pub fn events(&self) -> &EventEmitter {
    self.inner.events()
  }

  /// Subscribe to page events. Calls the callback for each matching event.
  /// Returns a `ListenerId` for later removal with `off()`.
  ///
  /// ```ignore
  /// let id = page.on("response", Arc::new(|event| {
  ///     if let PageEvent::Response(r) = event {
  ///         println!("Response: {} {}", r.status, r.url);
  ///     }
  /// }));
  /// ```
  pub fn on(&self, event_name: &str, callback: crate::events::EventCallback) -> crate::events::ListenerId {
    self.inner.events().on(event_name, callback)
  }

  /// Subscribe to a single event, then auto-remove the listener.
  pub fn once(&self, event_name: &str, callback: crate::events::EventCallback) -> crate::events::ListenerId {
    self.inner.events().once(event_name, callback)
  }

  /// Remove an event listener by ID.
  pub fn off(&self, id: crate::events::ListenerId) {
    self.inner.events().off(id);
  }

  /// Remove all event listeners.
  pub fn remove_all_listeners(&self) {
    self.inner.events().remove_all_listeners();
  }

  /// Start listening for a navigation event. Call BEFORE the action that triggers navigation.
  /// Returns a future that resolves when navigation completes.
  ///
  /// ```ignore
  /// let nav = page.expect_navigation(None);
  /// page.click("#link").await?;
  /// nav.await?; // resolves when navigation completes
  /// ```
  ///
  /// # Errors
  ///
  /// Returns an error if the navigation event does not occur within the timeout.
  pub fn expect_navigation(
    &self,
    timeout_ms: Option<u64>,
  ) -> impl std::future::Future<Output = Result<(), String>> + '_ {
    let timeout = timeout_ms.unwrap_or(self.default_timeout());
    let events = self.inner.events().clone();
    async move {
      events
        .wait_for(|e| matches!(e, PageEvent::Load | PageEvent::DomContentLoaded), timeout)
        .await?;
      Ok(())
    }
  }

  /// Start listening for a response matching URL pattern. Call BEFORE the action.
  ///
  /// # Errors
  ///
  /// Returns an error if no matching response is received within the timeout.
  pub fn expect_response(
    &self,
    url_pattern: &str,
    timeout_ms: Option<u64>,
  ) -> impl std::future::Future<Output = Result<crate::events::NetResponse, String>> + '_ {
    let timeout = timeout_ms.unwrap_or(self.default_timeout());
    let events = self.inner.events().clone();
    let pattern = url_pattern.to_string();
    async move {
      let event = events
        .wait_for(
          move |e| matches!(e, PageEvent::Response(r) if r.url.contains(&pattern)),
          timeout,
        )
        .await?;
      match event {
        PageEvent::Response(r) => Ok(r),
        _ => Err("Unexpected event type".into()),
      }
    }
  }

  /// Start listening for a request matching URL pattern. Call BEFORE the action.
  ///
  /// # Errors
  ///
  /// Returns an error if no matching request is received within the timeout.
  pub fn expect_request(
    &self,
    url_pattern: &str,
    timeout_ms: Option<u64>,
  ) -> impl std::future::Future<Output = Result<crate::context::NetRequest, String>> + '_ {
    let timeout = timeout_ms.unwrap_or(self.default_timeout());
    let events = self.inner.events().clone();
    let pattern = url_pattern.to_string();
    async move {
      let event = events
        .wait_for(
          move |e| matches!(e, PageEvent::Request(r) if r.url.contains(&pattern)),
          timeout,
        )
        .await?;
      match event {
        PageEvent::Request(r) => Ok(r),
        _ => Err("Unexpected event type".into()),
      }
    }
  }

  /// Start listening for a download. Call BEFORE the action that triggers it.
  ///
  /// # Errors
  ///
  /// Returns an error if no download event occurs within the timeout.
  pub fn expect_download(
    &self,
    timeout_ms: Option<u64>,
  ) -> impl std::future::Future<Output = Result<crate::events::DownloadInfo, String>> + '_ {
    let timeout = timeout_ms.unwrap_or(self.default_timeout());
    let events = self.inner.events().clone();
    async move {
      let event = events
        .wait_for(|e| matches!(e, PageEvent::Download(_)), timeout)
        .await?;
      match event {
        PageEvent::Download(d) => Ok(d),
        _ => Err("Unexpected event type".into()),
      }
    }
  }

  /// Wait for a specific event (by name) with timeout.
  ///
  /// # Errors
  ///
  /// Returns an error if the event does not occur within the timeout.
  pub async fn wait_for_event(&self, event_name: &str, timeout_ms: Option<u64>) -> Result<PageEvent, String> {
    self
      .inner
      .events()
      .wait_for_event(event_name, timeout_ms.unwrap_or(self.default_timeout()))
      .await
  }

  /// Wait for a download to start, matching an optional URL pattern.
  ///
  /// # Errors
  ///
  /// Returns an error if no matching download occurs within the timeout.
  pub async fn wait_for_download(
    &self,
    url_pattern: Option<&str>,
    timeout_ms: Option<u64>,
  ) -> Result<crate::events::DownloadInfo, String> {
    let pattern = url_pattern.map(std::string::ToString::to_string);
    let event = self
      .inner
      .events()
      .wait_for(
        move |e| matches!(e, PageEvent::Download(d) if pattern.as_ref().is_none_or(|p| d.url.contains(p))),
        timeout_ms.unwrap_or(self.default_timeout()),
      )
      .await?;
    match event {
      PageEvent::Download(d) => Ok(d),
      _ => Err("Unexpected event type".into()),
    }
  }

  /// Wait for a network request matching a URL pattern.
  ///
  /// # Errors
  ///
  /// Returns an error if no matching request occurs within the timeout.
  pub async fn wait_for_request(
    &self,
    url_pattern: &str,
    timeout_ms: Option<u64>,
  ) -> Result<crate::context::NetRequest, String> {
    let pattern = url_pattern.to_string();
    let event = self
      .inner
      .events()
      .wait_for(
        move |e| matches!(e, PageEvent::Request(r) if r.url.contains(&pattern)),
        timeout_ms.unwrap_or(self.default_timeout()),
      )
      .await?;
    match event {
      PageEvent::Request(r) => Ok(r),
      _ => Err("Unexpected event type".into()),
    }
  }

  /// Wait for a network response matching a URL pattern.
  ///
  /// # Errors
  ///
  /// Returns an error if no matching response occurs within the timeout.
  pub async fn wait_for_response(
    &self,
    url_pattern: &str,
    timeout_ms: Option<u64>,
  ) -> Result<crate::events::NetResponse, String> {
    let pattern = url_pattern.to_string();
    let event = self
      .inner
      .events()
      .wait_for(
        move |e| matches!(e, PageEvent::Response(r) if r.url.contains(&pattern)),
        timeout_ms.unwrap_or(self.default_timeout()),
      )
      .await?;
    match event {
      PageEvent::Response(r) => Ok(r),
      _ => Err("Unexpected event type".into()),
    }
  }

  // ── Network Interception ────────────────────────────────────────────────

  /// Intercept network requests matching a URL glob pattern.
  /// The handler receives the intercepted request and returns a `RouteAction`
  /// (Continue, Fulfill, or Abort).
  ///
  /// ```ignore
  /// use ferridriver::route::{RouteAction, FulfillResponse};
  /// use std::sync::Arc;
  ///
  /// // Mock an API endpoint
  /// page.route("**/api/data", Arc::new(|req| {
  ///     RouteAction::Fulfill(FulfillResponse {
  ///         status: 200,
  ///         body: b"{\"mocked\": true}".to_vec(),
  ///         content_type: Some("application/json".into()),
  ///         ..Default::default()
  ///     })
  /// })).await?;
  ///
  /// // Block image loading
  /// page.route("**/*.{png,jpg,gif}", Arc::new(|_| {
  ///     RouteAction::Abort("blockedbyclient".into())
  /// })).await?;
  /// ```
  ///
  /// # Errors
  ///
  /// Returns an error if the route interception cannot be set up.
  pub async fn route(&self, pattern: &str, handler: crate::route::RouteHandler) -> Result<(), String> {
    self.inner.route(pattern, handler).await
  }

  /// Remove all route handlers matching the glob pattern.
  ///
  /// # Errors
  ///
  /// Returns an error if the route handlers cannot be removed.
  pub async fn unroute(&self, pattern: &str) -> Result<(), String> {
    self.inner.unroute(pattern).await
  }

  // ── Exposed Functions ───────────────────────────────────────────────────

  /// Expose a Rust function to the page as `window.<name>(...)`.
  /// The page can call it as an async function and receive the return value.
  /// The exposed function persists across navigations.
  ///
  /// ```ignore
  /// use std::sync::Arc;
  ///
  /// page.expose_function("compute", Arc::new(|args| {
  ///     let x = args[0].as_f64().unwrap_or(0.0);
  ///     serde_json::json!(x * 2.0)
  /// })).await?;
  ///
  /// // In the page:
  /// // const result = await window.compute(21); // returns 42
  /// ```
  ///
  /// # Errors
  ///
  /// Returns an error if the function cannot be exposed to the page.
  pub async fn expose_function(&self, name: &str, func: crate::events::ExposedFn) -> Result<(), String> {
    self.inner.expose_function(name, func).await
  }

  /// Remove a previously exposed function.
  ///
  /// # Errors
  ///
  /// Returns an error if the function cannot be removed.
  pub async fn remove_exposed_function(&self, name: &str) -> Result<(), String> {
    self.inner.remove_exposed_function(name).await
  }

  // ── Script / Style injection ────────────────────────────────────────────

  /// Add a `<script>` tag to the page. Provide either `url` (external) or `content` (inline).
  /// For URL scripts, waits for the script to load before returning.
  ///
  /// # Errors
  ///
  /// Returns an error if neither `url` nor `content` is provided, or if injection fails.
  pub async fn add_script_tag(
    &self,
    url: Option<&str>,
    content: Option<&str>,
    script_type: Option<&str>,
  ) -> Result<(), String> {
    let t = script_type.unwrap_or("text/javascript");
    if let Some(url) = url {
      self.inner.evaluate(&format!(
        "(function(){{return new Promise(function(r,j){{var s=document.createElement('script');\
         s.type='{}';s.src='{}';s.onload=r;s.onerror=function(){{j(new Error('Failed to load script'))}};document.head.appendChild(s)}})}})();",
        crate::steps::js_escape(t), crate::steps::js_escape(url)
      )).await?;
    } else if let Some(content) = content {
      self.inner.evaluate(&format!(
        "(function(){{var s=document.createElement('script');s.type='{}';s.text='{}';document.head.appendChild(s)}})()",
        crate::steps::js_escape(t), crate::steps::js_escape(content)
      )).await?;
    } else {
      return Err("Provide either 'url' or 'content'".into());
    }
    Ok(())
  }

  /// Add a `<style>` tag or `<link>` stylesheet to the page.
  /// Provide either `url` (external CSS) or `content` (inline CSS).
  /// For URL stylesheets, waits for the stylesheet to load before returning.
  ///
  /// # Errors
  ///
  /// Returns an error if neither `url` nor `content` is provided, or if injection fails.
  pub async fn add_style_tag(&self, url: Option<&str>, content: Option<&str>) -> Result<(), String> {
    if let Some(url) = url {
      self.inner.evaluate(&format!(
        "(function(){{return new Promise(function(r,j){{var l=document.createElement('link');\
         l.rel='stylesheet';l.href='{}';l.onload=r;l.onerror=function(){{j(new Error('Failed to load stylesheet'))}};document.head.appendChild(l)}})}})();",
        crate::steps::js_escape(url)
      )).await?;
    } else if let Some(content) = content {
      self
        .inner
        .evaluate(&format!(
          "(function(){{var s=document.createElement('style');s.textContent='{}';document.head.appendChild(s)}})()",
          crate::steps::js_escape(content)
        ))
        .await?;
    } else {
      return Err("Provide either 'url' or 'content'".into());
    }
    Ok(())
  }

  // ── Dialog handling ─────────────────────────────────────────────────────

  /// Set a custom dialog handler. The handler is called for every JS dialog
  /// (alert, confirm, prompt, beforeunload) and decides the action to take.
  ///
  /// Default behavior: accept alerts/confirms, accept prompts with default value.
  ///
  /// ```ignore
  /// use ferridriver::events::{DialogAction, PendingDialog};
  /// use std::sync::Arc;
  ///
  /// // Dismiss all dialogs
  /// page.set_dialog_handler(Arc::new(|_dialog: &PendingDialog| DialogAction::Dismiss)).await;
  ///
  /// // Accept prompts with custom text
  /// page.set_dialog_handler(Arc::new(|dialog: &PendingDialog| {
  ///     if dialog.dialog_type == "prompt" {
  ///         DialogAction::Accept(Some("custom answer".into()))
  ///     } else {
  ///         DialogAction::Accept(None)
  ///     }
  /// })).await;
  /// ```
  pub async fn set_dialog_handler(&self, handler: crate::events::DialogHandler) {
    self.inner.set_dialog_handler(handler).await;
  }

  // ── Init Scripts ────────────────────────────────────────────────────────

  /// Inject a script to run before any page JS on every navigation.
  /// The script runs at document start, before any page scripts execute.
  /// Returns an identifier that can be used with `remove_init_script`.
  ///
  /// # Errors
  ///
  /// Returns an error if the init script cannot be injected.
  pub async fn add_init_script(&self, source: &str) -> Result<String, String> {
    self.inner.add_init_script(source).await
  }

  /// Remove a previously injected init script by identifier.
  ///
  /// # Errors
  ///
  /// Returns an error if the init script cannot be removed.
  pub async fn remove_init_script(&self, identifier: &str) -> Result<(), String> {
    self.inner.remove_init_script(identifier).await
  }

  // ── Lifecycle ───────────────────────────────────────────────────────────

  /// Close this page. After closing, most operations will fail.
  ///
  /// # Errors
  ///
  /// Returns an error if the page cannot be closed.
  #[tracing::instrument(skip(self))]
  pub async fn close(&self) -> Result<(), String> {
    self.inner.close_page().await?;

    // Remove closed page from context's page list so context.pages() stays accurate.
    if let Some(ctx) = &self.context_ref {
      let mut state = ctx.state.write().await;
      if let Ok(browser_ctx) = state.context_mut_checked(&ctx.name) {
        browser_ctx.pages.retain(|p| !p.is_closed());
        if browser_ctx.active_page_idx >= browser_ctx.pages.len() && !browser_ctx.pages.is_empty() {
          browser_ctx.active_page_idx = browser_ctx.pages.len() - 1;
        }
      }
    }

    Ok(())
  }

  /// Check if this page has been closed.
  #[must_use]
  pub fn is_closed(&self) -> bool {
    self.inner.is_closed()
  }

  // ── Input device accessors ────────────────────────────────────────────

  /// Get the Keyboard interface for this page.
  #[must_use]
  pub fn keyboard(&self) -> Keyboard<'_> {
    Keyboard { page: self }
  }

  /// Get the Mouse interface for this page.
  #[must_use]
  pub fn mouse(&self) -> Mouse<'_> {
    Mouse { page: self }
  }

  /// Get the Touchscreen interface for this page.
  #[must_use]
  pub fn touchscreen(&self) -> Touchscreen<'_> {
    Touchscreen { page: self }
  }

  // ── Screencast (video recording) ──

  /// Start CDP screencast. Returns a channel of `(jpeg_bytes, timestamp_secs)` pairs.
  /// Timestamp is Chrome's `metadata.timestamp` from the screencastFrame event.
  ///
  /// # Errors
  ///
  /// Returns an error if screencast cannot be started on the backend.
  pub async fn start_screencast(
    &self,
    quality: u8,
    max_width: u32,
    max_height: u32,
  ) -> Result<tokio::sync::mpsc::UnboundedReceiver<(Vec<u8>, f64)>, String> {
    self.inner.start_screencast(quality, max_width, max_height).await
  }

  /// Stop CDP screencast.
  ///
  /// # Errors
  ///
  /// Returns an error if screencast cannot be stopped on the backend.
  pub async fn stop_screencast(&self) -> Result<(), String> {
    self.inner.stop_screencast().await
  }
}

impl std::fmt::Debug for Page {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Page").finish()
  }
}

// ── Keyboard ──────────────────────────────────────────────────────────────

/// Keyboard interface for a page. Mirrors Playwright's `page.keyboard`.
pub struct Keyboard<'a> {
  page: &'a Page,
}

impl Keyboard<'_> {
  /// Dispatch a keyDown event. The key is held until `up()` is called.
  ///
  /// Supports modifier keys: "Shift", "Control", "Alt", "Meta".
  /// Holding Shift will type uppercase text via subsequent `press()` or `type()` calls.
  ///
  /// # Errors
  ///
  /// Returns an error if the key down dispatch fails.
  pub async fn down(&self, key: &str) -> Result<(), String> {
    self.page.key_down(key).await
  }

  /// Dispatch a keyUp event for a previously held key.
  ///
  /// # Errors
  ///
  /// Returns an error if the key up dispatch fails.
  pub async fn up(&self, key: &str) -> Result<(), String> {
    self.page.key_up(key).await
  }

  /// Press a key or key combination (e.g., "Enter", "Control+a", "Shift+ArrowDown").
  ///
  /// Shortcut for `down(key)` followed by `up(key)`. Supports `+` combinator for
  /// modifier combinations.
  ///
  /// # Errors
  ///
  /// Returns an error if the key press dispatch fails.
  pub async fn press(&self, key: &str) -> Result<(), String> {
    self.page.press_key(key).await
  }

  /// Type text character by character with full keyboard events.
  ///
  /// Sends `keydown`, `keypress`/`input`, and `keyup` events for each character
  /// in the text. For characters not representable as single key presses,
  /// falls back to `insert_text` for that character.
  ///
  /// # Errors
  ///
  /// Returns an error if the typing dispatch fails.
  pub async fn r#type(&self, text: &str) -> Result<(), String> {
    for ch in text.chars() {
      let s = ch.to_string();
      // Single printable ASCII characters and common keys get full key events
      self.page.press_key(&s).await?;
    }
    Ok(())
  }

  /// Insert text directly without emitting keyboard events.
  ///
  /// Only dispatches an `input` event. Modifier keys do NOT affect `insert_text`.
  /// Useful for inserting characters not available on a US keyboard layout.
  ///
  /// # Errors
  ///
  /// Returns an error if the text insertion fails.
  pub async fn insert_text(&self, text: &str) -> Result<(), String> {
    self.page.inner.insert_text(text).await
  }
}

// ── Mouse ─────────────────────────────────────────────────────────────────

/// Mouse interface for a page. Mirrors Playwright's `page.mouse`.
pub struct Mouse<'a> {
  page: &'a Page,
}

impl Mouse<'_> {
  /// Click at coordinates.
  ///
  /// # Errors
  ///
  /// Returns an error if the click dispatch fails.
  pub async fn click(&self, x: f64, y: f64, opts: Option<MouseClickOptions>) -> Result<(), String> {
    let button = opts.as_ref().and_then(|o| o.button.as_deref()).unwrap_or("left");
    let count = opts.as_ref().and_then(|o| o.click_count).unwrap_or(1);
    self.page.click_at_opts(x, y, button, count).await
  }

  /// Move mouse to coordinates.
  ///
  /// # Errors
  ///
  /// Returns an error if the mouse move dispatch fails.
  pub async fn r#move(&self, x: f64, y: f64, steps: Option<u32>) -> Result<(), String> {
    match steps {
      Some(step_count) => {
        let (from_x, from_y) = *self
          .page
          .mouse_position
          .lock()
          .map_err(|e| format!("mouse position lock poisoned: {e}"))?;
        self.page.move_mouse_smooth(from_x, from_y, x, y, step_count).await
      },
      None => self.page.move_mouse(x, y).await,
    }
  }

  /// Double-click at coordinates.
  ///
  /// # Errors
  ///
  /// Returns an error if the click dispatch fails.
  pub async fn dblclick(&self, x: f64, y: f64, opts: Option<MouseClickOptions>) -> Result<(), String> {
    let button = opts.as_ref().and_then(|o| o.button.as_deref()).unwrap_or("left");
    self.page.move_mouse(x, y).await?;
    self.page.mouse_down(x, y, button).await?;
    self.page.mouse_up(x, y, button).await?;
    self.page.mouse_down(x, y, button).await?;
    self.page.mouse_up(x, y, button).await?;
    Ok(())
  }

  /// Press mouse button down at the current cursor position.
  ///
  /// # Errors
  ///
  /// Returns an error if the mouse down dispatch fails.
  pub async fn down(&self, opts: Option<MouseDownOptions>) -> Result<(), String> {
    let button = opts.as_ref().and_then(|o| o.button.as_deref()).unwrap_or("left");
    let (x, y) = *self
      .page
      .mouse_position
      .lock()
      .map_err(|e| format!("mouse position lock poisoned: {e}"))?;
    self.page.mouse_down(x, y, button).await
  }

  /// Release mouse button at the current cursor position.
  ///
  /// # Errors
  ///
  /// Returns an error if the mouse up dispatch fails.
  pub async fn up(&self, opts: Option<MouseUpOptions>) -> Result<(), String> {
    let button = opts.as_ref().and_then(|o| o.button.as_deref()).unwrap_or("left");
    let (x, y) = *self
      .page
      .mouse_position
      .lock()
      .map_err(|e| format!("mouse position lock poisoned: {e}"))?;
    self.page.mouse_up(x, y, button).await
  }

  /// Scroll via mouse wheel.
  ///
  /// # Errors
  ///
  /// Returns an error if the wheel event dispatch fails.
  pub async fn wheel(&self, delta_x: f64, delta_y: f64) -> Result<(), String> {
    self.page.mouse_wheel(delta_x, delta_y).await
  }
}

/// Options for `Mouse.click()`.
#[derive(Debug, Clone, Default)]
pub struct MouseClickOptions {
  /// Mouse button: "left", "right", "middle"
  pub button: Option<String>,
  /// Click count (1=single, 2=double, 3=triple)
  pub click_count: Option<u32>,
}

/// Options for `Mouse.down()`.
#[derive(Debug, Clone, Default)]
pub struct MouseDownOptions {
  /// Mouse button: "left", "right", "middle"
  pub button: Option<String>,
  /// Click count for the event
  pub click_count: Option<u32>,
}

/// Options for `Mouse.up()`.
#[derive(Debug, Clone, Default)]
pub struct MouseUpOptions {
  /// Mouse button: "left", "right", "middle"
  pub button: Option<String>,
  /// Click count for the event
  pub click_count: Option<u32>,
}

// ── Touchscreen ───────────────────────────────────────────────────────────

/// Touchscreen interface for a page. Mirrors Playwright's `page.touchscreen`.
pub struct Touchscreen<'a> {
  page: &'a Page,
}

impl Touchscreen<'_> {
  /// Tap at coordinates. Uses Touch/TouchEvent on platforms that support them,
  /// falls back to `PointerEvent` + click on desktop (e.g. macOS `WKWebView`).
  ///
  /// # Errors
  ///
  /// Returns an error if the tap event dispatch fails.
  pub async fn tap(&self, x: f64, y: f64) -> Result<(), String> {
    self.page.inner.evaluate(&format!(
      "(function(){{var el=document.elementFromPoint({x},{y})||document.body;\
       if(typeof Touch!=='undefined'&&typeof TouchEvent!=='undefined'){{\
         var t=new Touch({{identifier:1,target:el,clientX:{x},clientY:{y}}});\
         el.dispatchEvent(new TouchEvent('touchstart',{{touches:[t],changedTouches:[t],bubbles:true}}));\
         el.dispatchEvent(new TouchEvent('touchend',{{touches:[],changedTouches:[t],bubbles:true}}));\
       }}else{{\
         el.dispatchEvent(new PointerEvent('pointerdown',{{clientX:{x},clientY:{y},bubbles:true,isPrimary:true,pointerType:'touch'}}));\
         el.dispatchEvent(new PointerEvent('pointerup',{{clientX:{x},clientY:{y},bubbles:true,isPrimary:true,pointerType:'touch'}}));\
         el.click();\
       }}}})()"
    )).await?;
    Ok(())
  }
}
