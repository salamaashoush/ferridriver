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

use crate::backend::AnyElement;
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

pub(super) fn empty_arg() -> SerializedArgument {
  SerializedArgument {
    value: SerializedValue::Special(SpecialValue::Undefined),
    handles: Vec::new(),
    functions: Vec::new(),
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
  /// The remote `js_handle` is backed by, held here so the DOM-element
  /// invariant holds by construction rather than by a match on the backing.
  remote: HandleRemote,
  /// Backend element captured at materialisation time. Phase-E action
  /// methods delegate through this to the existing per-backend
  /// `AnyElement::click` / `fill` / etc. helpers rather than
  /// re-resolving the DOM node from the `HandleRemote` on every call.
  /// Carried as `Arc` so clones of `ElementHandle` share the backend
  /// element cheaply.
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
    let js_handle = JSHandle::new(page, remote.clone());
    Ok(Self {
      js_handle,
      remote,
      element: Arc::new(element),
    })
  }

  /// Construct an `ElementHandle` from an existing [`JSHandle`] plus a
  /// freshly-minted backend `AnyElement`. Used by
  /// [`JSHandle::as_element`] when re-wrapping a `JSHandle` whose
  /// remote turns out to be a DOM node — the handle's disposed flag,
  /// page, and remote are reused so the two views share a lifecycle.
  pub(crate) fn from_js_handle_and_element(js_handle: JSHandle, remote: HandleRemote, element: AnyElement) -> Self {
    Self {
      js_handle,
      remote,
      element: Arc::new(element),
    }
  }

  /// Underlying [`JSHandle`] — exposes lifecycle + evaluate (phase D).
  #[must_use]
  pub fn as_js_handle(&self) -> &JSHandle {
    &self.js_handle
  }

  /// Backend-specific remote reference. `ElementHandle` always wraps
  /// a page-side DOM element, so the underlying `JSHandle` is always
  /// remote-backed. Callers that only need to probe the backing's
  /// remote can go directly to `self.as_js_handle().remote()`; this
  /// helper exists for the common case where the DOM-element
  /// invariant is relied upon.
  #[must_use]
  pub fn remote(&self) -> &HandleRemote {
    &self.remote
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
  #[doc(hidden)]
  #[must_use]
  pub fn any_element(&self) -> &AnyElement {
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
      .evaluate("el => el.innerHTML", empty_arg(), Some(true))
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
      .evaluate("el => el.innerText", empty_arg(), Some(true))
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
      .evaluate("el => el.textContent", empty_arg(), Some(true))
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
    let result = self.js_handle.evaluate(&expr, empty_arg(), Some(true)).await?;
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
      .evaluate(
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
    let result = self.js_handle.evaluate(expr, empty_arg(), Some(true)).await?;
    expect_bool(&result, what)
  }

  /// Playwright: `elementHandle.isVisible(): Promise<boolean>`.
  ///
  /// # Errors
  ///
  /// See [`Self::inner_html`].
  pub async fn is_visible(&self) -> Result<bool> {
    self.element_state("visible").await
  }

  /// Playwright: `elementHandle.isHidden(): Promise<boolean>`.
  ///
  /// # Errors
  ///
  /// See [`Self::inner_html`].
  pub async fn is_hidden(&self) -> Result<bool> {
    self.element_state("hidden").await
  }

  /// Playwright: `elementHandle.isDisabled(): Promise<boolean>`: native
  /// `disabled`, a disabled `<fieldset>`, or `aria-disabled` on a role
  /// that takes it.
  ///
  /// # Errors
  ///
  /// See [`Self::inner_html`].
  pub async fn is_disabled(&self) -> Result<bool> {
    self.element_state("disabled").await
  }

  /// Playwright: `elementHandle.isEnabled(): Promise<boolean>`.
  ///
  /// # Errors
  ///
  /// See [`Self::inner_html`].
  pub async fn is_enabled(&self) -> Result<bool> {
    self.element_state("enabled").await
  }

  /// Playwright: `elementHandle.isChecked(): Promise<boolean>`. Honors
  /// ARIA `aria-checked` in addition to native `input[type=checkbox|radio]`.
  ///
  /// # Errors
  ///
  /// See [`Self::inner_html`].
  pub async fn is_checked(&self) -> Result<bool> {
    // Same funnel as `Locator::is_checked`: the injected
    // `getChecked` retargets through `follow-label` and understands
    // every `aria-checked` role, not just `aria-checked="true"` on the
    // element itself.
    let fd = self.page().inner().injected_script().await?;
    self
      .eval_bool(&format!("el => {fd}.getChecked(el) === true"), "isChecked")
      .await
  }

  /// Playwright: `elementHandle.isEditable(): Promise<boolean>`: enabled
  /// and not read-only, `aria-readonly` included. An element that cannot
  /// be edited at all is an error, as upstream.
  ///
  /// # Errors
  ///
  /// See [`Self::inner_html`].
  pub async fn is_editable(&self) -> Result<bool> {
    self.element_state("editable").await
  }

  /// `injected.elementState(el, state)`, through the same engine the
  /// locator form uses.
  async fn element_state(&self, state: &str) -> Result<bool> {
    self.page().inner().ensure_engine_injected().await?;
    self
      .eval_bool(
        &format!(
          "el => window.__fd.elementState(el, {}) === true",
          serde_json::json!(state)
        ),
        state,
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
      .evaluate(
        &format!("el => ({}).call(el)", crate::screenshotter::MAIN_FRAME_BOX_FN),
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
      serde_json::to_string(selector).map_err(|e| FerriError::Backend(format!("$eval selector escape: {e}")))?;
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
      .evaluate_handle(&probe, SerializedArgument::default(), Some(true))
      .await?;
    let result = match_handle.evaluate(fn_source, arg, None).await;
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
      serde_json::to_string(selector).map_err(|e| FerriError::Backend(format!("$$eval selector escape: {e}")))?;
    let probe = format!("el => Array.from(el.querySelectorAll({sel_escaped}))");
    let array_handle = self
      .js_handle
      .evaluate_handle(&probe, SerializedArgument::default(), Some(true))
      .await?;
    let result = array_handle.evaluate(fn_source, arg, None).await;
    let _ = array_handle.dispose().await;
    result
  }

  // ── $ / $$ (Playwright: elementHandle.$ / $$) ─────────────────────────

  /// Playwright: `elementHandle.$(selector): Promise<ElementHandle | null>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:206`).
  /// Resolves the first descendant matching `selector` inside this
  /// element's subtree, returning a pinned [`ElementHandle`] or `None`
  /// when nothing matches.
  ///
  /// # Errors
  ///
  /// Returns [`FerriError::Evaluation`] / backend error on protocol
  /// failure; the disposed-handle error when [`Self::is_disposed`].
  pub async fn query_selector(&self, selector: &str) -> Result<Option<ElementHandle>> {
    self.ensure_live()?;
    let sel_escaped = serde_json::to_string(selector)
      .map_err(|e| FerriError::Backend(format!("query_selector selector escape: {e}")))?;
    let expr = format!("el => el.querySelector({sel_escaped})");
    let handle = self
      .js_handle
      .evaluate_handle(&expr, SerializedArgument::default(), Some(true))
      .await?;
    if let Some(eh) = handle.as_element() {
      return Ok(Some(eh));
    }
    // Null match (or a non-node, which querySelector never yields) —
    // release the intermediate handle and report no match.
    let _ = handle.dispose().await;
    Ok(None)
  }

  /// Playwright: `elementHandle.$$(selector): Promise<ElementHandle[]>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:210`).
  /// Resolves every descendant matching `selector` in document order,
  /// returning one pinned [`ElementHandle`] per match. An empty match
  /// is not an error.
  ///
  /// # Errors
  ///
  /// Returns [`FerriError::Evaluation`] / backend error on protocol
  /// failure; the disposed-handle error when [`Self::is_disposed`].
  pub async fn query_selector_all(&self, selector: &str) -> Result<Vec<ElementHandle>> {
    self.ensure_live()?;
    let sel_escaped = serde_json::to_string(selector)
      .map_err(|e| FerriError::Backend(format!("query_selector_all selector escape: {e}")))?;
    let expr = format!("el => Array.from(el.querySelectorAll({sel_escaped}))");
    let array_handle = self
      .js_handle
      .evaluate_handle(&expr, SerializedArgument::default(), Some(true))
      .await?;
    // The array's own enumerable properties are its numeric indices;
    // each indexed property handle is a DOM element. Non-element
    // properties (e.g. `length`, which is non-enumerable on arrays and
    // thus excluded) are skipped via `as_element`.
    let props = array_handle.get_properties().await?;
    let mut out = Vec::with_capacity(props.len());
    for (_key, prop) in props {
      match prop.as_element() {
        Some(eh) => out.push(eh),
        None => {
          let _ = prop.dispose().await;
        },
      }
    }
    let _ = array_handle.dispose().await;
    Ok(out)
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
      .evaluate(
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
  /// Resolution is deterministic on every backend: CDP and `WebKit` map
  /// the element to its `contentFrameId` via `DOM.describeNode`, `BiDi`
  /// reads the `contentWindow`'s browsing-context id. The frame-cache
  /// name/`id`/`src` heuristic remains only as a last-resort fallback
  /// (a backend that ever fails to describe the node), covering unnamed
  /// / `srcdoc` / `data:` iframes through the protocol paths above.
  ///
  /// # Errors
  ///
  /// Forwards backend error on protocol failure / page-side exception.
  pub async fn content_frame(&self) -> Result<Option<crate::frame::Frame>> {
    self.ensure_live()?;
    if let AnyElement::WebDriver(element) = &*self.element {
      return Ok(
        element
          .page
          .content_frame_id(&element.id)
          .await?
          .map(|frame_id| crate::frame::Frame::new(std::sync::Arc::clone(self.page()), std::sync::Arc::from(frame_id))),
      );
    }
    // Deterministic first: CDP and WebKit `DOM.describeNode` map the
    // iframe element to its real content-frame id (robust for unnamed
    // / `srcdoc` / `data:` / churned iframes). Backends without it
    // return `None` and we fall through to the name/url cache
    // heuristic below.
    let describe_obj = match self.js_handle.remote() {
      Some(
        crate::js_handle::HandleRemote::Cdp { object_id: obj, .. } | crate::js_handle::HandleRemote::WebKit(obj),
      ) => Some(obj),
      _ => None,
    };
    if let Some(obj) = describe_obj
      && let Ok(Some(fid)) = self.page().inner().content_frame_id(obj).await
    {
      return Ok(Some(crate::frame::Frame::new(
        std::sync::Arc::clone(self.page()),
        std::sync::Arc::from(fid.as_str()),
      )));
    }
    // BiDi deterministic path: the iframe's `contentWindow` serialises
    // with its child browsing-context id (works for srcdoc / data: /
    // nested / re-attached, unlike the name/url heuristic).
    if let AnyElement::Bidi(be) = &*self.element
      && let Ok(Some(ctx)) = be.content_frame_context().await
    {
      return Ok(Some(crate::frame::Frame::new(
        std::sync::Arc::clone(self.page()),
        std::sync::Arc::from(ctx.as_str()),
      )));
    }
    let probe = self
      .js_handle
      .evaluate(
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
      c.all_frame_ids().into_iter().filter(|fid| *fid != main_id).find(|fid| {
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
    crate::operation_budget::scoped(
      resolved_timeout,
      || format!("wait_for_element_state {state_literal}"),
      async move {
        loop {
          if let Some(d) = deadline
            && tokio::time::Instant::now() >= d
          {
            return Err(FerriError::timeout(
              format!("wait_for_element_state {state_literal}"),
              resolved_timeout,
            ));
          }
          let delay_ms = backoffs_ms[idx.min(backoffs_ms.len() - 1)];
          idx = idx.saturating_add(1);
          if delay_ms > 0 {
            let sleep_ms = match deadline {
              Some(d) => {
                let left = u64::try_from(d.saturating_duration_since(tokio::time::Instant::now()).as_millis())
                  .unwrap_or(delay_ms);
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
            .evaluate(&probe_expr, SerializedArgument::default(), Some(true))
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
      },
    )
    .await
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
      serde_json::to_string(selector).map_err(|e| FerriError::Backend(format!("wait_for_selector escape: {e}")))?;
    let probe_expr = format!("el => el.querySelector({sel_escaped})");

    let backoffs_ms: [u64; 7] = [0, 0, 20, 50, 100, 100, 500];
    let mut idx: usize = 0;
    crate::operation_budget::scoped(
      resolved_timeout,
      || format!("wait_for_selector {selector}"),
      async move {
        loop {
          if let Some(d) = deadline
            && tokio::time::Instant::now() >= d
          {
            return Err(FerriError::timeout(
              format!("wait_for_selector {selector}"),
              resolved_timeout,
            ));
          }
          let delay_ms = backoffs_ms[idx.min(backoffs_ms.len() - 1)];
          idx = idx.saturating_add(1);
          if delay_ms > 0 {
            let sleep_ms = match deadline {
              Some(d) => {
                let left = u64::try_from(d.saturating_duration_since(tokio::time::Instant::now()).as_millis())
                  .unwrap_or(delay_ms);
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
            .evaluate_handle(&probe_expr, SerializedArgument::default(), Some(true))
            .await?;
          if let Some(eh) = result.as_element() {
            return Ok(Some(eh));
          }
          let _ = result.dispose().await;
        }
      },
    )
    .await
  }

  // ── Temp-tag bridge to Locator actions ───────────────────────────────

  /// Tag this element with `data-fd-eh='<nonce>'` on the page and
  /// return the nonce string. The tag is the cheapest way to make
  /// this element findable by a temporary [`crate::locator::Locator`]
  /// so we can dispatch every Playwright action through the
  /// already-shipped Locator retry / actionability pipeline without
  /// re-implementing each action's option bag at the handle layer.
  pub(super) async fn temp_tag(&self) -> Result<String> {
    let ctr = TAG_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nonce = format!("fdeh{ctr:x}");
    let expr = format!("el => {{ el.setAttribute('data-fd-eh', '{nonce}'); return '{nonce}'; }}");
    let _ = self
      .js_handle
      .evaluate(&expr, SerializedArgument::default(), Some(true))
      .await?;
    Ok(nonce)
  }

  /// Best-effort untag. Elements detached during the action can't be
  /// untagged; we swallow the error so the action's own error path
  /// is what the caller sees.
  pub(super) async fn temp_untag(&self) {
    let _ = self
      .js_handle
      .evaluate(
        "el => { if (el && el.removeAttribute) el.removeAttribute('data-fd-eh'); }",
        SerializedArgument::default(),
        Some(true),
      )
      .await;
  }

  /// Build a CSS selector for [`Self::temp_tag`]'s nonce.
  pub(super) fn temp_selector(nonce: &str) -> String {
    format!("[data-fd-eh='{nonce}']")
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

mod actions;
