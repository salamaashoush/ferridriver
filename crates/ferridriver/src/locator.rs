//! Lazy element locator -- mirrors Playwright's Locator interface.
//!
//! A Locator stores a selector string and a reference to its Page.
//! It does NOT query the DOM when created. Resolution happens lazily
//! when an action method (click, fill, etc.) is called.
//!
//! Locators can be chained to narrow scope:
//! ```ignore
//! page.locator("css=.form").get_by_role("textbox", &Default::default()).fill("hello").await?;
//! ```

use std::fmt::Write as _;
use std::sync::Arc;

use crate::actions;
use crate::backend::AnyElement;
use crate::error::Result;
use crate::options::{BoundingBox, FilterOptions, RoleOptions, TextOptions, WaitOptions};
use crate::selectors;

/// Zero-cost retry macro that resolves an element with backoff, then runs an
/// action body inline. Provides `$el: AnyElement` and `$page: &AnyPage` to the
/// body without any `AnyPage` cloning — the page reference is borrowed from `self`
/// for the entire retry loop.
///
/// The body must be an `async move { ... }` block returning `Result<R, String>`
/// (the underlying backend error type). The macro converts every error into
/// [`crate::error::FerriError`] so call sites declare `-> crate::error::Result<R>`.
/// References to outer variables (parameters, locals) are captured by copy for
/// `Copy` types (like `&str`, `&Arc<Page>`) or by move for owned types.
macro_rules! retry_resolve {
  ($self:expr, |$el:ident, $page:ident| $body:expr) => {{
    let $page: &$crate::backend::AnyPage = $self.page.inner();
    $page
      .ensure_engine_injected()
      .await
      .map_err($crate::error::FerriError::from)?;
    let __fd = "window.__fd";
    let __sel_js =
      $crate::selectors::build_selone_js(&$self.selector, &__fd).map_err($crate::error::FerriError::from)?;

    for (__i, &__delay_ms) in Locator::RETRY_BACKOFFS_MS.iter().enumerate() {
      if __delay_ms > 0 {
        ::tokio::time::sleep(::std::time::Duration::from_millis(__delay_ms)).await;
      }
      match $crate::selectors::query_one_prebuilt($page, &__sel_js, &$self.selector).await {
        ::std::result::Result::Ok($el) => match ($body).await {
          ::std::result::Result::Ok(val) => return ::std::result::Result::Ok(val),
          ::std::result::Result::Err(e)
            if e.contains("not connected") || e.contains("not found") || e.contains("detached") =>
          {
            if __i >= Locator::RETRY_BACKOFFS_MS.len() - 1 {
              return ::std::result::Result::Err($crate::error::FerriError::from(e));
            }
          },
          ::std::result::Result::Err(e) => return ::std::result::Result::Err($crate::error::FerriError::from(e)),
        },
        ::std::result::Result::Err(_) if __i < Locator::RETRY_BACKOFFS_MS.len() - 1 => {},
        ::std::result::Result::Err(e) => return ::std::result::Result::Err($crate::error::FerriError::from(e)),
      }
    }
    ::std::result::Result::Err($crate::error::FerriError::invalid_selector(
      $self.selector.clone(),
      "no element found",
    ))
  }};
}

/// A lazy element locator. Does not query the DOM until an action is called.
/// Holds `Arc<Page>` instead of owned Page — locator chains are just atomic
/// refcount bumps, not full Page clones.
#[derive(Clone)]
pub struct Locator {
  pub(crate) page: Arc<crate::page::Page>,
  pub(crate) selector: String,
  /// If set, evaluate in this frame instead of the main frame.
  /// Uses `Arc<str>` so that chaining locators only bumps a refcount.
  pub(crate) frame_id: Option<Arc<str>>,
}

impl Locator {
  // ── Sub-locators (chain with >>) ──────────────────────────────────────────

  /// Narrow this locator's scope with an additional selector.
  #[must_use]
  pub fn locator(&self, selector: &str) -> Locator {
    self.chain(selector)
  }

  /// Locate elements by ARIA role, optionally filtered by role options.
  #[must_use]
  pub fn get_by_role(&self, role: &str, opts: &RoleOptions) -> Locator {
    self.chain(&build_role_selector(role, opts))
  }

  /// Locate elements by visible text content.
  #[must_use]
  pub fn get_by_text(&self, text: &str, opts: &TextOptions) -> Locator {
    self.chain(&build_text_selector("text", text, opts))
  }

  /// Locate form elements by their associated label text.
  #[must_use]
  pub fn get_by_label(&self, text: &str, opts: &TextOptions) -> Locator {
    self.chain(&build_text_selector("label", text, opts))
  }

  /// Locate input elements by their placeholder text.
  #[must_use]
  pub fn get_by_placeholder(&self, text: &str, opts: &TextOptions) -> Locator {
    self.chain(&build_text_selector("placeholder", text, opts))
  }

  /// Locate elements by their `alt` attribute text.
  #[must_use]
  pub fn get_by_alt_text(&self, text: &str, opts: &TextOptions) -> Locator {
    self.chain(&build_text_selector("alt", text, opts))
  }

  /// Locate elements by their `title` attribute text.
  #[must_use]
  pub fn get_by_title(&self, text: &str, opts: &TextOptions) -> Locator {
    self.chain(&build_text_selector("title", text, opts))
  }

  #[must_use]
  pub fn get_by_test_id(&self, test_id: &str) -> Locator {
    self.chain(&format!("testid={test_id}"))
  }

  #[must_use]
  pub fn first(&self) -> Locator {
    self.chain("nth=0")
  }

  #[must_use]
  pub fn last(&self) -> Locator {
    self.chain("nth=-1")
  }

  #[must_use]
  pub fn nth(&self, index: i32) -> Locator {
    self.chain(&format!("nth={index}"))
  }

  /// Filter this locator by text content, sub-selector presence, or absence.
  #[must_use]
  pub fn filter(&self, opts: &FilterOptions) -> Locator {
    use std::fmt::Write as _;

    // Build the combined filter suffix in one buffer, then chain once.
    // Avoids: self.clone() + up to 4 intermediate chain allocations.
    let mut suffix = String::new();
    if let Some(text) = &opts.has_text {
      let _ = write!(suffix, "has-text={text}");
    }
    if let Some(text) = &opts.has_not_text {
      if !suffix.is_empty() {
        suffix.push_str(" >> ");
      }
      let _ = write!(suffix, "has-not-text={text}");
    }
    if let Some(sel) = &opts.has {
      if !suffix.is_empty() {
        suffix.push_str(" >> ");
      }
      let _ = write!(suffix, "has={sel}");
    }
    if let Some(sel) = &opts.has_not {
      if !suffix.is_empty() {
        suffix.push_str(" >> ");
      }
      let _ = write!(suffix, "has-not={sel}");
    }
    if suffix.is_empty() {
      self.clone()
    } else {
      self.chain(&suffix)
    }
  }

  // ── Actions ───────────────────────────────────────────────────────────────
  //
  // All action methods use the `retry_resolve!` macro which:
  //   1. Pre-builds selector JS once (no re-parsing per retry)
  //   2. Borrows `&AnyPage` from self — zero AnyPage clones
  //   3. Borrows `&str` parameters directly — zero String clones
  //   4. Expands inline — no closure/future type-erasure overhead

  /// Click the element matched by this locator.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found, is not actionable
  /// (e.g. a `<select>` or file input), or the click fails.
  pub async fn click(&self) -> Result<()> {
    retry_resolve!(self, |el, page| async move {
      actions::check_click_guard(&el, page).await.map_err(|e| e.to_string())?;
      actions::wait_for_actionable(&el, page).await.ok();
      el.click().await
    })
  }

  /// Double-click the element matched by this locator.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or the double-click fails.
  pub async fn dblclick(&self) -> Result<()> {
    retry_resolve!(self, |el, page| async move {
      actions::wait_for_actionable(&el, page).await.ok();
      el.dblclick().await
    })
  }

  /// Right-click (context menu click) on the element.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found, its bounding box
  /// cannot be computed, or the right-click dispatch fails.
  pub async fn right_click(&self) -> Result<()> {
    retry_resolve!(self, |el, page| async move {
      let center = el.call_js_fn_value(
        "function() { this.scrollIntoViewIfNeeded(); var r = this.getBoundingClientRect(); return {x: r.x + r.width/2, y: r.y + r.height/2}; }"
      ).await?;
      if let Some(c) = center {
        let x = c.get("x").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
        let y = c.get("y").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
        page.click_at_opts(x, y, "right", 1).await?;
      }
      Ok::<(), String>(())
    })
  }

  /// Fill an input or textarea element with the given value.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or is not a fillable element.
  pub async fn fill(&self, value: &str) -> Result<()> {
    retry_resolve!(self, |el, _page| async move { actions::fill(&el, value).await })
  }

  /// Clear the value of an input or textarea element.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found.
  pub async fn clear(&self) -> Result<()> {
    retry_resolve!(self, |el, _page| async move {
      el.call_js_fn(
        "function() { \
        if (window.__fd) window.__fd.clearAndDispatch(this); \
        else { this.value = ''; } \
      }",
      )
      .await?;
      Ok::<(), String>(())
    })
  }

  /// Type text into the element character by character using keyboard events.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or key dispatch fails.
  pub async fn r#type(&self, text: &str) -> Result<()> {
    retry_resolve!(self, |el, page| async move {
      actions::wait_for_actionable(&el, page).await.ok();
      el.type_str(text).await
    })
  }

  /// Press a key or key combination (e.g. "Enter", "Control+a").
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or the key press fails.
  pub async fn press(&self, key: &str) -> Result<()> {
    retry_resolve!(self, |_el, page| async move { page.press_key(key).await })
  }

  /// Hover over the element matched by this locator.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or the hover action fails.
  pub async fn hover(&self) -> Result<()> {
    retry_resolve!(self, |el, page| async move {
      actions::wait_for_actionable(&el, page).await.ok();
      el.hover().await
    })
  }

  /// Focus the element matched by this locator.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found.
  pub async fn focus(&self) -> Result<()> {
    retry_resolve!(self, |el, _page| async move {
      el.call_js_fn("function() { this.focus(); }").await?;
      Ok::<(), String>(())
    })
  }

  /// Check a checkbox or radio button if it is not already checked.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or is not actionable.
  pub async fn check(&self) -> Result<()> {
    retry_resolve!(self, |el, page| async move {
      actions::wait_for_actionable(&el, page).await.ok();
      el.call_js_fn("function() { if (!this.checked) this.click(); }").await?;
      Ok::<(), String>(())
    })
  }

  /// Uncheck a checkbox if it is currently checked.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or is not actionable.
  pub async fn uncheck(&self) -> Result<()> {
    retry_resolve!(self, |el, page| async move {
      actions::wait_for_actionable(&el, page).await.ok();
      el.call_js_fn("function() { if (this.checked) this.click(); }").await?;
      Ok::<(), String>(())
    })
  }

  /// Set the checked state of a checkbox or radio button.
  /// If `checked` is true, checks it. If false, unchecks it.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or is not actionable.
  pub async fn set_checked(&self, checked: bool) -> Result<()> {
    if checked {
      self.check().await
    } else {
      self.uncheck().await
    }
  }

  /// Tap the element (touch event). Dispatches touchstart + touchend on platforms
  /// that support Touch/TouchEvent APIs, falls back to pointerdown + pointerup + click
  /// on desktop browsers (e.g. macOS `WKWebView`) where Touch constructors are unavailable.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or the tap event dispatch fails.
  pub async fn tap(&self) -> Result<()> {
    let el = self.resolve().await?;
    actions::wait_for_actionable(&el, self.page.inner()).await.ok();
    el.call_js_fn("function() { \
      this.scrollIntoViewIfNeeded && this.scrollIntoViewIfNeeded(); \
      var r = this.getBoundingClientRect(); \
      var cx = r.left + r.width/2, cy = r.top + r.height/2; \
      if (typeof Touch !== 'undefined' && typeof TouchEvent !== 'undefined') { \
        var t = new Touch({identifier:1,target:this,clientX:cx,clientY:cy}); \
        this.dispatchEvent(new TouchEvent('touchstart',{touches:[t],changedTouches:[t],bubbles:true})); \
        this.dispatchEvent(new TouchEvent('touchend',{touches:[],changedTouches:[t],bubbles:true})); \
      } else { \
        this.dispatchEvent(new PointerEvent('pointerdown',{clientX:cx,clientY:cy,bubbles:true,isPrimary:true,pointerType:'touch'})); \
        this.dispatchEvent(new PointerEvent('pointerup',{clientX:cx,clientY:cy,bubbles:true,isPrimary:true,pointerType:'touch'})); \
        this.click(); \
      } \
    }").await.map_err(Into::into)
  }

  /// Select all text in an input or textarea element.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or the selection fails.
  pub async fn select_text(&self) -> Result<()> {
    let el = self.resolve().await?;
    el.call_js_fn(
      "function() { \
      this.focus(); \
      if (this.select) { this.select(); } \
      else if (this.setSelectionRange) { this.setSelectionRange(0, this.value ? this.value.length : 0); } \
    }",
    )
    .await
    .map_err(Into::into)
  }

  /// Select an `<option>` by value within a `<select>` element.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or is not a `<select>`.
  pub async fn select_option(&self, value: &str) -> Result<Vec<String>> {
    let el = self.resolve().await?;
    let result = actions::select_option(&el, self.page.inner(), value).await?;
    Ok(vec![result.selected_value])
  }

  // (select_option reserved for future ElementHandle / SelectOption array overloads per task #5.)

  /// Set file paths on a file input element.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not a file input or the upload fails.
  pub async fn set_input_files(&self, paths: &[String]) -> Result<()> {
    actions::upload_file(self.page.inner(), &self.selector, paths)
      .await
      .map_err(Into::into)
  }

  /// Scroll the element into the visible area of the viewport.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or scroll fails.
  pub async fn scroll_into_view_if_needed(&self) -> Result<()> {
    let el = self.resolve().await?;
    el.scroll_into_view().await.map_err(Into::into)
  }

  /// Dispatch a DOM event of the given type on the element.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found.
  pub async fn dispatch_event(&self, event_type: &str) -> Result<()> {
    let el = self.resolve().await?;
    let _ = el
      .call_js_fn(&format!(
        "function() {{ this.dispatchEvent(new Event('{event_type}', {{bubbles: true}})); }}"
      ))
      .await;
    Ok(())
  }

  // ── Content & state ───────────────────────────────────────────────────────

  /// Return the `textContent` of the element, or `None` if not found.
  ///
  /// # Errors
  ///
  /// Returns an error if selector parsing or JS evaluation fails.
  pub async fn text_content(&self) -> Result<Option<String>> {
    self.eval_prop("textContent").await
  }

  /// Return the `innerText` of the element.
  ///
  /// # Errors
  ///
  /// Returns an error if selector parsing or JS evaluation fails.
  pub async fn inner_text(&self) -> Result<String> {
    self
      .eval_prop("innerText")
      .await
      .map(std::option::Option::unwrap_or_default)
  }

  /// Return the `innerHTML` of the element.
  ///
  /// # Errors
  ///
  /// Returns an error if selector parsing or JS evaluation fails.
  pub async fn inner_html(&self) -> Result<String> {
    self
      .eval_prop("innerHTML")
      .await
      .map(std::option::Option::unwrap_or_default)
  }

  /// Get the value of an attribute on the element.
  ///
  /// # Errors
  ///
  /// Returns an error if selector parsing or JS evaluation fails.
  pub async fn get_attribute(&self, name: &str) -> Result<Option<String>> {
    let escaped = name.replace('\\', "\\\\").replace('\'', "\\'");
    let val = self
      .eval_on_element(&format!("return el.getAttribute('{escaped}');"))
      .await?;
    Ok(val.and_then(|v| match v {
      serde_json::Value::String(s) => Some(s),
      serde_json::Value::Null => None,
      other => Some(other.to_string()),
    }))
  }

  /// Return the `value` property of an input or textarea element.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or JS evaluation fails.
  pub async fn input_value(&self) -> Result<String> {
    self
      .eval_prop("value")
      .await
      .map(std::option::Option::unwrap_or_default)
  }

  /// Check whether the element is visible (not `display:none`, `visibility:hidden`,
  /// or `opacity:0`). Returns `false` if the element does not exist.
  ///
  /// # Errors
  ///
  /// Returns an error if selector parsing or JS evaluation fails.
  pub async fn is_visible(&self) -> Result<bool> {
    // Single evaluate: find element + check visibility. Returns false if not found.
    let val = self
      .eval_on_element(
        "var s = getComputedStyle(el); \
       return s.display !== 'none' && s.visibility !== 'hidden' && s.opacity !== '0';",
      )
      .await?;
    // eval_on_element returns null if element not found -> false (Playwright behavior)
    Ok(val.and_then(|v| v.as_bool()).unwrap_or(false))
  }

  /// Check whether the element is hidden (inverse of `is_visible`).
  ///
  /// # Errors
  ///
  /// Returns an error if selector parsing or JS evaluation fails.
  pub async fn is_hidden(&self) -> Result<bool> {
    self.is_visible().await.map(|v| !v)
  }

  /// Check whether the element is enabled (i.e. not `disabled`).
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or JS evaluation fails.
  pub async fn is_enabled(&self) -> Result<bool> {
    self.eval_bool("function() { return !this.disabled; }").await
  }

  /// Check whether the element is disabled.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or JS evaluation fails.
  pub async fn is_disabled(&self) -> Result<bool> {
    self.eval_bool("function() { return !!this.disabled; }").await
  }

  /// Check whether a checkbox or radio button is checked.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or JS evaluation fails.
  pub async fn is_checked(&self) -> Result<bool> {
    self.eval_bool("function() { return !!this.checked; }").await
  }

  /// Check if the element is attached to the DOM (exists in the document).
  ///
  /// # Errors
  ///
  /// Returns an error if selector parsing fails.
  pub async fn is_attached(&self) -> Result<bool> {
    Ok(self.resolve().await.is_ok())
  }

  /// Count the number of elements matching this locator's selector.
  ///
  /// # Errors
  ///
  /// Returns an error if selector parsing or JS evaluation fails.
  pub async fn count(&self) -> Result<usize> {
    let parsed = selectors::parse(&self.selector)?;
    let parts_json = selectors::build_parts_json(&parsed);
    let fd = self.page.inner().injected_script().await?;
    let js = format!("{fd}.selCount({parts_json})");
    let val = self
      .page
      .inner()
      .evaluate(&js)
      .await?
      .and_then(|v| v.as_u64())
      .unwrap_or(0);
    Ok(usize::try_from(val).unwrap_or(usize::MAX))
  }

  /// Return the bounding box of the element, or `None` if the element is not found.
  ///
  /// # Errors
  ///
  /// Returns an error if selector parsing or JS evaluation fails.
  pub async fn bounding_box(&self) -> Result<Option<BoundingBox>> {
    let val = self
      .eval_on_element("var r = el.getBoundingClientRect(); return {x:r.x,y:r.y,width:r.width,height:r.height};")
      .await?;
    match val {
      Some(v) => Ok(Some(BoundingBox {
        x: v["x"].as_f64().unwrap_or(0.0),
        y: v["y"].as_f64().unwrap_or(0.0),
        width: v["width"].as_f64().unwrap_or(0.0),
        height: v["height"].as_f64().unwrap_or(0.0),
      })),
      None => Ok(None),
    }
  }

  // ── Waiting ───────────────────────────────────────────────────────────────

  /// Wait for the element to reach the specified state ("visible", "hidden",
  /// "attached", or "detached").
  ///
  /// # Errors
  ///
  /// Returns an error if the timeout expires before the element reaches
  /// the desired state, or if an unknown state is specified.
  pub async fn wait_for(&self, opts: WaitOptions) -> Result<()> {
    let timeout = opts.timeout.unwrap_or(30000);
    let state = opts.state.as_deref().unwrap_or("visible");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout);

    loop {
      if tokio::time::Instant::now() >= deadline {
        return Err(crate::error::FerriError::timeout(
          format!("waiting for '{}' to be {state}", self.selector),
          timeout,
        ));
      }
      match state {
        "attached" | "visible" => {
          if selectors::query_one(self.page.inner(), &self.selector, false)
            .await
            .is_ok()
          {
            selectors::cleanup_tags(self.page.inner()).await;
            return Ok(());
          }
        },
        "hidden" | "detached" => {
          if selectors::query_one(self.page.inner(), &self.selector, false)
            .await
            .is_err()
          {
            return Ok(());
          }
          selectors::cleanup_tags(self.page.inner()).await;
        },
        _ => {
          return Err(crate::error::FerriError::invalid_argument(
            "state",
            format!("unknown wait state: {state}"),
          ));
        },
      }
      tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
  }

  // ── Screenshot ────────────────────────────────────────────────────────────

  /// Take a PNG screenshot of the element.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or screenshot capture fails.
  pub async fn screenshot(&self) -> Result<Vec<u8>> {
    let el = self.resolve().await?;
    el.screenshot(crate::backend::ImageFormat::Png)
      .await
      .map_err(Into::into)
  }

  // ── Editable check ───────────────────────────────────────────────────────

  /// Check whether the element is editable (not disabled and not read-only).
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or JS evaluation fails.
  pub async fn is_editable(&self) -> Result<bool> {
    self
      .eval_bool("function() { return !this.disabled && !this.readOnly; }")
      .await
  }

  // ── Blur ────────────────────────────────────────────────────────────────

  /// Remove focus from the element.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found.
  pub async fn blur(&self) -> Result<()> {
    let el = self.resolve().await?;
    let _ = el.call_js_fn("function() { this.blur(); }").await;
    Ok(())
  }

  // ── Press sequentially ──────────────────────────────────────────────────

  /// Type text character by character with a delay between each.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or any key press fails.
  pub async fn press_sequentially(&self, text: &str, delay_ms: Option<u64>) -> Result<()> {
    let el = self.resolve().await?;
    actions::wait_for_actionable(&el, self.page.inner()).await.ok();
    let delay = delay_ms.unwrap_or(50);
    for ch in text.chars() {
      self.page.inner().press_key(&ch.to_string()).await?;
      if delay > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
      }
    }
    Ok(())
  }

  // ── Drag to another locator ─────────────────────────────────────────────

  /// Drag this element to the target locator's element.
  ///
  /// # Errors
  ///
  /// Returns an error if either element cannot be found, bounding box
  /// coordinates cannot be read, or the drag operation fails.
  pub async fn drag_to(&self, target: &Locator) -> Result<()> {
    // Get both source and target center coordinates via call_js_fn_value (1 CDP each)
    let source_el = self.resolve().await?;
    let target_el = target.resolve().await?;

    // Parallel: get both centers simultaneously
    let (src_result, tgt_result) = tokio::join!(
      source_el.call_js_fn_value(
        "function() { this.scrollIntoViewIfNeeded(); var r = this.getBoundingClientRect(); return {x: r.x + r.width/2, y: r.y + r.height/2}; }"
      ),
      target_el.call_js_fn_value(
        "function() { this.scrollIntoViewIfNeeded(); var r = this.getBoundingClientRect(); return {x: r.x + r.width/2, y: r.y + r.height/2}; }"
      ),
    );

    let src = src_result?.ok_or_else(|| crate::error::FerriError::Other("no source bounding box".into()))?;
    let tgt = tgt_result?.ok_or_else(|| crate::error::FerriError::Other("no target bounding box".into()))?;

    let from = (
      src.get("x").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
      src.get("y").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
    );
    let to = (
      tgt.get("x").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
      tgt.get("y").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
    );
    self.page.inner().click_and_drag(from, to).await.map_err(Into::into)
  }

  // ── Combinators ─────────────────────────────────────────────────────────

  /// Union: matches elements from either this or the other locator.
  /// Creates a new locator that matches elements found by either selector.
  /// For CSS selectors, uses `:is(a, b)`. For rich selectors, falls back to
  /// trying both selectors in order.
  #[must_use]
  pub fn or(&self, other: &Locator) -> Locator {
    let is_css_a = !selectors::is_rich_selector(&self.selector);
    let is_css_b = !selectors::is_rich_selector(&other.selector);

    let combined = if is_css_a && is_css_b {
      // Both are CSS -- use :is() for a proper CSS OR
      format!(
        "css=:is({}, {})",
        self.selector.strip_prefix("css=").unwrap_or(&self.selector),
        other.selector.strip_prefix("css=").unwrap_or(&other.selector)
      )
    } else {
      // At least one is a rich selector -- combine with | operator
      // This is handled by the selector engine's _exec
      format!("{} | {}", self.selector, other.selector)
    };
    Locator {
      page: self.page.clone(),
      selector: combined,
      frame_id: self.frame_id.clone(),
    }
  }

  /// Intersection: matches elements that match both locators.
  #[must_use]
  pub fn and(&self, other: &Locator) -> Locator {
    // Chain with >> which narrows scope
    self.chain(&other.selector)
  }

  // ── All matches ─────────────────────────────────────────────────────────

  /// Return all matching locators as individual Locator instances.
  ///
  /// # Errors
  ///
  /// Returns an error if the count query fails due to selector parsing
  /// or JS evaluation errors.
  pub async fn all(&self) -> Result<Vec<Locator>> {
    let count = self.count().await?;
    let mut locators = Vec::with_capacity(count);
    let base = &self.selector;
    for i in 0..count {
      let idx = i32::try_from(i).unwrap_or(i32::MAX);
      let selector = if base.is_empty() {
        format!("nth={idx}")
      } else {
        format!("{base} >> nth={idx}")
      };
      locators.push(Locator {
        page: Arc::clone(&self.page),
        selector,
        frame_id: self.frame_id.clone(),
      });
    }
    Ok(locators)
  }

  /// Get text content of all matching elements.
  ///
  /// # Errors
  ///
  /// Returns an error if selector parsing or JS evaluation fails.
  pub async fn all_text_contents(&self) -> Result<Vec<String>> {
    let parsed = selectors::parse(&self.selector)?;
    let parts_json = selectors::build_parts_json(&parsed);
    self.page.inner().ensure_engine_injected().await?;
    let fd = "window.__fd";
    let js = format!(
      "(function() {{ var r = {fd}._exec({parts_json}, document); \
       return r.map(function(e) {{ return (e.textContent || '').trim(); }}); }})()"
    );
    let val = self.page.inner().evaluate(&js).await?;
    match val {
      Some(serde_json::Value::Array(arr)) => Ok(
        arr
          .into_iter()
          .filter_map(|v| v.as_str().map(std::string::ToString::to_string))
          .collect(),
      ),
      _ => Ok(Vec::new()),
    }
  }

  /// Get inner text of all matching elements.
  ///
  /// # Errors
  ///
  /// Returns an error if selector parsing or JS evaluation fails.
  pub async fn all_inner_texts(&self) -> Result<Vec<String>> {
    // Same as all_text_contents for our implementation
    self.all_text_contents().await
  }

  // ── Evaluate ────────────────────────────────────────────────────────────

  /// Evaluate a JS expression with the first matching element as `el`.
  /// The expression should return a value.
  ///
  /// ```ignore
  /// let tag = locator.evaluate("el.tagName").await?;
  /// ```
  ///
  /// # Errors
  ///
  /// Returns an error if selector parsing or JS evaluation fails.
  pub async fn evaluate(&self, expression: &str) -> Result<Option<serde_json::Value>> {
    self.eval_on_element(&format!("return ({expression});")).await
  }

  /// Evaluate a JS expression with ALL matching elements as `elements` array.
  /// The expression should return a value.
  ///
  /// ```ignore
  /// let count = locator.evaluate_all("elements.length").await?;
  /// let texts = locator.evaluate_all("elements.map(e => e.textContent)").await?;
  /// ```
  ///
  /// # Errors
  ///
  /// Returns an error if selector parsing or JS evaluation fails.
  pub async fn evaluate_all(&self, expression: &str) -> Result<Option<serde_json::Value>> {
    let parsed = selectors::parse(&self.selector)?;
    let parts_json = selectors::build_parts_json(&parsed);
    self.page.inner().ensure_engine_injected().await?;
    let fd = "window.__fd";
    let js = format!("(function() {{ var elements = {fd}.selAll({parts_json}); return ({expression}); }})()");
    if let Some(fid) = &self.frame_id {
      self.page.inner().evaluate_in_frame(&js, fid).await.map_err(Into::into)
    } else {
      self.page.inner().evaluate(&js).await.map_err(Into::into)
    }
  }

  // ── Page / Frame access ────────────────────────────────────────────────────

  /// Get the page this locator belongs to.
  #[must_use]
  pub fn page(&self) -> &Arc<crate::page::Page> {
    &self.page
  }

  /// Treat this locator as an `<iframe>` and return a `FrameLocator` for its content.
  ///
  /// Equivalent to Playwright's `locator.contentFrame()`. The returned
  /// `FrameLocator` creates locators scoped to the iframe's content document.
  #[must_use]
  pub fn content_frame(&self) -> FrameLocator {
    FrameLocator {
      page: Arc::clone(&self.page),
      iframe_selector: self.selector.clone(),
    }
  }

  /// Create a `FrameLocator` targeting an `<iframe>` matched by `selector` within
  /// this locator's scope.
  ///
  /// Equivalent to Playwright's `locator.frameLocator(selector)`.
  #[must_use]
  pub fn frame_locator(&self, selector: &str) -> FrameLocator {
    FrameLocator {
      page: Arc::clone(&self.page),
      iframe_selector: if self.selector.is_empty() {
        selector.to_string()
      } else {
        format!("{} >> {selector}", self.selector)
      },
    }
  }

  // ── Selector access ───────────────────────────────────────────────────────

  #[must_use]
  pub fn selector(&self) -> &str {
    &self.selector
  }

  // ── Core retry system ─────────────────────────────────────────────────────
  //
  // Matches Playwright's retryWithProgressAndTimeouts + _retryWithProgressIfNotConnected
  // + _callOnElementOnceMatches. ALL element operations go through one of these two
  // methods. Retry backoff: [0, 20, 50, 100, 100, 500]ms (same as Playwright).

  /// Backoff schedule matching Playwright's retryWithProgressAndTimeouts.
  const RETRY_BACKOFFS_MS: &'static [u64] = &[0, 0, 20, 50, 100, 100, 500];

  /// Resolve element + run JS callback in ONE CDP call, with retry.
  /// Used by: innerText, textContent, innerHTML, getAttribute, inputValue, isVisible, etc.
  /// Matches Playwright's `_callOnElementOnceMatches`.
  async fn retry_eval_on_element(&self, js_body: &str) -> Result<Option<serde_json::Value>> {
    let parsed = selectors::parse(&self.selector)?;
    let parts_json = selectors::build_parts_json(&parsed);
    self.page.inner().ensure_engine_injected().await?;
    let fd = "window.__fd";
    let js = format!("(function() {{ var el = {fd}.selOne({parts_json}); if (!el) return null; {js_body} }})()");

    for (i, &delay_ms) in Self::RETRY_BACKOFFS_MS.iter().enumerate() {
      if delay_ms > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
      }
      let result: std::result::Result<Option<serde_json::Value>, String> = if let Some(fid) = &self.frame_id {
        self.page.inner().evaluate_in_frame(&js, fid).await
      } else {
        self.page.inner().evaluate(&js).await
      };
      match result {
        // Element not found or evaluation failed -- retry if attempts remain.
        Ok(Some(serde_json::Value::Null) | None) | Err(_) if i < Self::RETRY_BACKOFFS_MS.len() - 1 => {},
        Ok(val) => return Ok(val),
        Err(e) => return Err(e.into()),
      }
    }
    Ok(None)
  }

  // ── Internal helpers ────────────────────────────────────────────────────────

  /// Resolve the locator to a concrete element.
  ///
  /// # Errors
  ///
  /// Returns an error if the selector engine cannot be injected or the element is not found.
  pub async fn resolve(&self) -> Result<AnyElement> {
    self.page.inner().ensure_engine_injected().await?;
    let fd = "window.__fd";
    let sel_js = selectors::build_selone_js(&self.selector, fd)?;
    selectors::query_one_prebuilt(self.page.inner(), &sel_js, &self.selector)
      .await
      .map_err(Into::into)
  }

  fn chain(&self, sub: &str) -> Locator {
    let selector = if self.selector.is_empty() {
      sub.to_string()
    } else {
      format!("{} >> {sub}", self.selector)
    };
    Locator {
      page: Arc::clone(&self.page),
      selector,
      frame_id: self.frame_id.clone(),
    }
  }

  async fn eval_prop(&self, prop: &str) -> Result<Option<String>> {
    let val = self
      .retry_eval_on_element(&format!("var v = el.{prop}; return v == null ? null : String(v);"))
      .await?;
    Ok(val.and_then(|v| match v {
      serde_json::Value::String(s) => Some(s),
      serde_json::Value::Null => None,
      other => Some(other.to_string()),
    }))
  }

  async fn eval_bool(&self, func: &str) -> Result<bool> {
    let val = self
      .retry_eval_on_element(&format!("return !!({func}).call(el);"))
      .await?;
    Ok(val.and_then(|v| v.as_bool()).unwrap_or(false))
  }

  /// Legacy: non-retrying eval for callers that handle retry themselves.
  async fn eval_on_element(&self, js_body: &str) -> Result<Option<serde_json::Value>> {
    let parsed = selectors::parse(&self.selector)?;
    let parts_json = selectors::build_parts_json(&parsed);
    self.page.inner().ensure_engine_injected().await?;
    let fd = "window.__fd";
    let js = format!("(function() {{ var el = {fd}.selOne({parts_json}); if (!el) return null; {js_body} }})()");
    if let Some(fid) = &self.frame_id {
      self.page.inner().evaluate_in_frame(&js, fid).await.map_err(Into::into)
    } else {
      self.page.inner().evaluate(&js).await.map_err(Into::into)
    }
  }
}

impl std::fmt::Debug for Locator {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Locator")
      .field("selector", &self.selector)
      .field("frame_id", &self.frame_id)
      .finish_non_exhaustive()
  }
}

// ── FrameLocator ──────────────────────────────────────────────────────────────

/// A locator for content inside an `<iframe>`. Mirrors Playwright's `FrameLocator`.
///
/// Created via `page.frame_locator(selector)`, `locator.frame_locator(selector)`,
/// or `locator.content_frame()`. The `FrameLocator` resolves the iframe element
/// lazily when creating child locators, which then operate in the iframe's context.
#[derive(Clone)]
pub struct FrameLocator {
  page: Arc<crate::page::Page>,
  iframe_selector: String,
}

impl FrameLocator {
  /// Create a locator inside this iframe's content document.
  ///
  /// The returned locator resolves elements within the iframe. It first
  /// resolves the iframe element from `iframe_selector`, finds the corresponding
  /// frame ID, then evaluates the inner selector in that frame's context.
  ///
  /// # Errors
  ///
  /// Returns an error if the iframe cannot be found.
  pub async fn locator(&self, selector: &str) -> Result<Locator> {
    let frame_id = self.resolve_frame_id().await?;
    Ok(Locator {
      page: Arc::clone(&self.page),
      selector: selector.to_string(),
      frame_id: Some(frame_id),
    })
  }

  /// Locate by ARIA role inside this iframe.
  ///
  /// # Errors
  ///
  /// Returns an error if the iframe cannot be resolved.
  pub async fn get_by_role(&self, role: &str, opts: &RoleOptions) -> Result<Locator> {
    self.locator(&build_role_selector(role, opts)).await
  }

  /// Locate by visible text inside this iframe.
  ///
  /// # Errors
  ///
  /// Returns an error if the iframe cannot be resolved.
  pub async fn get_by_text(&self, text: &str, opts: &TextOptions) -> Result<Locator> {
    self.locator(&build_text_selector("text", text, opts)).await
  }

  /// Locate by test ID inside this iframe.
  ///
  /// # Errors
  ///
  /// Returns an error if the iframe cannot be resolved.
  pub async fn get_by_test_id(&self, test_id: &str) -> Result<Locator> {
    self.locator(&format!("testid={test_id}")).await
  }

  /// Locate by label text inside this iframe.
  ///
  /// # Errors
  ///
  /// Returns an error if the iframe cannot be resolved.
  pub async fn get_by_label(&self, text: &str, opts: &TextOptions) -> Result<Locator> {
    self.locator(&build_text_selector("label", text, opts)).await
  }

  /// Locate by placeholder text inside this iframe.
  ///
  /// # Errors
  ///
  /// Returns an error if the iframe cannot be resolved.
  pub async fn get_by_placeholder(&self, text: &str, opts: &TextOptions) -> Result<Locator> {
    self.locator(&build_text_selector("placeholder", text, opts)).await
  }

  /// Locate by alt text inside this iframe.
  ///
  /// # Errors
  ///
  /// Returns an error if the iframe cannot be resolved.
  pub async fn get_by_alt_text(&self, text: &str, opts: &TextOptions) -> Result<Locator> {
    self.locator(&build_text_selector("alt", text, opts)).await
  }

  /// Locate by title attribute inside this iframe.
  ///
  /// # Errors
  ///
  /// Returns an error if the iframe cannot be resolved.
  pub async fn get_by_title(&self, text: &str, opts: &TextOptions) -> Result<Locator> {
    self.locator(&build_text_selector("title", text, opts)).await
  }

  /// Get the locator that owns the iframe element (the `<iframe>` itself).
  #[must_use]
  pub fn owner(&self) -> Locator {
    Locator {
      page: Arc::clone(&self.page),
      selector: self.iframe_selector.clone(),
      frame_id: None,
    }
  }

  /// Target a nested iframe inside this iframe's content.
  #[must_use]
  pub fn frame_locator(&self, selector: &str) -> FrameLocator {
    FrameLocator {
      page: Arc::clone(&self.page),
      iframe_selector: format!("{} >> frame >> {selector}", self.iframe_selector),
    }
  }

  /// Resolve the iframe element to a frame ID.
  async fn resolve_frame_id(&self) -> Result<Arc<str>> {
    // Find the iframe element and match it to a frame in the tree
    let page_inner = self.page.inner();
    page_inner.ensure_engine_injected().await?;
    let fd = "window.__fd";
    let parsed = selectors::parse(&self.iframe_selector)?;
    let parts_json = selectors::build_parts_json(&parsed);
    let js = format!(
      "(function() {{ \
         var el = {fd}.selOne({parts_json}); \
         if (!el || el.tagName !== 'IFRAME') return null; \
         return el.getAttribute('name') || el.src || null; \
       }})()"
    );
    let result = page_inner.evaluate(&js).await?;

    // Get the frame tree and match by name or URL
    let frames = page_inner.get_frame_tree().await?;
    let frame_hint = result
      .and_then(|v| v.as_str().map(std::string::ToString::to_string))
      .unwrap_or_default();

    // Try matching by name first, then by URL
    for frame in &frames {
      if !frame.name.is_empty() && frame.name == frame_hint {
        return Ok(Arc::from(frame.frame_id.as_str()));
      }
    }
    for frame in &frames {
      if !frame.url.is_empty() && frame.url == frame_hint {
        return Ok(Arc::from(frame.frame_id.as_str()));
      }
    }

    // Fallback: if there's only one child frame, use it
    let child_frames: Vec<_> = frames.iter().filter(|f| f.parent_frame_id.is_some()).collect();
    if child_frames.len() == 1 {
      return Ok(Arc::from(child_frames[0].frame_id.as_str()));
    }

    Err(crate::error::FerriError::invalid_selector(
      self.iframe_selector.clone(),
      format!("could not resolve iframe; found {} child frames", child_frames.len()),
    ))
  }
}

impl std::fmt::Debug for FrameLocator {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("FrameLocator")
      .field("iframe_selector", &self.iframe_selector)
      .finish_non_exhaustive()
  }
}

// ── Selector builders ───────────────────────────────────────────────────────

pub(crate) fn build_role_selector(role: &str, opts: &RoleOptions) -> String {
  let mut sel = format!("role={role}");
  if let Some(name) = &opts.name {
    let _ = write!(sel, "[name=\"{}\"]", name.replace('"', "\\\""));
  }
  if opts.exact == Some(true) {
    // exact name matching handled at the engine level
  }
  if let Some(true) = opts.checked {
    sel.push_str("[checked=true]");
  }
  if let Some(false) = opts.checked {
    sel.push_str("[checked=false]");
  }
  if let Some(true) = opts.disabled {
    sel.push_str("[disabled=true]");
  }
  if let Some(false) = opts.disabled {
    sel.push_str("[disabled=false]");
  }
  if let Some(true) = opts.expanded {
    sel.push_str("[expanded=true]");
  }
  if let Some(false) = opts.expanded {
    sel.push_str("[expanded=false]");
  }
  if let Some(level) = opts.level {
    let _ = write!(sel, "[level={level}]");
  }
  if let Some(true) = opts.pressed {
    sel.push_str("[pressed=true]");
  }
  if let Some(true) = opts.selected {
    sel.push_str("[selected=true]");
  }
  if let Some(true) = opts.include_hidden {
    sel.push_str("[include-hidden=true]");
  }
  sel
}

fn build_text_selector(engine: &str, text: &str, opts: &TextOptions) -> String {
  if opts.exact == Some(true) {
    format!("{engine}=\"{text}\"")
  } else {
    format!("{engine}={text}")
  }
}
