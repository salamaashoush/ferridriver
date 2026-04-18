//! `JSHandle` — lifecycle object for an arbitrary JavaScript value in the page.
//!
//! Mirrors Playwright's `JSHandle` class
//! (`/tmp/playwright/packages/playwright-core/src/client/jsHandle.ts`). A handle
//! holds a backend-agnostic reference to a value that lives in the page (CDP
//! `Runtime.RemoteObjectId`, `BiDi` `sharedId`, or `WebKit` `window.__wr[id]`
//! index), plus the `Arc<Page>` the value was minted against. Callers can
//! pass the handle back into evaluate/eval-family calls or release the
//! underlying remote object via [`JSHandle::dispose`].
//!
//! ## Lifecycle contract
//!
//! - Every handle is created on exactly one page / execution context.
//! - `dispose()` is idempotent — first call releases, subsequent calls are
//!   no-ops.
//! - After dispose, any method that talks to the remote returns
//!   [`crate::error::FerriError::TargetClosed`] (Playwright raises
//!   `JavaScriptErrorInEvaluate` from the server for the same condition;
//!   we surface `TargetClosed` because the handle's target — the remote
//!   object — is gone).
//!
//! Not thread-local: handles are `Clone`, `Send`, and `Sync` so they can
//! flow through the `evaluate(fn, arg)` wire serialization just like any
//! other public type.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::backend::AnyPage;
use crate::element_handle::ElementHandle;
use crate::error::{FerriError, Result};
use crate::page::Page;
use crate::protocol::HandleId;

/// Backend-specific handle payload. Carries only the wire-level identifier;
/// the session/context/view is recovered from the owning `Page` at dispose /
/// evaluate time. Not public — callers interact via [`JSHandle`] and
/// [`ElementHandle`].
///
/// Each variant maps 1:1 onto the corresponding `protocol::HandleId` wire
/// variant — [`HandleRemote::to_handle_id`] converts one to the other at
/// the `evaluate(fn, arg)` serialization boundary.
#[derive(Debug, Clone)]
pub enum HandleRemote {
  /// CDP `Runtime.RemoteObjectId`. Released via `Runtime.releaseObject`.
  Cdp(Arc<str>),
  /// `BiDi` `SharedReference.sharedId` (plus optional `handle` field).
  /// Released via `script.disown`.
  Bidi { shared_id: String, handle: Option<String> },
  /// `WebKit` host IPC ref — the `ref_id` used to index `window.__wr`.
  /// Released via the new `Op::ReleaseRef` IPC op.
  WebKit(u64),
}

/// Backing of a [`JSHandle`] returned from `evaluateHandle`. Mirrors
/// Playwright's two-shape `JSHandle` constructor
/// (`/tmp/playwright/packages/playwright-core/src/server/javascript.ts:120-139`):
/// a retained remote reference OR a primitive `_value` for non-object
/// results.
///
/// Remote-backed handles live on the page and need `dispose()` to
/// release them; value-backed handles carry an inline
/// [`crate::protocol::SerializedValue`] and their `dispose()` is a
/// no-op because nothing is retained page-side.
#[derive(Debug, Clone)]
pub enum JSHandleBacking {
  /// Remote reference — `Runtime.RemoteObjectId`, `BiDi` shared-id /
  /// handle, `WebKit` `window.__wr` index.
  Remote(HandleRemote),
  /// Primitive value — mirrors Playwright's `JSHandle._value`. Returned
  /// when the backend's evaluateHandle path observes a non-object
  /// result (number / string / boolean / null / undefined); the value
  /// rides inline through the handle rather than requiring a page-side
  /// retained reference.
  Value(crate::protocol::SerializedValue),
}

/// Outcome of calling the utility script's `evaluate()` method through
/// one of the backends. When the caller requested `returnByValue=true`
/// (or Playwright parity's `page.evaluate(fn, arg)`), the backend parses
/// the returned `RemoteObject.value` into a [`Value`] variant.
/// When `returnByValue=false` (Playwright's `page.evaluateHandle`), the
/// result wraps in a [`Handle`] variant whose backing is either a
/// retained remote reference or an inline primitive value — matching
/// Playwright's dual `JSHandle` shape.
#[derive(Debug, Clone)]
pub enum EvaluateResult {
  /// The utility script ran with `returnByValue=true`; the page-side
  /// `UtilityScript.jsonValue` serialised the result back through the
  /// isomorphic wire format. Exceptions inside the user function
  /// surface as [`crate::error::FerriError::Evaluation`] from the
  /// enclosing backend call.
  Value(crate::protocol::SerializedValue),
  /// The utility script ran with `returnByValue=false`; the result is
  /// either a retained remote object addressable via
  /// [`JSHandleBacking::Remote`] or a primitive
  /// [`JSHandleBacking::Value`] when the result has no object identity.
  /// Callers typically wrap this in a [`JSHandle`] /
  /// [`crate::element_handle::ElementHandle`].
  Handle(JSHandleBacking),
}

impl HandleRemote {
  /// Convert to the serialization-boundary [`HandleId`] form used by the
  /// protocol wire serializer. The two types exist separately so the
  /// internal `HandleRemote` can carry `Arc<str>` / owned strings
  /// optimized for local cloning, while `HandleId` stays serde-native for
  /// the wire path.
  #[must_use]
  pub fn to_handle_id(&self) -> HandleId {
    match self {
      Self::Cdp(obj) => HandleId::Cdp((**obj).to_string()),
      Self::Bidi { shared_id, handle } => HandleId::Bidi {
        shared_id: shared_id.clone(),
        handle: handle.clone(),
      },
      Self::WebKit(ref_id) => HandleId::WebKit(*ref_id),
    }
  }

  /// Inverse of [`Self::to_handle_id`]. Returns a [`HandleRemote`] ready
  /// to dispatch against an `AnyPage`. The conversion is lossless.
  #[must_use]
  pub fn from_handle_id(id: HandleId) -> Self {
    match id {
      HandleId::Cdp(obj) => Self::Cdp(Arc::from(obj)),
      HandleId::Bidi { shared_id, handle } => Self::Bidi { shared_id, handle },
      HandleId::WebKit(ref_id) => Self::WebKit(ref_id),
    }
  }
}

impl JSHandleBacking {
  /// Package this backing as a single-slot [`crate::protocol::SerializedArgument`].
  /// Remote-backed handles ride through as `{h: 0}` with their
  /// [`HandleId`] in `handles[0]`; value-backed handles inline their
  /// primitive as the wire `value` with no entry in `handles`. Matches
  /// Playwright's `serializeArgument` behaviour in
  /// `/tmp/playwright/packages/playwright-core/src/client/jsHandle.ts:91-102`
  /// where `JSHandle._value` bypasses the handle table.
  ///
  /// The canonical packaging lives on core so NAPI and `QuickJS`
  /// bindings produce identical wire shapes for the same handle —
  /// per the Rule-1 "Rust is source of truth; bindings are thin
  /// mirrors" invariant.
  #[must_use]
  pub fn to_serialized_argument(&self) -> crate::protocol::SerializedArgument {
    match self {
      Self::Remote(remote) => crate::protocol::SerializedArgument {
        value: crate::protocol::SerializedValue::Handle(0),
        handles: vec![remote.to_handle_id()],
      },
      Self::Value(v) => crate::protocol::SerializedArgument {
        value: v.clone(),
        handles: Vec::new(),
      },
    }
  }
}

/// Handle to a JavaScript value living in a page, or a primitive held
/// inline. Mirrors Playwright's dual `JSHandle` shape — remote-backed
/// when the value has page-side identity (objects, arrays, DOM nodes)
/// and value-backed when the result is a primitive.
///
/// Cheaply cloneable — every clone shares the same `disposed` flag so
/// the first `dispose()` wins. Remote-backed handles release on first
/// dispose; value-backed handles treat dispose as a no-op.
#[derive(Clone)]
pub struct JSHandle {
  page: Arc<Page>,
  backing: JSHandleBacking,
  disposed: Arc<AtomicBool>,
}

impl JSHandle {
  /// Construct a remote-backed handle. Internal — callers go through
  /// page factories like `Page::query_selector` (`ElementHandle`) or
  /// `Page::evaluate_handle` (`JSHandle`).
  pub(crate) fn new(page: Arc<Page>, remote: HandleRemote) -> Self {
    Self::from_backing(page, JSHandleBacking::Remote(remote))
  }

  /// Construct directly from an already-built [`JSHandleBacking`] —
  /// the shape produced by `EvaluateResult::Handle(..)`. Callers that
  /// need a value-backed handle pass `JSHandleBacking::Value(..)` here.
  pub(crate) fn from_backing(page: Arc<Page>, backing: JSHandleBacking) -> Self {
    Self {
      page,
      backing,
      disposed: Arc::new(AtomicBool::new(false)),
    }
  }

  /// The owning page.
  #[must_use]
  pub fn page(&self) -> &Arc<Page> {
    &self.page
  }

  /// Raw backend reference — `Some(..)` for remote-backed handles,
  /// `None` for value-backed ones (Playwright's `_value` shape).
  #[must_use]
  pub fn remote(&self) -> Option<&HandleRemote> {
    match &self.backing {
      JSHandleBacking::Remote(r) => Some(r),
      JSHandleBacking::Value(_) => None,
    }
  }

  /// Inline primitive — `Some(..)` for value-backed handles, `None`
  /// for remote-backed ones.
  #[must_use]
  pub fn value(&self) -> Option<&crate::protocol::SerializedValue> {
    match &self.backing {
      JSHandleBacking::Value(v) => Some(v),
      JSHandleBacking::Remote(_) => None,
    }
  }

  /// Full backing — lets callers pattern-match over remote vs value
  /// without going through two `Option` accessors.
  #[must_use]
  pub fn backing(&self) -> &JSHandleBacking {
    &self.backing
  }

  /// `true` once [`Self::dispose`] has run for any clone of this handle.
  #[must_use]
  pub fn is_disposed(&self) -> bool {
    self.disposed.load(Ordering::SeqCst)
  }

  /// Borrow the `AnyPage` for backend dispatch. `pub(crate)` because the
  /// public Page API doesn't expose `AnyPage`.
  pub(crate) fn any_page(&self) -> &AnyPage {
    self.page.inner()
  }

  /// Claim the disposed flag. Returns `true` on the first call per handle
  /// graph, `false` thereafter. Internal — used to short-circuit
  /// idempotent dispose.
  fn claim_dispose(&self) -> bool {
    self
      .disposed
      .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
      .is_ok()
  }

  /// Release the underlying remote object on the backend, when there
  /// is one. Value-backed handles have nothing to release — dispose
  /// latches the flag but makes no backend call.
  ///
  /// - CDP: `Runtime.releaseObject { objectId }`.
  /// - `BiDi`: `script.disown { handles, target }`.
  /// - `WebKit`: `Op::ReleaseRef` over IPC — deletes the entry from the
  ///   host's `window.__wr` map.
  ///
  /// Idempotent — first call wins; later calls on any clone return
  /// `Ok(())` without a backend round-trip.
  ///
  /// # Errors
  ///
  /// Forwards the backend's dispose error if the protocol call fails.
  /// On a genuine failure the `disposed` flag is rolled back so the
  /// caller can retry; on success the flag is latched and every
  /// subsequent call short-circuits without a backend round-trip.
  pub async fn dispose(&self) -> Result<()> {
    if !self.claim_dispose() {
      return Ok(());
    }
    let JSHandleBacking::Remote(remote) = &self.backing else {
      // Value-backed handle: no page-side reference to release. The
      // disposed flag is latched so subsequent calls short-circuit.
      return Ok(());
    };
    let result = self.any_page().release_handle(remote).await;
    if result.is_err() {
      // Roll back the flag so the caller can retry the failed release.
      // Idempotence is preserved on success because the flag stays
      // latched; only failures un-latch.
      self.disposed.store(false, Ordering::SeqCst);
    }
    result
  }

  /// Playwright: `jsHandle.evaluate(pageFunction, arg?): Promise<R>`.
  /// Matches Playwright's call site
  /// (`/tmp/playwright/packages/playwright-core/src/server/javascript.ts:161`):
  /// `evaluate(ctx, true, pageFunction, this, arg)` — the handle and
  /// the user arg are the first two positional arguments to the user
  /// function, which receives `(handleValue, userArg) => ...`.
  ///
  /// # Errors
  ///
  /// Forwards backend error on protocol failure / page-side exception,
  /// and [`crate::error::FerriError::TargetClosed`] when this handle
  /// is already disposed.
  pub async fn evaluate(
    &self,
    fn_source: &str,
    user_arg: crate::protocol::SerializedArgument,
    is_function: Option<bool>,
  ) -> Result<crate::protocol::SerializedValue> {
    if self.is_disposed() {
      return Err(disposed_error());
    }
    let (args, handles) = build_handle_evaluate_args(&self.backing, user_arg);
    let result = self
      .any_page()
      .call_utility_evaluate(fn_source, &args, &handles, None, is_function, true)
      .await?;
    match result {
      EvaluateResult::Value(v) => Ok(v),
      EvaluateResult::Handle(_) => Err(crate::error::FerriError::Evaluation(
        "JSHandle::evaluate: backend returned handle in returnByValue=true mode".into(),
      )),
    }
  }

  /// Playwright: `jsHandle.evaluateHandle(pageFunction, arg?): Promise<JSHandle>`.
  /// Same wire path as [`Self::evaluate`] but retains the result on
  /// the page (or inlines primitives as `JSHandleBacking::Value`) and
  /// hands back a fresh [`JSHandle`].
  ///
  /// # Errors
  ///
  /// See [`Self::evaluate`].
  pub async fn evaluate_handle(
    &self,
    fn_source: &str,
    user_arg: crate::protocol::SerializedArgument,
    is_function: Option<bool>,
  ) -> Result<JSHandle> {
    if self.is_disposed() {
      return Err(disposed_error());
    }
    let (args, handles) = build_handle_evaluate_args(&self.backing, user_arg);
    let result = self
      .any_page()
      .call_utility_evaluate(fn_source, &args, &handles, None, is_function, false)
      .await?;
    match result {
      EvaluateResult::Handle(backing) => Ok(JSHandle::from_backing(Arc::clone(&self.page), backing)),
      EvaluateResult::Value(_) => Err(crate::error::FerriError::Evaluation(
        "JSHandle::evaluate_handle: backend returned value in returnByValue=false mode".into(),
      )),
    }
  }

  /// Playwright: `jsHandle.jsonValue(): Promise<T>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/jsHandle.ts:61`).
  /// Returns a JSON-like projection of the remote value — matches
  /// Playwright's server-side `_jsonValue` which short-circuits to the
  /// inline `_value` for primitive-backed handles and runs
  /// `utilityScript.jsonValue` page-side for remote-backed ones.
  ///
  /// # Errors
  ///
  /// Forwards backend error on protocol failure / page-side exception,
  /// and [`crate::error::FerriError::TargetClosed`] when this handle
  /// is already disposed.
  pub async fn json_value(&self) -> Result<crate::protocol::SerializedValue> {
    // Playwright's `_jsonValue` short-circuits to the inline value
    // when the handle lacks an objectId
    // (`/tmp/playwright/packages/playwright-core/src/server/javascript.ts:199-204`).
    // We mirror that — value-backed handles return the stored value
    // without a page round-trip.
    if let Some(v) = self.value() {
      return Ok(v.clone());
    }
    self
      .evaluate("h => h", crate::protocol::SerializedArgument::default(), Some(true))
      .await
  }

  /// Playwright: `jsHandle.getProperty(propertyName): Promise<JSHandle>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/jsHandle.ts:49`).
  /// Returns a [`JSHandle`] for the named own property. Matches
  /// Playwright's server-side `_getProperty` semantics by evaluating
  /// `h => h[propertyName]` with `propertyName` inlined as a
  /// JSON-escaped literal.
  ///
  /// # Errors
  ///
  /// Forwards backend error on protocol failure / page-side exception,
  /// and [`crate::error::FerriError::TargetClosed`] when this handle
  /// is already disposed.
  pub async fn get_property(&self, name: &str) -> Result<JSHandle> {
    let escaped =
      serde_json::to_string(name).map_err(|e| FerriError::Other(format!("getProperty name escape: {e}")))?;
    let expr = format!("h => h[{escaped}]");
    self
      .evaluate_handle(&expr, crate::protocol::SerializedArgument::default(), Some(true))
      .await
  }

  /// Playwright: `jsHandle.getProperties(): Promise<Map<string, JSHandle>>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/jsHandle.ts:54`).
  /// Returns every own enumerable string-keyed property as a
  /// `(name, handle)` pair. Uses a two-phase evaluate to stay backend-
  /// agnostic: first enumerate the keys, then mint a handle per key.
  /// Callers are responsible for disposing each returned handle when
  /// they're done with it.
  ///
  /// # Errors
  ///
  /// Forwards backend error on protocol failure / page-side exception,
  /// and [`crate::error::FerriError::TargetClosed`] when this handle
  /// is already disposed.
  pub async fn get_properties(&self) -> Result<Vec<(String, JSHandle)>> {
    use crate::protocol::SerializedValue;
    let keys_value = self
      .evaluate(
        "h => (h && typeof h === 'object') ? Object.keys(h) : []",
        crate::protocol::SerializedArgument::default(),
        Some(true),
      )
      .await?;
    let keys: Vec<String> = match keys_value {
      SerializedValue::Array { items, .. } => items
        .into_iter()
        .filter_map(|v| match v {
          SerializedValue::Str(s) => Some(s),
          _ => None,
        })
        .collect(),
      _ => Vec::new(),
    };
    let mut out = Vec::with_capacity(keys.len());
    for key in keys {
      let handle = self.get_property(&key).await?;
      out.push((key, handle));
    }
    Ok(out)
  }

  /// Playwright: `jsHandle.asElement(): ElementHandle | null`
  /// (`/tmp/playwright/packages/playwright-core/src/client/jsHandle.ts:65`).
  /// Inspects the remote value — returns `Some(ElementHandle)` if it
  /// is a DOM `Node`, `None` otherwise. Implemented via an
  /// `h => h instanceof Node` probe so every backend resolves the
  /// check page-side uniformly; on a true result the handle's remote
  /// is re-wrapped into a backend `AnyElement` and packaged as a
  /// [`ElementHandle`].
  ///
  /// # Errors
  ///
  /// Forwards backend error on protocol failure / page-side exception,
  /// and [`crate::error::FerriError::TargetClosed`] when this handle
  /// is already disposed.
  pub async fn as_element(&self) -> Result<Option<ElementHandle>> {
    if self.is_disposed() {
      return Ok(None);
    }
    // Value-backed handles are primitive — never DOM nodes. Short-circuit
    // without a page round-trip, matching Playwright's `asElement`
    // which returns `null` at the base `JSHandle` layer.
    let Some(remote) = self.remote() else {
      return Ok(None);
    };
    let is_node = self
      .evaluate(
        "h => h instanceof Node",
        crate::protocol::SerializedArgument::default(),
        Some(true),
      )
      .await?;
    let is_element = matches!(is_node, crate::protocol::SerializedValue::Bool(true));
    if !is_element {
      return Ok(None);
    }
    let any_element = crate::backend::element_from_remote(self.any_page(), remote)?;
    Ok(Some(ElementHandle::from_js_handle_and_element(
      self.clone(),
      any_element,
    )))
  }
}

impl std::fmt::Debug for JSHandle {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("JSHandle")
      .field("backing", &self.backing)
      .field("disposed", &self.is_disposed())
      .finish_non_exhaustive()
  }
}

/// Pack `handle.evaluate(fn, userArg)` as two positional args — the
/// handle at index 0 (as `{h: 0}` for remote-backed receivers, or as
/// the inline value for value-backed receivers) and the user arg at
/// index 1 with its `{h: i}` references relocated to `{h: i+1}` to
/// sit alongside the prepended handle. Mirrors Playwright's
/// `JSHandle.evaluate`'s call site at
/// `/tmp/playwright/packages/playwright-core/src/server/javascript.ts:161-163`
/// where the handle and user arg are passed as the first two variadic
/// arguments.
fn build_handle_evaluate_args(
  receiver: &JSHandleBacking,
  user_arg: crate::protocol::SerializedArgument,
) -> (Vec<crate::protocol::SerializedValue>, Vec<crate::protocol::HandleId>) {
  let crate::protocol::SerializedArgument {
    value: user_value,
    handles: user_handles,
  } = user_arg;

  if let JSHandleBacking::Value(inline) = receiver {
    // Value-backed receiver: no page-side reference to ship. Pass the
    // primitive inline as arg[0]; user arg keeps its own handle table
    // unshifted because no receiver handle was prepended.
    let args = vec![inline.clone(), user_value];
    return (args, user_handles);
  }
  let JSHandleBacking::Remote(remote) = receiver else {
    unreachable!("JSHandleBacking has only Remote and Value variants");
  };

  // Relocate `{h: i}` refs inside the user value by +1 so they index
  // into the combined handle table where `handles[0]` is the receiver.
  let shifted_user_value = shift_handle_indices(user_value, 1);

  let args = vec![crate::protocol::SerializedValue::handle(0), shifted_user_value];
  let mut handles = Vec::with_capacity(1 + user_handles.len());
  handles.push(remote.to_handle_id());
  handles.extend(user_handles);
  (args, handles)
}

/// Walk a [`crate::protocol::SerializedValue`] tree and shift every
/// `{h: idx}` reference by `offset`. Used when merging a user-arg
/// sub-tree into a larger multi-arg evaluate call whose shared
/// `handles` list starts with pre-existing receiver entries. Other
/// nodes pass through unchanged.
fn shift_handle_indices(value: crate::protocol::SerializedValue, offset: u32) -> crate::protocol::SerializedValue {
  use crate::protocol::{PropertyEntry, SerializedValue};
  match value {
    SerializedValue::Handle(i) => SerializedValue::Handle(i + offset),
    SerializedValue::Array { id, items } => SerializedValue::Array {
      id,
      items: items.into_iter().map(|v| shift_handle_indices(v, offset)).collect(),
    },
    SerializedValue::Object { id, entries } => SerializedValue::Object {
      id,
      entries: entries
        .into_iter()
        .map(|e| PropertyEntry {
          k: e.k,
          v: shift_handle_indices(e.v, offset),
        })
        .collect(),
    },
    other => other,
  }
}

/// Error raised when a caller tries to use a `JSHandle` / `ElementHandle`
/// whose underlying remote has been released.
///
/// Matches Playwright's message text — the server's
/// `JavaScriptErrorInEvaluate` carries `"JSHandle is disposed"` in the
/// same situation. Consumers that dispatch on error content can match
/// the substring without coupling to a dedicated `FerriError` variant.
pub(crate) fn disposed_error() -> FerriError {
  FerriError::TargetClosed {
    reason: Some("JSHandle is disposed".to_string()),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn handle_remote_roundtrips_through_handle_id() {
    let cases = [
      HandleRemote::Cdp(Arc::from("obj-42")),
      HandleRemote::Bidi {
        shared_id: "shared-42".into(),
        handle: Some("h-1".into()),
      },
      HandleRemote::Bidi {
        shared_id: "shared-43".into(),
        handle: None,
      },
      HandleRemote::WebKit(42),
    ];
    for original in cases {
      let id = original.to_handle_id();
      let back = HandleRemote::from_handle_id(id);
      // PartialEq not derived (Arc<str> comparison quirks), compare by
      // stringifying via Debug.
      assert_eq!(format!("{original:?}"), format!("{back:?}"));
    }
  }

  #[test]
  fn disposed_error_message_matches_playwright() {
    let e = disposed_error();
    assert!(e.to_string().contains("JSHandle is disposed"), "message drift: {e}");
    assert_eq!(e.name(), "TargetClosedError");
  }
}
