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
///
/// `$timeout_ms` is an `Option<u64>` — the per-call override from the action's
/// option bag. `None` falls back to `page.default_timeout()` (set via
/// `page.setDefaultTimeout`). A resolved value of `0` means "no timeout" and
/// loops forever (matches Playwright's behavior). `$op` is a `&str` used in the
/// timeout-error message (Playwright's `TimeoutError { while $op }`).
///
/// Polling schedule mirrors Playwright's `retryWithProgressAndTimeouts`:
/// `[0, 0, 20, 50, 100, 100, 500]`, clamped at the last value on overflow. See
/// `/tmp/playwright/packages/playwright-core/src/server/frames.ts:1102`.
macro_rules! retry_resolve {
  ($self:expr, $timeout_ms:expr, $op:expr, |$el:ident, $page:ident| $body:expr) => {{
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

    let __op_name: &str = $op;
    let __resolved_timeout: u64 = $timeout_ms.unwrap_or_else(|| $self.frame.page_arc().default_timeout());
    let __deadline: ::std::option::Option<::std::time::Instant> = if __resolved_timeout == 0 {
      ::std::option::Option::None
    } else {
      ::std::option::Option::Some(::std::time::Instant::now() + ::std::time::Duration::from_millis(__resolved_timeout))
    };

    let mut __idx: usize = 0;
    loop {
      // Deadline check up-front so we never race into one more attempt after
      // time has already run out.
      if let ::std::option::Option::Some(__d) = __deadline {
        if ::std::time::Instant::now() >= __d {
          return ::std::result::Result::Err($crate::error::FerriError::timeout(
            __op_name.to_string(),
            __resolved_timeout,
          ));
        }
      }

      let __delay_ms = Locator::RETRY_BACKOFFS_MS[__idx.min(Locator::RETRY_BACKOFFS_MS.len() - 1)];
      __idx = __idx.saturating_add(1);
      if __delay_ms > 0 {
        // Clamp the sleep to whatever's left on the deadline so the timeout
        // error fires on time rather than after an overshoot sleep.
        let __sleep_ms = match __deadline {
          ::std::option::Option::Some(__d) => {
            let __left = u64::try_from(__d.saturating_duration_since(::std::time::Instant::now()).as_millis())
              .unwrap_or(__delay_ms);
            __delay_ms.min(__left)
          },
          ::std::option::Option::None => __delay_ms,
        };
        if __sleep_ms > 0 {
          ::tokio::time::sleep(::std::time::Duration::from_millis(__sleep_ms)).await;
        }
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
            if e.contains("not connected")
              || e.contains("not found")
              || e.contains("detached")
              || e.starts_with("error:not") =>
          {
            // Retriable — matches Playwright's `_retryAction` contract
            // where `checkElementStates` returns `error:notvisible` /
            // `error:notenabled` / `error:noteditable` etc. as signals to
            // keep polling until the deadline.
          },
          ::std::result::Result::Err(e) => return ::std::result::Result::Err($crate::error::FerriError::from(e)),
        },
        ::std::result::Result::Err(_) => {
          // Element not found this iteration; retry until deadline.
        },
      }
    }
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

  /// Click the element matched by this locator with the full Playwright
  /// [`crate::options::ClickOptions`] surface. Mirrors
  /// `/tmp/playwright/packages/playwright-core/types/types.d.ts:12986`.
  ///
  /// All options (`button`, `click_count`, `delay`, `force`, `modifiers`,
  /// `position`, `steps`, `trial`, `timeout`) are honored across all
  /// four backends; `no_wait_after` is accepted for signature parity
  /// but has no effect (ferridriver does not implicitly wait for
  /// navigation after click).
  ///
  /// Pass `None` for the common no-options path.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found, is not actionable
  /// (unless `force=true`), or the click dispatch fails.
  pub async fn click(&self, opts: Option<crate::options::ClickOptions>) -> Result<()> {
    let opts = opts.unwrap_or_default();
    // Borrow `opts` across retry iterations — references are `Copy`, so
    // each `async move` closure captures a fresh ref instead of moving
    // the owned `ClickOptions` (which contains a non-Copy `Vec<Modifier>`).
    let opts_ref = &opts;
    retry_resolve!(self, opts_ref.timeout, "click", |el, page| async move {
      actions::click_with_opts(&el, page, opts_ref).await
    })
  }

  /// Double-click the element matched by this locator.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or the double-click fails.
  pub async fn dblclick(&self, opts: Option<crate::options::DblClickOptions>) -> Result<()> {
    // `dblclick` is a click pair with `clickCount` = 1 then 2 — Playwright's
    // `server/dom.ts::ElementHandle._dblclick` does the same; our shared
    // `click_with_opts` honors that when `click_count` is set to `2`.
    let click_opts = opts.unwrap_or_default().into_click_options();
    let click_opts_ref = &click_opts;
    retry_resolve!(self, click_opts_ref.timeout, "dblclick", |el, page| async move {
      actions::click_with_opts(&el, page, click_opts_ref).await
    })
  }

  /// Right-click (context menu click) on the element.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found, its bounding box
  /// cannot be computed, or the right-click dispatch fails.
  pub async fn right_click(&self) -> Result<()> {
    retry_resolve!(
      self,
      ::std::option::Option::<u64>::None,
      "right_click",
      |el, page| async move {
        let center = el.call_js_fn_value(
        "function() { this.scrollIntoViewIfNeeded(); var r = this.getBoundingClientRect(); return {x: r.x + r.width/2, y: r.y + r.height/2}; }"
      ).await?;
        if let Some(c) = center {
          let x = c.get("x").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
          let y = c.get("y").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
          page.click_at_opts(x, y, "right", 1).await?;
        }
        Ok::<(), String>(())
      }
    )
  }

  /// Fill an input or textarea element with the given value.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or is not a fillable element.
  pub async fn fill(&self, value: &str, opts: Option<crate::options::FillOptions>) -> Result<()> {
    let opts = opts.unwrap_or_default();
    let force = opts.is_force();
    let opts_ref = &opts;
    retry_resolve!(self, opts_ref.timeout, "fill", |el, page| async move {
      // `actions::fill(..., force)` runs Playwright's
      // `checkElementStates(['visible','enabled','editable'])` internally
      // when `force` is false and returns the `error:not<state>` marker
      // the retry loop knows to keep polling on. `force=true` jumps
      // straight to the DOM write, matching Playwright's `_fill(force)`.
      actions::fill(&el, page, value, force).await
    })
  }

  /// Clear the value of an input or textarea element.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found.
  pub async fn clear(&self) -> Result<()> {
    retry_resolve!(
      self,
      ::std::option::Option::<u64>::None,
      "clear",
      |el, _page| async move {
        el.call_js_fn(
          "function() { \
        if (window.__fd) window.__fd.clearAndDispatch(this); \
        else { this.value = ''; } \
      }",
        )
        .await?;
        Ok::<(), String>(())
      }
    )
  }

  /// Type text into the element character by character using keyboard events.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or key dispatch fails.
  pub async fn r#type(&self, text: &str, opts: Option<crate::options::TypeOptions>) -> Result<()> {
    let opts = opts.unwrap_or_default();
    let delay_ms = opts.resolved_delay_ms();
    let timeout_ms = opts.timeout;
    retry_resolve!(self, timeout_ms, "type", |el, page| async move {
      actions::wait_for_actionable(&el, page).await.ok();
      if delay_ms > 0 {
        // With a per-char delay, fall back to the character-by-character
        // keyboard dispatch (same code path Playwright uses for
        // `pressSequentially`).
        for ch in text.chars() {
          page.press_key(&ch.to_string()).await?;
          tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        }
        Ok(())
      } else {
        el.type_str(text).await
      }
    })
  }

  /// Press a key or key combination (e.g. "Enter", "Control+a").
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or the key press fails.
  pub async fn press(&self, key: &str, opts: Option<crate::options::PressOptions>) -> Result<()> {
    let opts = opts.unwrap_or_default();
    let delay_ms = opts.resolved_delay_ms();
    let timeout_ms = opts.timeout;
    retry_resolve!(self, timeout_ms, "press", |el, page| async move {
      actions::wait_for_actionable(&el, page).await.ok();
      if delay_ms > 0 {
        // Playwright: when delay is set, press is equivalent to
        // keyDown + sleep(delay) + keyUp so the page observes the
        // held-key interval.
        page.key_down(key).await?;
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        page.key_up(key).await
      } else {
        page.press_key(key).await
      }
    })
  }

  /// Hover over the element matched by this locator.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or the hover action fails.
  pub async fn hover(&self, opts: Option<crate::options::HoverOptions>) -> Result<()> {
    let opts = opts.unwrap_or_default();
    let opts_ref = &opts;
    retry_resolve!(self, opts_ref.timeout, "hover", |el, page| async move {
      actions::hover_with_opts(&el, page, opts_ref).await
    })
  }

  /// Focus the element matched by this locator.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found.
  pub async fn focus(&self) -> Result<()> {
    retry_resolve!(
      self,
      ::std::option::Option::<u64>::None,
      "focus",
      |el, _page| async move {
        el.call_js_fn("function() { this.focus(); }").await?;
        Ok::<(), String>(())
      }
    )
  }

  /// Check a checkbox or radio button if it is not already checked.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or is not actionable.
  pub async fn check(&self, opts: Option<crate::options::CheckOptions>) -> Result<()> {
    self.set_checked(true, opts).await
  }

  /// Uncheck a checkbox if it is currently checked. Mirrors Playwright's
  /// `LocatorUncheckOptions`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or is not actionable.
  pub async fn uncheck(&self, opts: Option<crate::options::CheckOptions>) -> Result<()> {
    self.set_checked(false, opts).await
  }

  /// Set the checked state of a checkbox or radio button to match
  /// `checked`. Playwright-parity: reads the element's current
  /// `checked` property; if it already matches the target state, the
  /// call is a no-op (but actionability checks still run). Otherwise
  /// dispatches a real click via [`actions::click_with_opts`] so the
  /// page sees `input` / `change` events with the correct timing.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found, is not
  /// actionable, or the click dispatch fails.
  pub async fn set_checked(&self, checked: bool, opts: Option<crate::options::CheckOptions>) -> Result<()> {
    let opts = opts.unwrap_or_default();
    let trial = opts.is_trial();
    // Lower to ClickOptions for the shared click dispatch path so
    // `force` / `trial` / `position` / `timeout` all flow through.
    let click_opts = opts.into_click_options();
    let click_opts_ref = &click_opts;
    retry_resolve!(self, click_opts_ref.timeout, "check", |el, page| async move {
      // Playwright's `_setChecked` flow (server/dom.ts:758):
      //   1. Read current checked state (via `fd.getChecked`, which
      //      understands `input[type=checkbox|radio]` AND ARIA
      //      `aria-checked` roles — `this.checked` alone misses the
      //      latter).
      //   2. If current already matches target → done, no click.
      //   3. Uncheck of a checked radio → hard error (radios only
      //      toggle off by selecting another in their group).
      //   4. Dispatch the click with the caller's options.
      //   5. If `trial` → done (skip verification).
      //   6. Re-read state; if it still doesn't match the target →
      //      `"Clicking the checkbox did not change its state"`.
      let fd = page.injected_script().await?;
      let state_js = format!(
        "function() {{ \
           var r = {fd}.getChecked(this); \
           var isRadio = this.nodeName === 'INPUT' && this.type === 'radio'; \
           return JSON.stringify({{ state: r, isRadio: isRadio }}); \
         }}"
      );
      let read_state = async || -> ::std::result::Result<(Option<bool>, bool), String> {
        let raw = el
          .call_js_fn_value(&state_js)
          .await?
          .and_then(|v| v.as_str().map(std::string::ToString::to_string))
          .unwrap_or_default();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap_or(serde_json::json!({}));
        let is_radio = parsed
          .get("isRadio")
          .and_then(serde_json::Value::as_bool)
          .unwrap_or(false);
        let state_val = match parsed.get("state") {
          Some(v) if v.is_boolean() => Some(v.as_bool().unwrap_or(false)),
          _ => None,
        };
        Ok((state_val, is_radio))
      };

      let (current, is_radio) = read_state().await?;
      let Some(current) = current else {
        return Err("Not a checkbox, radio button, or ARIA-checkable element — cannot check/uncheck".to_string());
      };
      if current == checked {
        return Ok::<(), String>(());
      }
      if !checked && is_radio {
        return Err(
          "Cannot uncheck radio button. Radio buttons can only be unchecked by selecting another \
           radio button in the same group."
            .to_string(),
        );
      }
      actions::click_with_opts(&el, page, click_opts_ref).await?;
      if trial {
        return Ok::<(), String>(());
      }
      let (new_state, _) = read_state().await?;
      if new_state != Some(checked) {
        return Err("Clicking the checkbox did not change its state".to_string());
      }
      Ok::<(), String>(())
    })
  }

  /// Tap the element (touch event). Dispatches touchstart + touchend on platforms
  /// that support Touch/TouchEvent APIs, falls back to pointerdown + pointerup + click
  /// on desktop browsers (e.g. macOS `WKWebView`) where Touch constructors are unavailable.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or the tap event dispatch fails.
  pub async fn tap(&self, opts: Option<crate::options::TapOptions>) -> Result<()> {
    let opts = opts.unwrap_or_default();
    let opts_ref = &opts;
    retry_resolve!(self, opts_ref.timeout, "tap", |el, page| async move {
      actions::tap_with_opts(&el, page, opts_ref).await
    })
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
  pub async fn select_option(
    &self,
    values: Vec<crate::options::SelectOptionValue>,
    opts: Option<crate::options::SelectOptionOptions>,
  ) -> Result<Vec<String>> {
    let opts = opts.unwrap_or_default();
    let timeout_ms = opts.timeout;
    let force = opts.force.unwrap_or(false);
    let values_ref = &values;
    // Mirrors Playwright's `server/dom.ts::_selectOption`: when not
    // `force`, gate the dispatch on `checkElementStates(['visible',
    // 'enabled'])` so a hidden or disabled `<select>` returns the
    // `error:not<state>` retriable marker until the deadline fires.
    // `force: true` skips the pre-check and goes straight to the
    // injected `selectOptions` call.
    retry_resolve!(self, timeout_ms, "selectOption", |el, page| async move {
      if !force {
        let fd = page.injected_script().await?;
        let state_raw = el
          .call_js_fn_value(&format!(
            "function() {{ return {fd}.checkElementStates(this, ['visible', 'enabled']); }}"
          ))
          .await?
          .and_then(|v| v.as_str().map(std::string::ToString::to_string))
          .unwrap_or_else(|| "error:notconnected".to_string());
        if state_raw != "done" {
          return Err(state_raw);
        }
      }
      actions::select_options(&el, page, values_ref).await
    })
  }

  /// Set file paths on a file input element.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not a file input or the upload fails.
  pub async fn set_input_files(
    &self,
    files: crate::options::InputFiles,
    _opts: Option<crate::options::SetInputFilesOptions>,
  ) -> Result<()> {
    // Lower `Payloads` to temp-file paths so the wire-level CDP
    // `DOM.setFileInputFiles` command can carry them unchanged — the
    // alternative would be a separate per-backend `setFileInputBytes`
    // op, which only Playwright's internal CDP protocol supports.
    // Temp files live for the action only; we delete them after the
    // backend call returns regardless of success/failure.
    match files {
      crate::options::InputFiles::Paths(paths) => {
        let strs: Vec<String> = paths.into_iter().map(|p| p.display().to_string()).collect();
        actions::upload_file(self.frame.page_arc().inner(), &self.selector, &strs)
          .await
          .map_err(Into::into)
      },
      crate::options::InputFiles::Payloads(payloads) => {
        let tmp_dir = std::env::temp_dir().join(format!("ferridriver-files-{}", std::process::id()));
        std::fs::create_dir_all(&tmp_dir)
          .map_err(|e| crate::error::FerriError::Other(format!("failed to create upload temp dir: {e}")))?;
        let mut paths: Vec<String> = Vec::new();
        for (i, p) in payloads.iter().enumerate() {
          let safe_name = p.name.replace(['/', '\\', '\0'], "_");
          let path = tmp_dir.join(format!("{i}-{safe_name}"));
          std::fs::write(&path, &p.buffer)
            .map_err(|e| crate::error::FerriError::Other(format!("failed to write upload payload: {e}")))?;
          paths.push(path.display().to_string());
        }
        let res = actions::upload_file(self.frame.page_arc().inner(), &self.selector, &paths).await;
        // Best-effort cleanup — don't let an unlink error shadow the
        // primary upload outcome.
        for p in &paths {
          let _ = std::fs::remove_file(p);
        }
        let _ = std::fs::remove_dir(&tmp_dir);
        res.map_err(Into::into)
      },
    }
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

  /// Dispatch a DOM event of the given type on the element. Mirrors
  /// Playwright's `frames.ts::dispatchEvent` (see
  /// `/tmp/playwright/packages/playwright-core/src/server/frames.ts:847`):
  /// resolve the element under the retry loop (Playwright does NOT run
  /// actionability for dispatchEvent — it's a programmatic dispatch),
  /// then invoke `injectedScript.dispatchEvent` with the matching
  /// constructor. `opts.timeout` flows through to the retry deadline.
  ///
  /// # Errors
  ///
  /// Returns `FerriError::Timeout` if the element does not appear
  /// before the deadline.
  pub async fn dispatch_event(
    &self,
    event_type: &str,
    event_init: Option<serde_json::Value>,
    opts: Option<crate::options::DispatchEventOptions>,
  ) -> Result<()> {
    let timeout_ms = opts.and_then(|o| o.timeout);
    let init_json = event_init.as_ref().map_or_else(
      || "{}".to_string(),
      |v| serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()),
    );
    // Escape `</script>` which would break our JS formatting if
    // event_init contained a close-script sequence.
    let init_js = init_json.replace("</", "<\\/");
    let js = format!(
      "function() {{ \
        var type = '{event_type}'; \
        var init = Object.assign({{bubbles: true, cancelable: true, composed: true}}, {init_js}); \
        var ev; \
        if (['click','dblclick','mousedown','mouseup','mouseenter','mouseleave','mousemove','mouseover','mouseout','contextmenu','auxclick'].includes(type)) {{ \
          ev = new MouseEvent(type, init); \
        }} else if (['keydown','keyup','keypress'].includes(type)) {{ \
          ev = new KeyboardEvent(type, init); \
        }} else if (['touchstart','touchend','touchmove','touchcancel'].includes(type) && typeof TouchEvent !== 'undefined') {{ \
          ev = new TouchEvent(type, init); \
        }} else if (['pointerdown','pointerup','pointermove','pointerover','pointerout','pointerenter','pointerleave','pointercancel','gotpointercapture','lostpointercapture'].includes(type)) {{ \
          ev = new PointerEvent(type, init); \
        }} else if (['dragstart','drag','dragenter','dragleave','dragover','drop','dragend'].includes(type)) {{ \
          ev = new DragEvent(type, init); \
        }} else if (['focus','blur','focusin','focusout'].includes(type)) {{ \
          ev = new FocusEvent(type, init); \
        }} else if (['input','beforeinput'].includes(type)) {{ \
          ev = new InputEvent(type, init); \
        }} else if (type === 'wheel') {{ \
          ev = new WheelEvent(type, init); \
        }} else if (['deviceorientation','deviceorientationabsolute'].includes(type)) {{ \
          ev = new DeviceOrientationEvent(type, init); \
        }} else {{ \
          ev = new Event(type, init); \
        }} \
        this.dispatchEvent(ev); \
      }}"
    );
    let js_ref = js.as_str();
    retry_resolve!(self, timeout_ms, "dispatchEvent", |el, _page| async move {
      el.call_js_fn(js_ref).await
    })
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
  pub async fn press_sequentially(&self, text: &str, opts: Option<crate::options::TypeOptions>) -> Result<()> {
    // Playwright's `pressSequentially` shares the `TypeOptions` shape
    // with deprecated `type` (same three fields), so route both here.
    self.r#type(text, opts).await
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
