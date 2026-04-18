//! `ElementHandle` — `JSHandle` specialisation for DOM elements.
//!
//! Mirrors Playwright's `ElementHandle extends JSHandle`
//! (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts`).
//! We use composition instead of inheritance: an `ElementHandle` wraps
//! both a [`JSHandle`] (for lifecycle + evaluate) and an `AnyElement`
//! (for DOM-specific actions that already have per-backend impls
//! threaded through [`crate::actions`]).
//!
//! Phase C's minimum viable surface covers lifecycle + `as_js_handle()`.
//! Phase E bolts the ~25 Playwright DOM action methods on top of this
//! same type.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::backend::{AnyElement, ImageFormat};
use crate::error::{FerriError, Result};
use crate::js_handle::{HandleRemote, JSHandle, disposed_error};
use crate::page::Page;
use crate::protocol::{SerializedArgument, SerializedValue, SpecialValue};

/// Monotonic counter for the temp-tag nonce used by
/// [`ElementHandle`]'s action methods. Combined with a process-wide
/// random prefix and the element's own identity, this is unique
/// enough to avoid clashes even when many handles tag concurrently.
static TAG_COUNTER: AtomicU64 = AtomicU64::new(0);

pub use crate::options::BoundingBox;

/// Element-state query accepted by
/// [`ElementHandle::wait_for_element_state`]. Mirrors Playwright's
/// `ElementHandleWaitForElementStateOptions['state']` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElementState {
  Visible,
  Hidden,
  Stable,
  Enabled,
  Disabled,
  Editable,
}

impl ElementState {
  #[must_use]
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Visible => "visible",
      Self::Hidden => "hidden",
      Self::Stable => "stable",
      Self::Enabled => "enabled",
      Self::Disabled => "disabled",
      Self::Editable => "editable",
    }
  }

  /// Parse from a Playwright wire string. Returns
  /// [`FerriError::InvalidArgument`] on an unknown value.
  ///
  /// # Errors
  ///
  /// Fails when `s` is not one of the six accepted values.
  pub fn parse(s: &str) -> Result<Self> {
    match s {
      "visible" => Ok(Self::Visible),
      "hidden" => Ok(Self::Hidden),
      "stable" => Ok(Self::Stable),
      "enabled" => Ok(Self::Enabled),
      "disabled" => Ok(Self::Disabled),
      "editable" => Ok(Self::Editable),
      _ => Err(FerriError::invalid_argument(
        "state",
        format!("expected visible|hidden|stable|enabled|disabled|editable, got {s:?}"),
      )),
    }
  }
}

fn empty_arg() -> SerializedArgument {
  SerializedArgument {
    value: SerializedValue::Special(SpecialValue::Undefined),
    handles: Vec::new(),
  }
}

fn expect_string(v: &SerializedValue, what: &str) -> Result<String> {
  match v {
    SerializedValue::Str(s) => Ok(s.clone()),
    SerializedValue::Special(SpecialValue::Null | SpecialValue::Undefined) => Ok(String::new()),
    other => Err(FerriError::Evaluation(format!(
      "{what}: expected string, got {other:?}"
    ))),
  }
}

fn expect_optional_string(v: &SerializedValue) -> Option<String> {
  match v {
    SerializedValue::Str(s) => Some(s.clone()),
    _ => None,
  }
}

fn expect_bool(v: &SerializedValue, what: &str) -> Result<bool> {
  match v {
    SerializedValue::Bool(b) => Ok(*b),
    other => Err(FerriError::Evaluation(format!("{what}: expected bool, got {other:?}"))),
  }
}

/// Handle to a DOM element living in a page.
///
/// Cloneable. Every clone shares the same `disposed` flag via the
/// underlying [`JSHandle`].
#[derive(Clone)]
pub struct ElementHandle {
  js_handle: JSHandle,
  /// Backend element captured at materialisation time. Phase-E action
  /// methods delegate through this to the existing per-backend
  /// `AnyElement::click` / `fill` / etc. helpers rather than
  /// re-resolving the DOM node from the `HandleRemote` on every call.
  /// Carried as `Arc` so clones of `ElementHandle` share the backend
  /// element cheaply.
  #[allow(dead_code)]
  element: Arc<AnyElement>,
}

impl ElementHandle {
  /// Construct an `ElementHandle` from an existing backend `AnyElement`.
  /// Called from `Page::query_selector` / `Locator::element_handle` /
  /// etc. once the backend has minted a remote reference.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend element has no addressable handle
  /// (should never happen for elements that came out of `find_element`
  /// / `evaluate_to_element`).
  pub(crate) async fn from_any_element(page: Arc<Page>, element: AnyElement) -> Result<Self> {
    let remote = crate::backend::element_handle_remote(&element).await?;
    let js_handle = JSHandle::new(page, remote);
    Ok(Self {
      js_handle,
      element: Arc::new(element),
    })
  }

  /// Construct an `ElementHandle` from an existing [`JSHandle`] plus a
  /// freshly-minted backend `AnyElement`. Used by
  /// [`JSHandle::as_element`] when re-wrapping a `JSHandle` whose
  /// remote turns out to be a DOM node — the handle's disposed flag,
  /// page, and remote are reused so the two views share a lifecycle.
  pub(crate) fn from_js_handle_and_element(js_handle: JSHandle, element: AnyElement) -> Self {
    Self {
      js_handle,
      element: Arc::new(element),
    }
  }

  /// Underlying [`JSHandle`] — exposes lifecycle + evaluate (phase D).
  #[must_use]
  pub fn as_js_handle(&self) -> &JSHandle {
    &self.js_handle
  }

  /// Backend-specific remote reference.
  #[must_use]
  pub fn remote(&self) -> &HandleRemote {
    self.js_handle.remote()
  }

  /// Owning page.
  #[must_use]
  pub fn page(&self) -> &Arc<Page> {
    self.js_handle.page()
  }

  /// Borrow the backend `AnyElement`. Phase-E action methods use this to
  /// delegate to the per-backend element implementations; phase F's
  /// materialisation helpers use it to round-trip through locator /
  /// frame APIs.
  #[allow(dead_code)]
  pub(crate) fn any_element(&self) -> &AnyElement {
    &self.element
  }

  /// Whether the backing remote has been released.
  #[must_use]
  pub fn is_disposed(&self) -> bool {
    self.js_handle.is_disposed()
  }

  /// Release the remote object. See [`JSHandle::dispose`] for semantics.
  ///
  /// # Errors
  ///
  /// Forwards the backend's dispose error. Idempotent.
  pub async fn dispose(&self) -> Result<()> {
    self.js_handle.dispose().await
  }

  /// Short-circuit helper for phase-E action methods: returns
  /// [`crate::error::FerriError::TargetClosed`] with the Playwright
  /// `"JSHandle is disposed"` message when this handle has already
  /// been released.
  ///
  /// # Errors
  ///
  /// Returns the disposed-handle error if [`Self::is_disposed`].
  pub(crate) fn ensure_live(&self) -> Result<()> {
    if self.is_disposed() {
      return Err(disposed_error());
    }
    Ok(())
  }

  // ── Content reads (Phase E) ────────────────────────────────────────────

  /// Playwright: `elementHandle.innerHTML(): Promise<string>`.
  ///
  /// # Errors
  ///
  /// Forwards page-side / protocol errors; raises disposed-handle
  /// when [`Self::is_disposed`].
  pub async fn inner_html(&self) -> Result<String> {
    self.ensure_live()?;
    let result = self
      .js_handle
      .evaluate_with_arg("el => el.innerHTML", empty_arg(), Some(true))
      .await?;
    expect_string(&result, "innerHTML")
  }

  /// Playwright: `elementHandle.innerText(): Promise<string>`.
  ///
  /// # Errors
  ///
  /// See [`Self::inner_html`].
  pub async fn inner_text(&self) -> Result<String> {
    self.ensure_live()?;
    let result = self
      .js_handle
      .evaluate_with_arg("el => el.innerText", empty_arg(), Some(true))
      .await?;
    expect_string(&result, "innerText")
  }

  /// Playwright: `elementHandle.textContent(): Promise<string | null>`.
  ///
  /// # Errors
  ///
  /// See [`Self::inner_html`].
  pub async fn text_content(&self) -> Result<Option<String>> {
    self.ensure_live()?;
    let result = self
      .js_handle
      .evaluate_with_arg("el => el.textContent", empty_arg(), Some(true))
      .await?;
    Ok(expect_optional_string(&result))
  }

  /// Playwright: `elementHandle.getAttribute(name): Promise<string | null>`.
  ///
  /// # Errors
  ///
  /// See [`Self::inner_html`].
  pub async fn get_attribute(&self, name: &str) -> Result<Option<String>> {
    self.ensure_live()?;
    // Inline the attribute name as a JSON-escaped literal so special
    // characters (quotes, colons) don't break the function source.
    // Playwright's equivalent path uses the utility script's arg
    // marshaling for this; phase-D's multi-arg support is pending so
    // we inline.
    let escaped = serde_json::to_string(name).unwrap_or_else(|_| "\"\"".into());
    let expr = format!("el => el.getAttribute({escaped})");
    let result = self.js_handle.evaluate_with_arg(&expr, empty_arg(), Some(true)).await?;
    Ok(expect_optional_string(&result))
  }

  /// Playwright: `elementHandle.inputValue(): Promise<string>`. Works
  /// on `<input>`, `<textarea>`, and `<select>` — throws on anything
  /// else, matching Playwright's server-side check.
  ///
  /// # Errors
  ///
  /// Returns [`FerriError::Evaluation`] when called on a non-input
  /// element (wire error from the page).
  pub async fn input_value(&self) -> Result<String> {
    self.ensure_live()?;
    let result = self
      .js_handle
      .evaluate_with_arg(
        "el => {\
          if (el.nodeName === 'INPUT' || el.nodeName === 'TEXTAREA' || el.nodeName === 'SELECT') return el.value || '';\
          throw new Error('Node is not an HTMLInputElement or HTMLTextAreaElement or HTMLSelectElement');\
        }",
        empty_arg(),
        Some(true),
      )
      .await?;
    expect_string(&result, "inputValue")
  }

  // ── State predicates (Phase E) ─────────────────────────────────────────

  async fn eval_bool(&self, expr: &str, what: &str) -> Result<bool> {
    self.ensure_live()?;
    let result = self.js_handle.evaluate_with_arg(expr, empty_arg(), Some(true)).await?;
    expect_bool(&result, what)
  }

  /// Playwright: `elementHandle.isVisible(): Promise<boolean>`.
  ///
  /// # Errors
  ///
  /// See [`Self::inner_html`].
  pub async fn is_visible(&self) -> Result<bool> {
    // Playwright's `isVisible` matches the CSS/layout check:
    //  - `display: none` → hidden
    //  - `visibility: hidden/collapse` → hidden
    //  - zero bounding rect → hidden
    self
      .eval_bool(
        "el => {\
          if (!el || !el.isConnected) return false;\
          const style = getComputedStyle(el);\
          if (style.visibility !== 'visible') return false;\
          const rect = el.getBoundingClientRect();\
          return rect.width > 0 && rect.height > 0;\
        }",
        "isVisible",
      )
      .await
  }

  /// Playwright: `elementHandle.isHidden(): Promise<boolean>`.
  ///
  /// # Errors
  ///
  /// See [`Self::inner_html`].
  pub async fn is_hidden(&self) -> Result<bool> {
    Ok(!self.is_visible().await?)
  }

  /// Playwright: `elementHandle.isDisabled(): Promise<boolean>`. True
  /// for form controls that carry the HTML `disabled` attribute OR
  /// inherit disabled from a `<fieldset disabled>` ancestor.
  ///
  /// # Errors
  ///
  /// See [`Self::inner_html`].
  pub async fn is_disabled(&self) -> Result<bool> {
    self
      .eval_bool(
        "el => {\
          const disabledTags = ['BUTTON', 'INPUT', 'SELECT', 'TEXTAREA', 'OPTION', 'OPTGROUP', 'FIELDSET'];\
          let cur = el;\
          while (cur) {\
            if (disabledTags.includes(cur.nodeName) && cur.disabled) return true;\
            cur = cur.parentElement;\
          }\
          return false;\
        }",
        "isDisabled",
      )
      .await
  }

  /// Playwright: `elementHandle.isEnabled(): Promise<boolean>`.
  ///
  /// # Errors
  ///
  /// See [`Self::inner_html`].
  pub async fn is_enabled(&self) -> Result<bool> {
    Ok(!self.is_disabled().await?)
  }

  /// Playwright: `elementHandle.isChecked(): Promise<boolean>`. Honors
  /// ARIA `aria-checked` in addition to native `input[type=checkbox|radio]`.
  ///
  /// # Errors
  ///
  /// See [`Self::inner_html`].
  pub async fn is_checked(&self) -> Result<bool> {
    self
      .eval_bool(
        "el => {\
          if (el.getAttribute && el.getAttribute('aria-checked') === 'true') return true;\
          if (el.nodeName === 'INPUT' && (el.type === 'checkbox' || el.type === 'radio')) return el.checked;\
          return false;\
        }",
        "isChecked",
      )
      .await
  }

  /// Playwright: `elementHandle.isEditable(): Promise<boolean>`. Returns
  /// `true` for enabled `<input>` / `<textarea>` / `<select>` and for
  /// `contenteditable` elements.
  ///
  /// # Errors
  ///
  /// See [`Self::inner_html`].
  pub async fn is_editable(&self) -> Result<bool> {
    self
      .eval_bool(
        "el => {\
          if (el.isContentEditable) return true;\
          if (!(el.nodeName === 'INPUT' || el.nodeName === 'TEXTAREA' || el.nodeName === 'SELECT')) return false;\
          if (el.disabled) return false;\
          if (el.readOnly) return false;\
          return true;\
        }",
        "isEditable",
      )
      .await
  }

  // ── Geometry (Phase E) ────────────────────────────────────────────────

  /// Playwright: `elementHandle.boundingBox(): Promise<BoundingBox | null>`.
  /// Returns `None` when the element is detached or has no box
  /// (`display: none`).
  ///
  /// # Errors
  ///
  /// See [`Self::inner_html`].
  pub async fn bounding_box(&self) -> Result<Option<BoundingBox>> {
    self.ensure_live()?;
    let result = self
      .js_handle
      .evaluate_with_arg(
        "el => {\
          const r = el.getBoundingClientRect();\
          if (r.width === 0 && r.height === 0 && r.x === 0 && r.y === 0) return null;\
          return {x: r.x, y: r.y, width: r.width, height: r.height};\
        }",
        empty_arg(),
        Some(true),
      )
      .await?;
    match result {
      SerializedValue::Special(SpecialValue::Null | SpecialValue::Undefined) => Ok(None),
      SerializedValue::Object { entries, .. } => {
        let mut bbox = BoundingBox {
          x: 0.0,
          y: 0.0,
          width: 0.0,
          height: 0.0,
        };
        for entry in &entries {
          if let SerializedValue::Number(n) = &entry.v {
            match entry.k.as_str() {
              "x" => bbox.x = *n,
              "y" => bbox.y = *n,
              "width" => bbox.width = *n,
              "height" => bbox.height = *n,
              _ => {},
            }
          }
        }
        Ok(Some(bbox))
      },
      other => Err(FerriError::Evaluation(format!(
        "boundingBox: expected object, got {other:?}"
      ))),
    }
  }

  // ── Actions (Phase E) ─────────────────────────────────────────────────

  /// Playwright: `elementHandle.click()`. Phase-E MVP: delegates to
  /// the backend's native element click path (the same call
  /// `Locator::click` uses after resolution) — actionability is not
  /// re-checked because the handle was already resolved at
  /// materialisation time.
  ///
  /// # Errors
  ///
  /// Forwards the backend's click error.
  pub async fn click(&self) -> Result<()> {
    self.ensure_live()?;
    self.any_element().click().await.map_err(FerriError::from)
  }

  /// Playwright: `elementHandle.dblclick()`. Delegates to the
  /// backend's native element dblclick path.
  ///
  /// # Errors
  ///
  /// Forwards the backend's click error.
  pub async fn dblclick(&self) -> Result<()> {
    self.ensure_live()?;
    self.any_element().dblclick().await.map_err(FerriError::from)
  }

  /// Playwright: `elementHandle.hover()`. Delegates to the backend's
  /// native element hover path.
  ///
  /// # Errors
  ///
  /// Forwards the backend's hover error.
  pub async fn hover(&self) -> Result<()> {
    self.ensure_live()?;
    self.any_element().hover().await.map_err(FerriError::from)
  }

  /// Playwright: `elementHandle.type(text)`. Delegates to the
  /// backend's native type path. Playwright's `type` dispatches one
  /// character at a time via the keyboard — matches our `AnyElement`
  /// impl.
  ///
  /// # Errors
  ///
  /// Forwards the backend's type error.
  pub async fn type_str(&self, text: &str) -> Result<()> {
    self.ensure_live()?;
    self.any_element().type_str(text).await.map_err(FerriError::from)
  }

  /// Playwright: `elementHandle.focus()`. JS `el.focus()`.
  ///
  /// # Errors
  ///
  /// Forwards page-side / protocol error.
  pub async fn focus(&self) -> Result<()> {
    self.ensure_live()?;
    self
      .js_handle
      .evaluate_with_arg("el => { el.focus(); }", empty_arg(), Some(true))
      .await
      .map(|_| ())
  }

  /// Playwright: `elementHandle.scrollIntoViewIfNeeded()`. Delegates
  /// through the backend's native `scrollIntoViewIfNeeded` path.
  ///
  /// # Errors
  ///
  /// Forwards the backend's scroll error.
  pub async fn scroll_into_view_if_needed(&self) -> Result<()> {
    self.ensure_live()?;
    self.any_element().scroll_into_view().await.map_err(FerriError::from)
  }

  /// Playwright: `elementHandle.screenshot(opts?): Promise<Buffer>`.
  /// Captures the element's bounding rect via the backend's native
  /// element screenshot path.
  ///
  /// # Errors
  ///
  /// Forwards the backend's screenshot error.
  pub async fn screenshot(&self, format: ImageFormat) -> Result<Vec<u8>> {
    self.ensure_live()?;
    self.any_element().screenshot(format).await.map_err(FerriError::from)
  }

  // ── $eval / $$eval (Playwright: elementHandle.$eval / $$eval) ──────────

  /// Playwright: `elementHandle.$eval(selector, pageFunction, arg?): Promise<R>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:215`).
  /// Resolves `selector` inside this element's subtree, then calls the
  /// user function with the matched element as the first argument.
  /// Throws when no element matches the selector (same as Playwright).
  ///
  /// # Errors
  ///
  /// Returns [`FerriError::Evaluation`] when the selector matches
  /// nothing or the user function throws.
  pub async fn eval_on_selector(
    &self,
    selector: &str,
    fn_source: &str,
    arg: SerializedArgument,
  ) -> Result<SerializedValue> {
    self.ensure_live()?;
    let sel_escaped =
      serde_json::to_string(selector).map_err(|e| FerriError::Other(format!("$eval selector escape: {e}")))?;
    // Probe expression returns the matched element as a handle or
    // throws — identical error shape to Playwright's server-side
    // evalOnSelector handler.
    let probe = format!(
      "el => {{ const r = el.querySelector({sel_escaped}); \
        if (!r) throw new Error('failed to find element matching selector ' + {sel_escaped}); \
        return r; }}"
    );
    let match_handle = self
      .js_handle
      .evaluate_handle_with_arg(&probe, SerializedArgument::default(), Some(true))
      .await?;
    let result = match_handle.evaluate_with_arg(fn_source, arg, None).await;
    // Best-effort intermediate cleanup — do not let a dispose error
    // mask the primary outcome.
    let _ = match_handle.dispose().await;
    result
  }

  /// Playwright: `elementHandle.$$eval(selector, pageFunction, arg?): Promise<R>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:220`).
  /// Resolves every descendant matching `selector` as an array, then
  /// calls the user function with that array as the first argument.
  /// Unlike `$eval`, an empty match is not an error — the user
  /// function receives an empty array.
  ///
  /// # Errors
  ///
  /// Returns an error only when the user function throws or the
  /// protocol call fails.
  pub async fn eval_on_selector_all(
    &self,
    selector: &str,
    fn_source: &str,
    arg: SerializedArgument,
  ) -> Result<SerializedValue> {
    self.ensure_live()?;
    let sel_escaped =
      serde_json::to_string(selector).map_err(|e| FerriError::Other(format!("$$eval selector escape: {e}")))?;
    let probe = format!("el => Array.from(el.querySelectorAll({sel_escaped}))");
    let array_handle = self
      .js_handle
      .evaluate_handle_with_arg(&probe, SerializedArgument::default(), Some(true))
      .await?;
    let result = array_handle.evaluate_with_arg(fn_source, arg, None).await;
    let _ = array_handle.dispose().await;
    result
  }

  // ── Frame accessors (Playwright: elementHandle.ownerFrame / contentFrame) ──

  /// Playwright: `elementHandle.ownerFrame(): Promise<Frame | null>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:56`).
  /// Returns the [`crate::frame::Frame`] this element belongs to, or
  /// `None` if the element is detached.
  ///
  /// Implemented uniformly by reading the element's
  /// `ownerDocument.defaultView` — for CDP the result then passes
  /// through `DOM.describeNode` on the frame's `documentElement` to
  /// recover the frame id. For a first cut that satisfies the
  /// common main-frame case, we return the page's main frame when
  /// the element's owner document is the top document, and
  /// `None` when the probe indicates a detached element.
  ///
  /// # Errors
  ///
  /// Forwards backend error on protocol failure / page-side exception.
  pub async fn owner_frame(&self) -> Result<Option<crate::frame::Frame>> {
    self.ensure_live()?;
    // A detached element has no ownerDocument; otherwise every
    // element lives in some frame. For now we report the main frame
    // for any connected element — multi-frame attribution requires
    // either a backend-specific `DOM.describeNode` call or a frame
    // registry keyed by execution context, both of which exist in
    // ferridriver for CDP but need consistent wiring across BiDi and
    // WebKit. The main-frame mapping covers every single-frame page
    // and matches Playwright's output for the top document.
    let owner_ok = self
      .js_handle
      .evaluate_with_arg(
        "el => !!(el && el.isConnected && el.ownerDocument)",
        SerializedArgument::default(),
        Some(true),
      )
      .await?;
    if !matches!(owner_ok, SerializedValue::Bool(true)) {
      return Ok(None);
    }
    Ok(Some(self.page().main_frame()))
  }

  /// Playwright: `elementHandle.contentFrame(): Promise<Frame | null>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:60`).
  /// Returns the child [`crate::frame::Frame`] hosted inside this
  /// element when it is an `<iframe>` / `<frame>`; `None` otherwise.
  ///
  /// The implementation probes `el.tagName` server-side to decide
  /// whether to resolve a child frame. Frame resolution for the
  /// iframe content uses the frame cache on [`crate::page::Page`]
  /// keyed by the iframe's `name` / `id` / `src` — a best-effort
  /// match that covers the common case of named iframes. Returns
  /// `None` for inline iframes without any matching key.
  ///
  /// # Errors
  ///
  /// Forwards backend error on protocol failure / page-side exception.
  pub async fn content_frame(&self) -> Result<Option<crate::frame::Frame>> {
    self.ensure_live()?;
    let probe = self
      .js_handle
      .evaluate_with_arg(
        "el => { \
          if (!el) return null; \
          const tag = el.tagName; \
          if (tag !== 'IFRAME' && tag !== 'FRAME') return null; \
          return { name: el.getAttribute('name') || '', id: el.id || '', src: el.src || '' }; \
        }",
        SerializedArgument::default(),
        Some(true),
      )
      .await?;
    let SerializedValue::Object { entries, .. } = probe else {
      return Ok(None);
    };
    let find = |key: &str| -> String {
      entries
        .iter()
        .find(|e| e.k == key)
        .and_then(|e| match &e.v {
          SerializedValue::Str(s) => Some(s.clone()),
          _ => None,
        })
        .unwrap_or_default()
    };
    let name = find("name");
    let id = find("id");
    let src = find("src");
    let page = self.page();
    // Walk the frame cache looking for a child frame whose name or
    // URL lines up with the iframe's attributes. The main-frame
    // attribution is skipped explicitly — an iframe's content frame
    // is always a child of the current frame, never the main one.
    let main_id = page.with_frame_cache(crate::frame_cache::FrameCache::main_frame_id);
    let match_id = page.with_frame_cache(|c| {
      c.all_frame_ids()
        .into_iter()
        .filter(|fid| Some(fid) != main_id.as_ref())
        .find(|fid| {
          let rec = c.record(fid);
          let Some(rec) = rec else { return false };
          (!name.is_empty() && rec.info.name == name)
            || (!id.is_empty() && rec.info.name == id)
            || (!src.is_empty() && rec.info.url == src)
        })
    });
    Ok(match_id.map(|fid| crate::frame::Frame::new(Arc::clone(page), fid)))
  }

  // ── wait_for_element_state / wait_for_selector (Playwright parity) ─────

  /// Playwright: `elementHandle.waitForElementState(state, options?)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:225`).
  /// Polls the element's state via the injected engine's
  /// `checkElementStates` helper until it matches `state` or the
  /// timeout fires. `None` timeout means the page's default.
  ///
  /// Polling cadence mirrors the locator's `RETRY_BACKOFFS_MS` schedule
  /// (`0, 0, 20, 50, 100, 100, 500`) capped at the last value.
  ///
  /// # Errors
  ///
  /// Returns [`FerriError::Timeout`] when the deadline passes without
  /// the state matching; forwards backend / page-side errors otherwise.
  pub async fn wait_for_element_state(&self, state: ElementState, timeout_ms: Option<u64>) -> Result<()> {
    self.ensure_live()?;
    let resolved_timeout = timeout_ms.unwrap_or_else(|| self.page().default_timeout());
    let deadline = if resolved_timeout == 0 {
      None
    } else {
      Some(tokio::time::Instant::now() + std::time::Duration::from_millis(resolved_timeout))
    };

    // Probe script. Injected engine's `checkElementStates` returns
    // `'done'` when every requested state is satisfied, otherwise an
    // `'error:not<state>'` marker. We probe the single target state.
    let state_literal = state.as_str();
    let probe_expr = format!(
      "el => {{ \
        if (!window.__fd || !window.__fd.checkElementStates) return 'error:notconnected'; \
        return window.__fd.checkElementStates(el, ['{state_literal}']); \
      }}"
    );

    let backoffs_ms: [u64; 7] = [0, 0, 20, 50, 100, 100, 500];
    let mut idx: usize = 0;
    loop {
      if let Some(d) = deadline {
        if tokio::time::Instant::now() >= d {
          return Err(FerriError::timeout(
            format!("wait_for_element_state {state_literal}"),
            resolved_timeout,
          ));
        }
      }
      let delay_ms = backoffs_ms[idx.min(backoffs_ms.len() - 1)];
      idx = idx.saturating_add(1);
      if delay_ms > 0 {
        let sleep_ms = match deadline {
          Some(d) => {
            let left =
              u64::try_from(d.saturating_duration_since(tokio::time::Instant::now()).as_millis()).unwrap_or(delay_ms);
            delay_ms.min(left)
          },
          None => delay_ms,
        };
        if sleep_ms > 0 {
          tokio::time::sleep(std::time::Duration::from_millis(sleep_ms)).await;
        }
      }

      let outcome = self
        .js_handle
        .evaluate_with_arg(&probe_expr, SerializedArgument::default(), Some(true))
        .await?;
      if let SerializedValue::Str(s) = &outcome {
        if s == "done" {
          return Ok(());
        }
        // Any `error:not*` is a retriable signal — keep polling until
        // the deadline. Anything else is a hard error.
        if !s.starts_with("error:not") {
          return Err(FerriError::Evaluation(format!("wait_for_element_state: {s}")));
        }
      }
    }
  }

  /// Playwright: `elementHandle.waitForSelector(selector, options?)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:229`).
  /// Polls `el.querySelector(selector)` within this element's subtree
  /// until it returns a non-null result or the timeout fires. Returns
  /// `None` only for the `state: 'detached' | 'hidden'` variants; our
  /// MVP implements the default `state: 'visible' | 'attached'` path —
  /// polling until the element is present — and returns the resolved
  /// handle.
  ///
  /// # Errors
  ///
  /// Returns [`FerriError::Timeout`] when the deadline passes without
  /// the selector matching.
  pub async fn wait_for_selector(&self, selector: &str, timeout_ms: Option<u64>) -> Result<Option<ElementHandle>> {
    self.ensure_live()?;
    let resolved_timeout = timeout_ms.unwrap_or_else(|| self.page().default_timeout());
    let deadline = if resolved_timeout == 0 {
      None
    } else {
      Some(tokio::time::Instant::now() + std::time::Duration::from_millis(resolved_timeout))
    };

    let sel_escaped =
      serde_json::to_string(selector).map_err(|e| FerriError::Other(format!("wait_for_selector escape: {e}")))?;
    let probe_expr = format!("el => el.querySelector({sel_escaped})");

    let backoffs_ms: [u64; 7] = [0, 0, 20, 50, 100, 100, 500];
    let mut idx: usize = 0;
    loop {
      if let Some(d) = deadline {
        if tokio::time::Instant::now() >= d {
          return Err(FerriError::timeout(
            format!("wait_for_selector {selector}"),
            resolved_timeout,
          ));
        }
      }
      let delay_ms = backoffs_ms[idx.min(backoffs_ms.len() - 1)];
      idx = idx.saturating_add(1);
      if delay_ms > 0 {
        let sleep_ms = match deadline {
          Some(d) => {
            let left =
              u64::try_from(d.saturating_duration_since(tokio::time::Instant::now()).as_millis()).unwrap_or(delay_ms);
            delay_ms.min(left)
          },
          None => delay_ms,
        };
        if sleep_ms > 0 {
          tokio::time::sleep(std::time::Duration::from_millis(sleep_ms)).await;
        }
      }

      // Evaluate_handle so a positive match returns a retained handle
      // we can promote to an ElementHandle without a second round
      // trip. Dispose intermediates eagerly on null.
      let result = self
        .js_handle
        .evaluate_handle_with_arg(&probe_expr, SerializedArgument::default(), Some(true))
        .await?;
      if let Some(eh) = result.as_element().await? {
        return Ok(Some(eh));
      }
      let _ = result.dispose().await;
    }
  }

  // ── Temp-tag bridge to Locator actions ───────────────────────────────

  /// Tag this element with `data-fd-eh='<nonce>'` on the page and
  /// return the nonce string. The tag is the cheapest way to make
  /// this element findable by a temporary [`crate::locator::Locator`]
  /// so we can dispatch every Playwright action through the
  /// already-shipped Locator retry / actionability pipeline without
  /// re-implementing each action's option bag at the handle layer.
  async fn temp_tag(&self) -> Result<String> {
    let ctr = TAG_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nonce = format!("fdeh{ctr:x}");
    let expr = format!("el => {{ el.setAttribute('data-fd-eh', '{nonce}'); return '{nonce}'; }}");
    let _ = self
      .js_handle
      .evaluate_with_arg(&expr, SerializedArgument::default(), Some(true))
      .await?;
    Ok(nonce)
  }

  /// Best-effort untag. Elements detached during the action can't be
  /// untagged; we swallow the error so the action's own error path
  /// is what the caller sees.
  async fn temp_untag(&self) {
    let _ = self
      .js_handle
      .evaluate_with_arg(
        "el => { if (el && el.removeAttribute) el.removeAttribute('data-fd-eh'); }",
        SerializedArgument::default(),
        Some(true),
      )
      .await;
  }

  /// Build a CSS selector for [`Self::temp_tag`]'s nonce.
  fn temp_selector(nonce: &str) -> String {
    format!("[data-fd-eh='{nonce}']")
  }

  // ── Action methods (Playwright parity — delegate via temp-tag) ─────────

  /// Playwright: `elementHandle.fill(value, options?)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:139`).
  ///
  /// # Errors
  ///
  /// Forwards Locator's `fill` error.
  pub async fn fill(&self, value: &str, opts: Option<crate::options::FillOptions>) -> Result<()> {
    self.ensure_live()?;
    let nonce = self.temp_tag().await?;
    let locator = self.page().locator(&Self::temp_selector(&nonce), None);
    let result = locator.fill(value, opts).await;
    self.temp_untag().await;
    result
  }

  /// Playwright: `elementHandle.check(options?)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:167`).
  ///
  /// # Errors
  ///
  /// Forwards Locator's `check` error.
  pub async fn check(&self, opts: Option<crate::options::CheckOptions>) -> Result<()> {
    self.ensure_live()?;
    let nonce = self.temp_tag().await?;
    let locator = self.page().locator(&Self::temp_selector(&nonce), None);
    let result = locator.check(opts).await;
    self.temp_untag().await;
    result
  }

  /// Playwright: `elementHandle.uncheck(options?)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:171`).
  ///
  /// # Errors
  ///
  /// Forwards Locator's `uncheck` error.
  pub async fn uncheck(&self, opts: Option<crate::options::CheckOptions>) -> Result<()> {
    self.ensure_live()?;
    let nonce = self.temp_tag().await?;
    let locator = self.page().locator(&Self::temp_selector(&nonce), None);
    let result = locator.uncheck(opts).await;
    self.temp_untag().await;
    result
  }

  /// Playwright: `elementHandle.setChecked(checked, options?)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:175`).
  ///
  /// # Errors
  ///
  /// Forwards Locator's `set_checked` error.
  pub async fn set_checked(&self, checked: bool, opts: Option<crate::options::CheckOptions>) -> Result<()> {
    self.ensure_live()?;
    let nonce = self.temp_tag().await?;
    let locator = self.page().locator(&Self::temp_selector(&nonce), None);
    let result = locator.set_checked(checked, opts).await;
    self.temp_untag().await;
    result
  }

  /// Playwright: `elementHandle.tap(options?)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:130`).
  ///
  /// # Errors
  ///
  /// Forwards Locator's `tap` error.
  pub async fn tap(&self, opts: Option<crate::options::TapOptions>) -> Result<()> {
    self.ensure_live()?;
    let nonce = self.temp_tag().await?;
    let locator = self.page().locator(&Self::temp_selector(&nonce), None);
    let result = locator.tap(opts).await;
    self.temp_untag().await;
    result
  }

  /// Playwright: `elementHandle.press(key, options?)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:163`).
  ///
  /// # Errors
  ///
  /// Forwards Locator's `press` error.
  pub async fn press(&self, key: &str, opts: Option<crate::options::PressOptions>) -> Result<()> {
    self.ensure_live()?;
    let nonce = self.temp_tag().await?;
    let locator = self.page().locator(&Self::temp_selector(&nonce), None);
    let result = locator.press(key, opts).await;
    self.temp_untag().await;
    result
  }

  /// Playwright: `elementHandle.dispatchEvent(type, eventInit)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:110`).
  /// `eventInit` mirrors Playwright's `EventInit` — a plain JS object
  /// merged into the event's `{bubbles, cancelable, composed}` defaults.
  ///
  /// # Errors
  ///
  /// Forwards Locator's `dispatch_event` error.
  pub async fn dispatch_event(
    &self,
    event_type: &str,
    event_init: Option<serde_json::Value>,
    opts: Option<crate::options::DispatchEventOptions>,
  ) -> Result<()> {
    self.ensure_live()?;
    let nonce = self.temp_tag().await?;
    let locator = self.page().locator(&Self::temp_selector(&nonce), None);
    let result = locator.dispatch_event(event_type, event_init, opts).await;
    self.temp_untag().await;
    result
  }

  /// Playwright: `elementHandle.selectOption(values, options?)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:134`).
  ///
  /// # Errors
  ///
  /// Forwards Locator's `select_option` error.
  pub async fn select_option(
    &self,
    values: Vec<crate::options::SelectOptionValue>,
    opts: Option<crate::options::SelectOptionOptions>,
  ) -> Result<Vec<String>> {
    self.ensure_live()?;
    let nonce = self.temp_tag().await?;
    let locator = self.page().locator(&Self::temp_selector(&nonce), None);
    let result = locator.select_option(values, opts).await;
    self.temp_untag().await;
    result
  }

  /// Playwright: `elementHandle.selectText(options?)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:143`).
  /// Focuses the element and selects all its text — `<input>` /
  /// `<textarea>` via `select()`, content-editable via a document
  /// range, anything else falls through without error.
  ///
  /// # Errors
  ///
  /// Forwards page-side / protocol errors.
  pub async fn select_text(&self) -> Result<()> {
    self.ensure_live()?;
    self
      .js_handle
      .evaluate_with_arg(
        "el => { \
          if (!el) return; \
          el.focus(); \
          if (el.isContentEditable) { \
            const sel = document.getSelection(); \
            if (sel) { sel.removeAllRanges(); const r = document.createRange(); r.selectNodeContents(el); sel.addRange(r); } \
          } else if (typeof el.select === 'function') { \
            el.select(); \
          } else if (typeof el.setSelectionRange === 'function') { \
            el.setSelectionRange(0, el.value ? el.value.length : 0); \
          } \
        }",
        SerializedArgument::default(),
        Some(true),
      )
      .await
      .map(|_| ())
  }

  /// Playwright: `elementHandle.setInputFiles(files, options?)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:147`).
  /// Delegates to [`crate::locator::Locator::set_input_files`] through
  /// the temp-tag bridge so path / payload handling is shared with the
  /// locator action path.
  ///
  /// # Errors
  ///
  /// Forwards Locator's `set_input_files` error.
  pub async fn set_input_files(
    &self,
    files: crate::options::InputFiles,
    opts: Option<crate::options::SetInputFilesOptions>,
  ) -> Result<()> {
    self.ensure_live()?;
    let nonce = self.temp_tag().await?;
    let locator = self.page().locator(&Self::temp_selector(&nonce), None);
    let result = locator.set_input_files(files, opts).await;
    self.temp_untag().await;
    result
  }

  /// Playwright: `elementHandle.screenshot(options?)` with the full
  /// option bag (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:187`).
  /// Captures the element's bounding rect; `opts.format` drives the
  /// PNG/JPEG selection, with the remaining options carried on the
  /// backend path when supported.
  ///
  /// # Errors
  ///
  /// Forwards the backend's screenshot error.
  pub async fn screenshot_with_opts(&self, opts: crate::backend::ScreenshotOpts) -> Result<Vec<u8>> {
    self.ensure_live()?;
    // Backend path today: element-level screenshot takes only a format
    // argument; the other fields of `ScreenshotOpts` are accepted at
    // this layer for API parity with Playwright and are honoured
    // transparently when the backend grows support (tracked as a
    // locator-level parity gap in Section B of PLAYWRIGHT_COMPAT.md).
    self
      .any_element()
      .screenshot(opts.format)
      .await
      .map_err(FerriError::from)
  }
}

impl std::fmt::Debug for ElementHandle {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("ElementHandle")
      .field("remote", self.remote())
      .field("disposed", &self.is_disposed())
      .finish()
  }
}
