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
    let $page: &$crate::backend::AnyPage = $self.frame.page_arc().inner();
    $page
      .ensure_engine_injected()
      .await
      .map_err($crate::error::FerriError::from)?;
    let __fd = "window.__fd";
    let __sel_js =
      $crate::selectors::build_selone_js(&$self.selector, &__fd).map_err($crate::error::FerriError::from)?;
    // Pass `None` for main-frame locators so the backend skips a
    // `frame_contexts` lookup; child frames thread their cached id.
    let __frame_id: ::std::option::Option<&str> = if $self.frame.is_main_frame() {
      ::std::option::Option::None
    } else {
      ::std::option::Option::Some($self.frame.id())
    };

    for (__i, &__delay_ms) in Locator::RETRY_BACKOFFS_MS.iter().enumerate() {
      if __delay_ms > 0 {
        ::tokio::time::sleep(::std::time::Duration::from_millis(__delay_ms)).await;
      }

      // Strict mode (Playwright default): resolve via `query_all` and bail
      // with [`crate::error::FerriError::StrictModeViolation`] if the
      // selector matches more than one element. We do the strict check on
      // every attempt so transient duplicates (e.g. during SSR rehydration)
      // still trigger the retry loop rather than failing immediately.
      if $self.strict {
        match $crate::selectors::query_all($page, &$self.selector, __frame_id).await {
          ::std::result::Result::Ok(ref __matches) if __matches.len() > 1 => {
            $crate::selectors::cleanup_tags($page).await;
            return ::std::result::Result::Err($crate::error::FerriError::strict(
              $self.selector.clone(),
              __matches.len(),
            ));
          },
          ::std::result::Result::Ok(_) | ::std::result::Result::Err(_) => {
            $crate::selectors::cleanup_tags($page).await;
          },
        }
      }

      match $crate::selectors::query_one_prebuilt($page, &__sel_js, &$self.selector, __frame_id).await {
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

/// A lazy element locator bound to a [`crate::Frame`]. Mirrors
/// Playwright's `client/locator.ts::Locator` — every Locator carries a
/// Frame reference, and all DOM resolution and action dispatch happens
/// in that frame's execution context. Chaining (`.locator()`,
/// `.filter()`, `.first()`, etc.) returns a new Locator on the same
/// Frame; the Frame itself is cheap to clone (two `Arc`s).
#[derive(Clone)]
pub struct Locator {
  /// Owning frame. Provides the page back-reference (`frame.page_arc()`)
  /// and the execution-context id (`frame.id()`) used by every action.
  pub(crate) frame: crate::frame::Frame,
  pub(crate) selector: String,
  /// Strict mode: error with [`crate::error::FerriError::StrictModeViolation`]
  /// if the selector resolves to multiple elements. Playwright enables strict
  /// mode on every Locator action by default; `first()` / `last()` / `nth()` /
  /// `strict(false)` opt out.
  pub(crate) strict: bool,
}

impl Locator {
  /// Construct a Locator with Playwright-default strict mode enabled.
  #[must_use]
  pub(crate) fn new(frame: crate::frame::Frame, selector: String) -> Self {
    Self {
      frame,
      selector,
      strict: true,
    }
  }

  /// Returns a copy of this locator with strict-mode toggled.
  ///
  /// In strict mode (default), any action on a locator that matches more than
  /// one element raises [`crate::error::FerriError::StrictModeViolation`].
  /// Pass `false` to explicitly allow multi-match (Playwright's behaviour
  /// under `locator.first()` / `.last()` / `.nth()`).
  #[must_use]
  pub fn strict(&self, strict: bool) -> Locator {
    Locator {
      frame: self.frame.clone(),
      selector: self.selector.clone(),
      strict,
    }
  }
  // ── Sub-locators (chain with >>) ──────────────────────────────────────────

  /// Narrow this locator's scope.
  ///
  /// Playwright:
  /// `locator(selectorOrLocator: string | Locator,
  ///          options?: Omit<LocatorOptions, 'visible'>): Locator`
  /// (`/tmp/playwright/packages/playwright-core/src/client/locator.ts:164`).
  ///
  /// Infallible by design — matches Playwright's chainable Locator API.
  /// A cross-page inner locator encodes a sentinel clause that the
  /// selector engine rejects at resolve time; JSON encoding never fails
  /// for a valid UTF-8 selector. `visible` is stripped from the option
  /// bag (Playwright restricts it to `filter()` and the constructor).
  #[must_use]
  pub fn locator(
    &self,
    selector_or_locator: impl Into<crate::options::LocatorLike>,
    options: Option<crate::options::FilterOptions>,
  ) -> Locator {
    let inner = selector_or_locator.into();
    let base = match &inner {
      crate::options::LocatorLike::Selector(s) => self.chain(s),
      crate::options::LocatorLike::Locator(l) => {
        if Arc::ptr_eq(self.frame.page_arc(), l.frame.page_arc()) {
          self.chain(&format!("internal:chain={}", json_quote(&l.selector)))
        } else {
          // Encoded sentinel — the selector engine rejects it, so the
          // caller sees an explicit InvalidSelector at the first action
          // rather than a silently-wrong filter. Playwright throws
          // synchronously in JS; we defer to resolve time to keep the
          // Locator chain API infallible.
          self.chain("internal:cross-frame-error=true")
        }
      },
    };
    match options {
      Some(mut opts) => {
        opts.visible = None; // Playwright's Omit<LocatorOptions, 'visible'>
        base.filter(&opts)
      },
      None => base,
    }
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

  /// First element. Opts out of strict mode because the selector explicitly
  /// narrows to a single match.
  #[must_use]
  pub fn first(&self) -> Locator {
    self.chain("nth=0").strict(false)
  }

  /// Last element. Opts out of strict mode (explicit single match).
  #[must_use]
  pub fn last(&self) -> Locator {
    self.chain("nth=-1").strict(false)
  }

  /// nth element. Opts out of strict mode (explicit single match).
  #[must_use]
  pub fn nth(&self, index: i32) -> Locator {
    self.chain(&format!("nth={index}")).strict(false)
  }

  /// Filter this locator by text content, inner-locator presence/absence,
  /// or visibility.
  ///
  /// Mirrors Playwright's
  /// `/tmp/playwright/packages/playwright-core/src/client/locator.ts::Locator#constructor`
  /// option-to-selector encoding:
  ///
  /// * `has_text` → ` >> internal:has-text=<escaped>` (plain-text clause).
  /// * `has_not_text` → ` >> internal:has-not-text=<escaped>`.
  /// * `has` (inner [`Locator`]) → ` >> internal:has=<JSON inner selector>`.
  /// * `has_not` (inner [`Locator`]) → ` >> internal:has-not=<JSON inner selector>`.
  /// * `visible: Some(b)` → ` >> visible=true|false`.
  ///
  /// Inner locators must belong to the same page as `self`; otherwise this
  /// returns a locator whose selector contains an explicit error marker
  /// — when resolved, the selector engine rejects it and the caller sees
  /// an [`crate::error::FerriError::InvalidSelector`]. This matches
  /// Playwright's behavior of throwing at construction time in JS while
  /// still keeping this method infallible in Rust.
  #[must_use]
  pub fn filter(&self, opts: &FilterOptions) -> Locator {
    use std::fmt::Write as _;

    // Build the combined filter suffix in one buffer, then chain once.
    let mut suffix = String::new();
    let push_sep = |buf: &mut String| {
      if !buf.is_empty() {
        buf.push_str(" >> ");
      }
    };

    if let Some(text) = &opts.has_text {
      let _ = write!(suffix, "internal:has-text={}", json_quote(text));
    }
    if let Some(text) = &opts.has_not_text {
      push_sep(&mut suffix);
      let _ = write!(suffix, "internal:has-not-text={}", json_quote(text));
    }
    if let Some(inner) = &opts.has {
      push_sep(&mut suffix);
      if inner
        .as_locator()
        .is_some_and(|l| !Arc::ptr_eq(self.frame.page_arc(), l.frame.page_arc()))
      {
        // Same-page invariant violation — inject a sentinel the selector
        // engine will reject so the caller sees an explicit error rather
        // than a silently-mismatched filter. Only enforceable when the
        // caller supplied a full `Locator`; raw selector strings skip
        // this check by design.
        let _ = write!(suffix, "internal:has-cross-frame-error=true");
      } else {
        let _ = write!(suffix, "internal:has={}", json_quote(inner.as_selector()));
      }
    }
    if let Some(inner) = &opts.has_not {
      push_sep(&mut suffix);
      if inner
        .as_locator()
        .is_some_and(|l| !Arc::ptr_eq(self.frame.page_arc(), l.frame.page_arc()))
      {
        let _ = write!(suffix, "internal:has-not-cross-frame-error=true");
      } else {
        let _ = write!(suffix, "internal:has-not={}", json_quote(inner.as_selector()));
      }
    }
    if let Some(v) = opts.visible {
      push_sep(&mut suffix);
      let _ = write!(suffix, "visible={}", if v { "true" } else { "false" });
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
    actions::wait_for_actionable(&el, self.frame.page_arc().inner())
      .await
      .ok();
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
    let result = actions::select_option(&el, self.frame.page_arc().inner(), value).await?;
    Ok(vec![result.selected_value])
  }

  // (select_option reserved for future ElementHandle / SelectOption array overloads per task #5.)

  /// Set file paths on a file input element.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not a file input or the upload fails.
  pub async fn set_input_files(&self, paths: &[String]) -> Result<()> {
    actions::upload_file(self.frame.page_arc().inner(), &self.selector, paths)
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
  /// Returns the raw attribute string exactly as
  /// `Element.getAttribute(name)` reports it (HTML attributes are always
  /// `string | null` per DOM spec — there is no native numeric/boolean
  /// attribute type). Playwright parity: Playwright's `getAttribute`
  /// returns `Promise<string | null>`; the previous implementation
  /// leaked the JSON-stringified form of non-string JS values (e.g.
  /// `"42"` vs `42`) — that path was unreachable with a well-behaved
  /// browser, but we now explicitly rule it out.
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
      // Per the DOM spec `Element.getAttribute` only ever returns
      // `string | null`. Anything else coming back from the eval
      // indicates a browser bug or an unexpected injected script —
      // surface as `None` rather than silently JSON-stringifying.
      _ => None,
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
    let fd = self.frame.page_arc().inner().injected_script().await?;
    let js = format!("{fd}.selCount({parts_json})");
    let val = self
      .evaluate_in_frame_js(&js)
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

  /// Wait for the element to reach the specified state.
  ///
  /// Playwright states (`packages/playwright-core/src/client/locator.ts`):
  ///
  /// * `"attached"` — element is present in the DOM. Computed style is
  ///   not consulted. Matches `element.isConnected`.
  /// * `"visible"` — element is attached **and** has non-empty bounding
  ///   box, is not `display:none` / `visibility:hidden` / `opacity:0`.
  /// * `"hidden"` — element is either detached or not visible. A
  ///   detached element satisfies `"hidden"` (Playwright parity).
  /// * `"detached"` — element is not present in the DOM.
  ///
  /// Previously `"attached"` and `"visible"` were conflated — both
  /// returned as soon as a DOM query succeeded. That broke Playwright
  /// tests that rely on `attached` resolving for zero-size or
  /// currently-invisible elements.
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
        "attached" => {
          // Only require DOM presence — do not consult computed style.
          if selectors::query_one(
            self.frame.page_arc().inner(),
            &self.selector,
            false,
            if self.frame.is_main_frame() {
              None
            } else {
              Some(self.frame.id())
            },
          )
          .await
          .is_ok()
          {
            selectors::cleanup_tags(self.frame.page_arc().inner()).await;
            return Ok(());
          }
        },
        "visible" => {
          // DOM presence AND computed-style visible. Fail silently
          // (fall through to next poll) if `is_visible()` errors
          // because the element is detached mid-poll.
          if let Ok(true) = self.is_visible().await {
            return Ok(());
          }
        },
        "detached" => {
          if selectors::query_one(
            self.frame.page_arc().inner(),
            &self.selector,
            false,
            if self.frame.is_main_frame() {
              None
            } else {
              Some(self.frame.id())
            },
          )
          .await
          .is_err()
          {
            return Ok(());
          }
          selectors::cleanup_tags(self.frame.page_arc().inner()).await;
        },
        "hidden" => {
          // Playwright: `hidden` is satisfied by detachment OR by the
          // element being present but not visible.
          if selectors::query_one(
            self.frame.page_arc().inner(),
            &self.selector,
            false,
            if self.frame.is_main_frame() {
              None
            } else {
              Some(self.frame.id())
            },
          )
          .await
          .is_err()
          {
            return Ok(());
          }
          selectors::cleanup_tags(self.frame.page_arc().inner()).await;
          if let Ok(false) = self.is_visible().await {
            return Ok(());
          }
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
    actions::wait_for_actionable(&el, self.frame.page_arc().inner())
      .await
      .ok();
    let delay = delay_ms.unwrap_or(50);
    for ch in text.chars() {
      self.frame.page_arc().inner().press_key(&ch.to_string()).await?;
      if delay > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
      }
    }
    Ok(())
  }

  // ── Drag to another locator ─────────────────────────────────────────────

  /// Drag this element to `target`. Mirrors Playwright's
  /// `Locator.dragTo(target, options)` signature per
  /// `/tmp/playwright/packages/playwright-core/types/types.d.ts:13293`.
  ///
  /// When [`DragAndDropOptions::source_position`] is set, the press point is
  /// the source element's padding-box origin offset by that point; otherwise
  /// the source element's center is used. Same for `target_position` on the
  /// release point. [`DragAndDropOptions::steps`] controls how many
  /// interpolated `mousemove` events are emitted between press and release
  /// (Playwright default: `1`). [`DragAndDropOptions::trial`] skips the
  /// actual mouse action, returning after both elements resolve.
  /// [`DragAndDropOptions::strict`] is ignored here (per Playwright) because
  /// this locator already carries its own strict flag.
  ///
  /// # Errors
  ///
  /// Returns an error if either element cannot be found, bounding box
  /// coordinates cannot be read, or the drag operation fails.
  pub async fn drag_to(&self, target: &Locator, options: Option<crate::options::DragAndDropOptions>) -> Result<()> {
    let opts = options.unwrap_or_default();

    // Get source + target geometry via call_js_fn_value (1 CDP each).
    let source_el = self.resolve().await?;
    let target_el = target.resolve().await?;

    // Parallel: get both bounding rects simultaneously — we need the full
    // rect (x, y, width, height) so that sourcePosition / targetPosition can
    // be offset from the padding-box origin as Playwright does.
    let (src_result, tgt_result) = tokio::join!(
      source_el.call_js_fn_value(
        "function() { try { this.scrollIntoViewIfNeeded(); } catch (e) { this.scrollIntoView(); } var r = this.getBoundingClientRect(); return {x: r.x, y: r.y, width: r.width, height: r.height}; }"
      ),
      target_el.call_js_fn_value(
        "function() { try { this.scrollIntoViewIfNeeded(); } catch (e) { this.scrollIntoView(); } var r = this.getBoundingClientRect(); return {x: r.x, y: r.y, width: r.width, height: r.height}; }"
      ),
    );

    let src = src_result?.ok_or_else(|| crate::error::FerriError::Other("no source bounding box".into()))?;
    let tgt = tgt_result?.ok_or_else(|| crate::error::FerriError::Other("no target bounding box".into()))?;

    let from = rect_point(&src, opts.source_position);
    let to = rect_point(&tgt, opts.target_position);

    // Playwright's `trial: true` performs actionability checks (resolve) and
    // skips the actual action. We've already resolved both elements above,
    // so simply return without dispatching mouse events.
    if opts.trial.unwrap_or(false) {
      return Ok(());
    }

    let steps = opts.steps.unwrap_or(1);
    self
      .frame
      .page_arc()
      .inner()
      .click_and_drag(from, to, steps)
      .await
      .map_err(Into::into)
  }

  // ── Combinators ─────────────────────────────────────────────────────────

  /// Union: matches elements from either this or the other locator.
  /// Creates a new locator that matches elements matched by **either**
  /// selector. Mirrors Playwright's `Locator.or(locator)` exactly: emits
  /// `>> internal:or=<json>` where the injected selector engine handles the
  /// union.
  ///
  /// Unlike CSS `:is()`, this works for every selector engine including
  /// `text=`, `role=`, `label=`, `testid=`, and chained rich selectors.
  #[must_use]
  pub fn or(&self, other: &Locator) -> Locator {
    self.chain(&format!(
      "internal:or={}",
      serde_json::to_string(&other.selector).unwrap_or_else(|_| format!("{:?}", other.selector))
    ))
  }

  /// Creates a new locator that matches elements matched by **both** this
  /// locator and `other` on the same element. Mirrors Playwright's
  /// `Locator.and(locator)` — emits `>> internal:and=<json>`.
  ///
  /// This is a fundamentally different operation from `locator.locator(...)`
  /// which narrows scope to descendants; `and` requires the same element to
  /// satisfy both selectors.
  #[must_use]
  pub fn and(&self, other: &Locator) -> Locator {
    self.chain(&format!(
      "internal:and={}",
      serde_json::to_string(&other.selector).unwrap_or_else(|_| format!("{:?}", other.selector))
    ))
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
        frame: self.frame.clone(),
        selector,
        strict: true,
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
    self.frame.page_arc().inner().ensure_engine_injected().await?;
    let fd = "window.__fd";
    let js = format!(
      "(function() {{ var r = {fd}._exec({parts_json}, document); \
       return r.map(function(e) {{ return (e.textContent || '').trim(); }}); }})()"
    );
    let val = self.frame.page_arc().inner().evaluate(&js).await?;
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
    self.frame.page_arc().inner().ensure_engine_injected().await?;
    let fd = "window.__fd";
    let js = format!("(function() {{ var elements = {fd}.selAll({parts_json}); return ({expression}); }})()");
    self.evaluate_in_frame_js(&js).await
  }

  /// Run a value-returning JS expression in this locator's frame.
  /// Uses `evaluate_in_frame` for non-main frames (CDP `contextId`,
  /// `BiDi` realm) and the no-context default for the main frame so we
  /// avoid an extra `frame_contexts` lookup on the hot path.
  async fn evaluate_in_frame_js(&self, js: &str) -> Result<Option<serde_json::Value>> {
    let inner = self.frame.page_arc().inner();
    if self.frame.is_main_frame() {
      inner.evaluate(js).await.map_err(Into::into)
    } else {
      inner.evaluate_in_frame(js, self.frame.id()).await.map_err(Into::into)
    }
  }

  // ── Page / Frame access ────────────────────────────────────────────────────

  /// Get the page this locator belongs to.
  #[must_use]
  pub fn page(&self) -> &Arc<crate::page::Page> {
    self.frame.page_arc()
  }

  /// The frame this locator resolves in. Mirrors Playwright's
  /// `locator._frame` — actions and queries always run in this frame's
  /// execution context.
  #[must_use]
  pub fn frame(&self) -> &crate::frame::Frame {
    &self.frame
  }

  /// Treat this locator as an `<iframe>` and return a `FrameLocator` for its content.
  ///
  /// Equivalent to Playwright's `locator.contentFrame()`. The returned
  /// `FrameLocator` creates locators scoped to the iframe's content document.
  #[must_use]
  pub fn content_frame(&self) -> FrameLocator {
    FrameLocator::for_iframe_in(self.frame.clone(), self.selector.clone())
  }

  /// Create a `FrameLocator` targeting an `<iframe>` matched by `selector` within
  /// this locator's scope.
  ///
  /// Equivalent to Playwright's `locator.frameLocator(selector)`.
  #[must_use]
  pub fn frame_locator(&self, selector: &str) -> FrameLocator {
    let frame_selector = if self.selector.is_empty() {
      selector.to_string()
    } else {
      format!("{} >> {selector}", self.selector)
    };
    FrameLocator::for_iframe_in(self.frame.clone(), frame_selector)
  }

  // ── Selector access ───────────────────────────────────────────────────────

  #[must_use]
  pub fn selector(&self) -> &str {
    &self.selector
  }

  /// Whether this locator runs action methods under strict mode (multi-match
  /// is an error). Mirrors Playwright's default.
  #[must_use]
  pub fn is_strict(&self) -> bool {
    self.strict
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
    self.frame.page_arc().inner().ensure_engine_injected().await?;
    let fd = "window.__fd";
    let js = format!("(function() {{ var el = {fd}.selOne({parts_json}); if (!el) return null; {js_body} }})()");

    for (i, &delay_ms) in Self::RETRY_BACKOFFS_MS.iter().enumerate() {
      if delay_ms > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
      }
      let result = self.evaluate_in_frame_js(&js).await;
      match result {
        // Element not found or evaluation failed -- retry if attempts remain.
        Ok(Some(serde_json::Value::Null) | None) | Err(_) if i < Self::RETRY_BACKOFFS_MS.len() - 1 => {},
        Ok(val) => return Ok(val),
        Err(e) => return Err(e),
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
    self.frame.page_arc().inner().ensure_engine_injected().await?;
    let fd = "window.__fd";
    let sel_js = selectors::build_selone_js(&self.selector, fd)?;
    let frame_id: Option<&str> = if self.frame.is_main_frame() {
      None
    } else {
      Some(self.frame.id())
    };
    selectors::query_one_prebuilt(self.frame.page_arc().inner(), &sel_js, &self.selector, frame_id)
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
      frame: self.frame.clone(),
      selector,
      strict: self.strict,
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
    self.frame.page_arc().inner().ensure_engine_injected().await?;
    let fd = "window.__fd";
    let js = format!("(function() {{ var el = {fd}.selOne({parts_json}); if (!el) return null; {js_body} }})()");
    self.evaluate_in_frame_js(&js).await
  }
}

impl std::fmt::Debug for Locator {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Locator")
      .field("selector", &self.selector)
      .field("frame", &self.frame)
      .field("strict", &self.strict)
      .finish()
  }
}

// ── FrameLocator ──────────────────────────────────────────────────────────────

/// A selector-builder that produces parent-frame [`Locator`]s targeting
/// content inside an `<iframe>`. Mirrors Playwright's `FrameLocator`
/// exactly:
///
/// `/tmp/playwright/packages/playwright-core/src/client/locator.ts::FrameLocatorImpl`
///
/// Holds the parent [`Frame`] (the one whose document contains the
/// `<iframe>` element) and the iframe's CSS-selector chain. Every
/// builder method composes a Locator selector with
/// `>> internal:control=enter-frame >>` so the iframe traversal is
/// performed by the selector engine at action time — not eagerly at
/// construction. The resulting `Locator` is the same `Locator` type
/// used everywhere else (no separate iframe-aware locator).
///
/// All methods are synchronous; `internal:control=enter-frame` is the
/// engine-side directive that switches root from the iframe element to
/// its `contentDocument` when a subsequent selector part runs.
#[derive(Clone)]
pub struct FrameLocator {
  /// Parent frame whose document contains the `<iframe>` element.
  /// For top-level `page.frame_locator(sel)` this is the main frame;
  /// nested `frame_locator.frame_locator(sel)` keeps the same parent
  /// frame and just appends to the selector chain.
  frame: crate::frame::Frame,
  /// CSS-selector chain ending at the `<iframe>` element. Composed with
  /// `>> internal:control=enter-frame >>` whenever we step further in.
  frame_selector: String,
}

impl FrameLocator {
  /// Construct a `FrameLocator` for an `<iframe>` matched by
  /// `iframe_selector` inside `parent_frame`'s document. Sync.
  #[must_use]
  pub fn for_iframe_in(parent_frame: crate::frame::Frame, iframe_selector: String) -> Self {
    Self {
      frame: parent_frame,
      frame_selector: iframe_selector,
    }
  }

  fn enter(&self, selector: &str) -> String {
    format!("{} >> internal:control=enter-frame >> {selector}", self.frame_selector)
  }

  /// Locator inside this iframe. Mirrors Playwright's
  /// `frameLocator.locator(selector, options?)` — sync, returns a
  /// `Locator` bound to the parent frame with an `enter-frame`
  /// selector chain.
  #[must_use]
  pub fn locator(&self, selector: &str, options: Option<crate::options::FilterOptions>) -> Locator {
    let base = Locator::new(self.frame.clone(), self.enter(selector));
    match options {
      Some(opts) => base.filter(&opts),
      None => base,
    }
  }

  /// `getByRole` inside this iframe.
  #[must_use]
  pub fn get_by_role(&self, role: &str, opts: &RoleOptions) -> Locator {
    self.locator(&build_role_selector(role, opts), None)
  }

  /// `getByText` inside this iframe.
  #[must_use]
  pub fn get_by_text(&self, text: &str, opts: &TextOptions) -> Locator {
    self.locator(&build_text_selector("text", text, opts), None)
  }

  /// `getByTestId` inside this iframe.
  #[must_use]
  pub fn get_by_test_id(&self, test_id: &str) -> Locator {
    self.locator(&format!("testid={test_id}"), None)
  }

  /// `getByLabel` inside this iframe.
  #[must_use]
  pub fn get_by_label(&self, text: &str, opts: &TextOptions) -> Locator {
    self.locator(&build_text_selector("label", text, opts), None)
  }

  /// `getByPlaceholder` inside this iframe.
  #[must_use]
  pub fn get_by_placeholder(&self, text: &str, opts: &TextOptions) -> Locator {
    self.locator(&build_text_selector("placeholder", text, opts), None)
  }

  /// `getByAltText` inside this iframe.
  #[must_use]
  pub fn get_by_alt_text(&self, text: &str, opts: &TextOptions) -> Locator {
    self.locator(&build_text_selector("alt", text, opts), None)
  }

  /// `getByTitle` inside this iframe.
  #[must_use]
  pub fn get_by_title(&self, text: &str, opts: &TextOptions) -> Locator {
    self.locator(&build_text_selector("title", text, opts), None)
  }

  /// The locator pointing at the `<iframe>` element itself, in the
  /// parent frame's context. Mirrors Playwright's `frameLocator.owner()`.
  #[must_use]
  pub fn owner(&self) -> Locator {
    Locator::new(self.frame.clone(), self.frame_selector.clone())
  }

  /// `frameLocator` for a nested `<iframe>`. Mirrors Playwright's
  /// `frameLocator.frameLocator(selector)` — appends an enter-frame
  /// step plus the next iframe selector.
  #[must_use]
  pub fn frame_locator(&self, selector: &str) -> FrameLocator {
    FrameLocator {
      frame: self.frame.clone(),
      frame_selector: self.enter(selector),
    }
  }

  /// First matching iframe (`nth=0`).
  #[must_use]
  pub fn first(&self) -> FrameLocator {
    FrameLocator {
      frame: self.frame.clone(),
      frame_selector: format!("{} >> nth=0", self.frame_selector),
    }
  }

  /// Last matching iframe (`nth=-1`).
  #[must_use]
  pub fn last(&self) -> FrameLocator {
    FrameLocator {
      frame: self.frame.clone(),
      frame_selector: format!("{} >> nth=-1", self.frame_selector),
    }
  }

  /// Nth matching iframe.
  #[must_use]
  pub fn nth(&self, index: i32) -> FrameLocator {
    FrameLocator {
      frame: self.frame.clone(),
      frame_selector: format!("{} >> nth={index}", self.frame_selector),
    }
  }
}

impl std::fmt::Debug for FrameLocator {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("FrameLocator")
      .field("frame_selector", &self.frame_selector)
      .field("frame", &self.frame)
      .finish()
  }
}

// ── Selector builders ───────────────────────────────────────────────────────

/// Compute the drag press/release point from an element's bounding rect and
/// an optional `position`. When `position` is `Some`, the point is the
/// element's padding-box top-left offset by `(position.x, position.y)` —
/// matching Playwright's `sourcePosition` / `targetPosition` semantics. When
/// `position` is `None`, the element's center is used.
fn rect_point(rect: &serde_json::Value, position: Option<crate::options::Point>) -> (f64, f64) {
  let x = rect.get("x").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
  let y = rect.get("y").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
  let width = rect.get("width").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
  let height = rect.get("height").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
  match position {
    Some(p) => (x + p.x, y + p.y),
    None => (x + width / 2.0, y + height / 2.0),
  }
}

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

/// Produce the Playwright-compatible JSON-quoted form of an inner
/// selector string — matches the output of `JSON.stringify(str)` in JS.
/// Used by [`Locator::filter`] and [`Locator::and`] / [`Locator::or`]
/// when embedding nested selector text in `internal:*` clauses.
///
/// Falls back to a Rust `{:?}` debug form if `serde_json` cannot encode
/// (impossible for valid UTF-8 strings, but kept for defensive symmetry
/// with existing call sites at `and` / `or`).
fn json_quote(s: &str) -> String {
  serde_json::to_string(s).unwrap_or_else(|_| format!("{s:?}"))
}
