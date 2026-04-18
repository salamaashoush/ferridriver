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

use crate::backend::{AnyElement, ImageFormat};
use crate::error::{FerriError, Result};
use crate::js_handle::{HandleRemote, JSHandle, disposed_error};
use crate::page::Page;
use crate::protocol::{SerializedArgument, SerializedValue, SpecialValue};

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
}

impl std::fmt::Debug for ElementHandle {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("ElementHandle")
      .field("remote", self.remote())
      .field("disposed", &self.is_disposed())
      .finish()
  }
}
