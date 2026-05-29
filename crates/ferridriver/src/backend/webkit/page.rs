//! Playwright `WebKit` page handle — the full ferridriver page API over
//! the PW `WebKit` Inspector protocol.
//!
//! A page owns three [`Session`]s: the root browser session (for
//! `Playwright.navigate` / screenshot), the page-proxy session (for
//! `Target.*` / `Dialog.*`), and the inner target session (for `Page.*`
//! / `Runtime.*` / `DOM.*` / `Network.*` / `Input.*`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use arc_swap::ArcSwap;
use base64::Engine as _;
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;

use super::browser::{BrowserError, WebKitBrowser};
use super::connection::Session;
use super::element::WebKitElement;
use super::protocol::{self, Envelope, NavigateParams, NavigateResult};
use crate::backend::{
  AnyElement, AxNodeData, CookieData, FrameInfo, ImageFormat, MetricData, NavLifecycle, ScreenshotOpts,
};
use crate::console_message::ConsoleMessage;
use crate::error::{FerriError, Result};
use crate::events::EventEmitter;
use crate::network::{Request as NetworkRequest, Response};
use crate::state::DialogEvent;

/// Name of the utility execution context — mirrors `UTILITY_WORLD_NAME`
/// in `wkPage.ts`.
pub const UTILITY_WORLD_NAME: &str = "__playwright_utility_world__";

/// Playwright `WebKit` page. Cheaply cloneable; clones share the
/// underlying sessions + managers.
#[derive(Clone)]
pub struct WebKitPage {
  proxy: Session,
  /// Live target session. Swapped on cross-process navigation when
  /// `WebKit` creates a provisional target and commits it via
  /// `Target.didCommitProvisionalTarget` (mirrors `wkPage.ts::_setSession`).
  /// Wrapped in `ArcSwap` so listeners can replace it atomically without
  /// blocking concurrent senders on the old session.
  target: Arc<ArcSwap<Session>>,
  browser: Session,
  proxy_id: Arc<str>,
  /// Live target id. Swapped alongside [`Self::target`] on commit.
  target_id: Arc<ArcSwap<Arc<str>>>,
  context_id: Option<Arc<str>>,
  closed: Arc<AtomicBool>,
  /// Latch: the `window.__fd` selector engine has been injected.
  engine_injected: Arc<AtomicBool>,
  /// Cached `objectId` of the main-world global. `Runtime.callFunctionOn`
  /// needs an `objectId` anchor (no `contextId` form); evaluating
  /// `"this"` once gives a stable handle into the page's main realm.
  /// Cleared on navigation — a new document means a new global.
  /// Mirrors `WKExecutionContext._contextGlobalObjectId`.
  global_object_id: Arc<std::sync::Mutex<Option<String>>>,
  /// Exposed-function callback registry. Keyed by the JS-side function
  /// name; the listener task dispatches `Runtime.bindingCalled` events
  /// back through these callbacks.
  exposed_fns: Arc<tokio::sync::RwLock<rustc_hash::FxHashMap<String, crate::events::ExposedFn>>>,
  /// Idempotent latch for the `Runtime.addBinding` + listener setup.
  binding_initialized: Arc<AtomicBool>,
  /// Ordered list of `(identifier, source)` bootstrap-script fragments.
  /// PW `WebKit`'s `Page.setBootstrapScript` holds a SINGLE source that
  /// each call overwrites, so every `add_init_script` must re-send the
  /// JOINED accumulation (mirrors `wkPage.ts::_calculateBootstrapScript`
  /// and `_updateBootstrapScript`). Without this, registering the binding
  /// controller and then a `__fd_bc.add(name)` fragment left only the
  /// fragment at bootstrap time, so a freshly-navigated page hit a
  /// `__fd_bc` undefined error and `window[name]` never installed.
  init_scripts: Arc<tokio::sync::Mutex<Vec<(String, String)>>>,
  /// Monotonic id source for [`Self::init_scripts`] identifiers.
  init_script_seq: Arc<std::sync::atomic::AtomicU64>,
  /// Live request table, keyed by PW `WebKit` `requestId`. The network
  /// listener inserts on `Network.requestWillBeSent`, links responses
  /// on `Network.responseReceived`, and removes on terminal
  /// finished/failed events.
  pub(crate) requests: Arc<std::sync::Mutex<rustc_hash::FxHashMap<String, crate::network::Request>>>,
  /// Slot holding the most-recent main-document `Request` so
  /// [`Self::goto`] / [`Self::reload`] / history traversals can resolve
  /// the navigation `Response` without polling.
  pub(crate) nav_request_slot: crate::network::NavRequestSlot,
  /// Registered network-interception routes. Listener consults this
  /// vec on `Network.requestIntercepted` to decide which handler to
  /// invoke.
  pub(crate) routes: Arc<tokio::sync::RwLock<Vec<crate::route::RegisteredRoute>>>,
  /// Idempotent latch for `Network.setInterceptionEnabled` +
  /// `Network.addInterception` setup.
  pub(crate) intercept_enabled: Arc<AtomicBool>,
  /// `frameId` → `executionContextId` mapping populated by
  /// `Runtime.executionContextCreated` events. Used by
  /// [`Self::evaluate_in_frame`] to evaluate inside an iframe's realm.
  pub(crate) frame_contexts: Arc<tokio::sync::RwLock<rustc_hash::FxHashMap<String, i64>>>,
  /// Per-execution-context cache of the global-object `objectId` keyed
  /// by `executionContextId`, plus a flag for whether the `window.__fd`
  /// selector engine has been injected into that context. The main
  /// frame's `__fd` injection is tracked by [`Self::engine_injected`] +
  /// [`Self::global_object_id`]; child frames each have their own realm
  /// where `__fd` must be injected independently (Playwright's
  /// `wkPage.ts` creates a `FrameExecutionContext` per
  /// `Runtime.executionContextCreated`). Without this, a
  /// `frame.waitForSelector` polling `window.__fd.selOne` in the child
  /// realm throws forever (`__fd` is undefined there) and the wait
  /// times out. Cleared on navigation.
  frame_engine_contexts: Arc<tokio::sync::RwLock<rustc_hash::FxHashMap<i64, FrameEngineContext>>>,
  /// Main-frame id captured from the initial `Page.getResourceTree`
  /// response. PW `WebKit`'s `frame_id` is distinct from `target_id`
  /// (the page proxy's id); returning `target_id` from
  /// `peek_main_frame_id` caused `main_frame()` to attach a phantom
  /// record under the wrong key while `Page.frameNavigated` events
  /// filled the real id with the navigated URL -- callers reading
  /// `page.url()` then saw the empty phantom record. Mirrors CDP's
  /// `main_frame_id` `OnceLock`.
  pub(crate) main_frame_id_cache: Arc<std::sync::Mutex<Option<String>>>,
  /// Live WebSocket table, keyed by PW `WebKit` `requestId`. The network
  /// listener inserts on `Network.webSocketCreated`, emits frames on
  /// `webSocketFrameSent` / `webSocketFrameReceived`, and removes the
  /// entry on `webSocketClosed`.
  pub(crate) websockets: Arc<tokio::sync::Mutex<rustc_hash::FxHashMap<String, crate::network::WebSocket>>>,
  pub events: EventEmitter,
  pub dialog_manager: crate::dialog::DialogManager,
  pub file_chooser_manager: crate::file_chooser::FileChooserManager,
  pub download_manager: crate::download::DownloadManager,
  pub page_backref: crate::backend::PageBackref,
  pub(crate) frame_cache: Arc<std::sync::Mutex<crate::frame_cache::FrameCache>>,
  pub(crate) frame_listener_started: Arc<AtomicBool>,
  /// Per-page lifecycle signals fed by the target-session listener.
  /// Lets navigation methods await `Page.loadEventFired` /
  /// `domContentEventFired` / `frameNavigated` survivably across
  /// cross-process target swaps (the listener re-subscribes to the new
  /// target on commit, so events continue arriving here).
  pub(crate) lifecycle: Arc<LifecycleSignals>,
  /// Where the listener writes console/network/dialog events. Swapped
  /// in by [`Self::attach_listeners`] so the always-on listener (spawned
  /// from [`Self::attach`]) writes to the caller's logs once they're
  /// provided. Default-empty sinks let the raw API (no `attach_listeners`
  /// call) use page methods without `wait_for_lifecycle` wedging on the
  /// missing-listener path.
  pub(crate) console_log: Arc<ArcSwap<tokio::sync::RwLock<Vec<ConsoleMessage>>>>,
  pub(crate) network_log: Arc<ArcSwap<tokio::sync::RwLock<Vec<NetworkRequest>>>>,
  pub(crate) dialog_log: Arc<ArcSwap<tokio::sync::RwLock<Vec<DialogEvent>>>>,
}

/// Per-child-frame execution-context bookkeeping for the `window.__fd`
/// selector engine. `global_object_id` anchors `Runtime.callFunctionOn`
/// at the child realm; `engine_injected` latches the one-time injection.
#[derive(Clone, Default)]
struct FrameEngineContext {
  global_object_id: Option<String>,
  engine_injected: bool,
}

/// Latch + notify for navigation lifecycle events fed by the
/// target-session listener.
#[derive(Default)]
pub(crate) struct LifecycleSignals {
  pub commit: AtomicBool,
  pub domcontentloaded: AtomicBool,
  pub load: AtomicBool,
  pub failed: AtomicBool,
  pub failure_text: std::sync::Mutex<Option<String>>,
  pub failed_request_id: std::sync::Mutex<Option<String>>,
  pub notify: tokio::sync::Notify,
}

impl LifecycleSignals {
  pub fn reset(&self) {
    self.commit.store(false, Ordering::SeqCst);
    self.domcontentloaded.store(false, Ordering::SeqCst);
    self.load.store(false, Ordering::SeqCst);
    self.failed.store(false, Ordering::SeqCst);
    *self
      .failure_text
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    *self
      .failed_request_id
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
  }

  pub fn mark(&self, kind: NavLifecycle) {
    match kind {
      NavLifecycle::Commit => self.commit.store(true, Ordering::SeqCst),
      NavLifecycle::DomContentLoaded => self.domcontentloaded.store(true, Ordering::SeqCst),
      NavLifecycle::Load => self.load.store(true, Ordering::SeqCst),
    }
    self.notify.notify_waiters();
  }

  pub fn mark_failed(&self, request_id: String, error_text: String) {
    self.failed.store(true, Ordering::SeqCst);
    *self
      .failure_text
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error_text);
    *self
      .failed_request_id
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(request_id);
    self.notify.notify_waiters();
  }

  pub fn seen(&self, kind: NavLifecycle) -> bool {
    match kind {
      NavLifecycle::Commit => self.commit.load(Ordering::SeqCst),
      NavLifecycle::DomContentLoaded => self.domcontentloaded.load(Ordering::SeqCst),
      NavLifecycle::Load => self.load.load(Ordering::SeqCst),
    }
  }

  pub fn failure(&self) -> Option<(String, String)> {
    if !self.failed.load(Ordering::SeqCst) {
      return None;
    }
    let req = self
      .failed_request_id
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone();
    let txt = self
      .failure_text
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone();
    Some((req.unwrap_or_default(), txt.unwrap_or_default()))
  }
}

impl WebKitPage {
  /// Attach to a freshly-created page proxy: wait for the inner
  /// `Target.targetCreated`, open the target session, run the standard
  /// `*.enable` initialisation (mirrors `WKPage._initializeSessionMayThrow`).
  pub async fn attach(
    browser: &WebKitBrowser,
    proxy: Session,
    context_id: Option<String>,
  ) -> std::result::Result<Self, BrowserError> {
    let conn = browser.connection();
    let proxy_id = proxy.page_proxy_id().unwrap_or_default().to_string();
    let target_id = wait_for_first_page_target(&proxy).await?;
    let target = conn.target_session(&proxy_id, &target_id);

    // Page agent before Runtime so executionContextCreated ordering holds.
    target.send("Page.enable", json!({})).await?;
    target.send("Runtime.enable", json!({})).await?;
    target.send("Network.enable", json!({})).await?;
    target.send("Console.enable", json!({})).await?;
    // Dialog domain lives on the page-proxy session (per wkPage.ts);
    // without `Dialog.enable` the `javascriptDialogOpening` event
    // never fires and `window.alert` would wedge the page.
    proxy.send("Dialog.enable", json!({})).await?;
    // Apply per-page context overrides BEFORE the about:blank document
    // becomes scriptable. Mirrors `WKPage._initializeSessionMayThrow` —
    // userAgent / timezone / bypassCSP / offline / permissions live on
    // the target session and must be set before any JS runs in the
    // initial document. Without this, `navigator.userAgent`,
    // `Intl.DateTimeFormat().resolvedOptions().timeZone`, etc. stay at
    // their default values for the lifetime of about:blank.
    if let Some(ctx_id) = context_id.as_deref() {
      if let Some(opts) = browser.context_options_for(ctx_id) {
        apply_pre_page_overrides(&target, &proxy, &opts).await;
      }
    }
    // File-chooser interception is enabled lazily through
    // [`Self::enable_file_chooser_intercept`] when a listener attaches,
    // matching CDP's `_updateFileChooserInterception`. Setting it at
    // attach time unconditionally caused matrix runs to wedge because
    // every page in the session held an intercept lease, and the
    // shared MCP browser couldn't drain pending events fast enough.
    let _ = target
      .send("Page.createUserWorld", json!({ "name": UTILITY_WORLD_NAME }))
      .await;
    let resource_tree = target.send("Page.getResourceTree", json!({})).await.ok();
    let main_frame_id_cache: Arc<std::sync::Mutex<Option<String>>> = Arc::new(std::sync::Mutex::new(None));
    if let Some(tree) = resource_tree
      .as_ref()
      .and_then(|r| r.get("frameTree"))
      .and_then(|t| t.get("frame"))
      .and_then(|f| f.get("id"))
      .and_then(Value::as_str)
    {
      *main_frame_id_cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(tree.to_string());
    }

    let page = WebKitPage {
      proxy,
      target: Arc::new(ArcSwap::from_pointee(target)),
      browser: browser.root().clone(),
      proxy_id: Arc::from(proxy_id),
      target_id: Arc::new(ArcSwap::from_pointee(Arc::<str>::from(target_id))),
      context_id: context_id.map(Arc::from),
      closed: Arc::new(AtomicBool::new(false)),
      engine_injected: Arc::new(AtomicBool::new(false)),
      global_object_id: Arc::new(std::sync::Mutex::new(None)),
      exposed_fns: Arc::new(tokio::sync::RwLock::new(rustc_hash::FxHashMap::default())),
      binding_initialized: Arc::new(AtomicBool::new(false)),
      init_scripts: Arc::new(tokio::sync::Mutex::new(Vec::new())),
      init_script_seq: Arc::new(std::sync::atomic::AtomicU64::new(0)),
      requests: Arc::new(std::sync::Mutex::new(rustc_hash::FxHashMap::default())),
      nav_request_slot: crate::network::NavRequestSlot::new(),
      routes: Arc::new(tokio::sync::RwLock::new(Vec::new())),
      intercept_enabled: Arc::new(AtomicBool::new(false)),
      frame_contexts: Arc::new(tokio::sync::RwLock::new(rustc_hash::FxHashMap::default())),
      frame_engine_contexts: Arc::new(tokio::sync::RwLock::new(rustc_hash::FxHashMap::default())),
      main_frame_id_cache,
      websockets: Arc::new(tokio::sync::Mutex::new(rustc_hash::FxHashMap::default())),
      events: EventEmitter::new(),
      dialog_manager: crate::dialog::DialogManager::new(),
      file_chooser_manager: crate::file_chooser::FileChooserManager::new(),
      download_manager: crate::download::DownloadManager::new(),
      page_backref: crate::backend::PageBackref::new(),
      frame_cache: Arc::new(std::sync::Mutex::new(crate::frame_cache::FrameCache::default())),
      frame_listener_started: Arc::new(AtomicBool::new(false)),
      lifecycle: Arc::new(LifecycleSignals::default()),
      console_log: Arc::new(ArcSwap::from_pointee(tokio::sync::RwLock::new(Vec::new()))),
      network_log: Arc::new(ArcSwap::from_pointee(tokio::sync::RwLock::new(Vec::new()))),
      dialog_log: Arc::new(ArcSwap::from_pointee(tokio::sync::RwLock::new(Vec::new()))),
    };
    // Spawn the always-on listener: lifecycle signals, frame events,
    // network log, console log, dialog log, route interception, and
    // cross-process target swap. Without this, raw users of the page
    // API (no `attach_listeners` from `BrowserState`) would see
    // `wait_for_lifecycle` wedge for the full timeout because no one
    // ever marks the lifecycle latches.
    super::events::attach_listeners(&page);
    Ok(page)
  }

  #[must_use]
  pub fn page_proxy_id(&self) -> &str {
    &self.proxy_id
  }

  /// Current target id. Snapshot of the swappable inner — callers
  /// holding the returned `Arc<str>` keep that snapshot stable even if
  /// the live target is swapped underneath them.
  #[must_use]
  pub fn target_id(&self) -> Arc<str> {
    Arc::clone(&self.target_id.load())
  }

  /// Current target session. Cheap clone (Session is `Arc`-shaped
  /// inside). Swappable underneath callers via
  /// [`Self::swap_target_session`]; each call returns the live session
  /// at the moment of the call.
  #[must_use]
  pub(crate) fn target_session(&self) -> Session {
    Session::clone(&self.target.load())
  }

  /// Handle to the swappable target slot — used by the page-proxy
  /// listener so dispatch helpers read the LIVE session on every event
  /// (not a snapshot taken at attach time).
  #[must_use]
  pub(crate) fn target_swap(&self) -> Arc<ArcSwap<Session>> {
    Arc::clone(&self.target)
  }

  /// Atomically replace the live target session and target id. Called
  /// by the page-proxy listener when `WebKit` commits a provisional
  /// target after a cross-process navigation.
  pub(crate) fn swap_target_session(&self, new_session: Session, new_target_id: Arc<str>) {
    self.target.store(Arc::new(new_session));
    self.target_id.store(Arc::new(new_target_id));
    // The committed target lives in a fresh process — any frame
    // contexts / realm caches / interception state from the OLD process
    // are invalid. The new target's listener events will reseed the
    // caches as frames attach + navigate; the interception latch needs
    // an explicit reset so `ensure_interception_enabled` re-issues the
    // protocol calls on the new target.
    *self
      .global_object_id
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    self.engine_injected.store(false, Ordering::Relaxed);
    self.intercept_enabled.store(false, Ordering::Relaxed);
    *self
      .frame_cache
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = crate::frame_cache::FrameCache::default();
    self.main_frame_id_cache_clear();
  }

  /// Reset the main-frame id cache. Used after a cross-process target
  /// swap because the new target has a fresh main frame id distinct
  /// from the old one. Next call to `peek_main_frame_id` returns `None`
  /// until the listener re-seeds via `Page.frameNavigated`.
  fn main_frame_id_cache_clear(&self) {
    *self
      .main_frame_id_cache
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
  }

  #[must_use]
  pub(crate) fn proxy_session(&self) -> &Session {
    &self.proxy
  }

  // ── Protocol helpers ──────────────────────────────────────────────────

  /// `Runtime.evaluate` on the target session. `return_by_value`
  /// controls whether the reply inlines the value or returns an
  /// `objectId` handle.
  async fn runtime_evaluate(&self, expression: &str, return_by_value: bool) -> Result<Value> {
    let resp = self
      .target_session()
      .send(
        protocol::RUNTIME_EVALUATE,
        json!({
          "expression": expression,
          "returnByValue": return_by_value,
        }),
      )
      .await
      .map_err(conn_err)?;
    if resp.get("wasThrown").and_then(Value::as_bool).unwrap_or(false) {
      let msg = resp
        .get("result")
        .and_then(|r| r.get("description").or_else(|| r.get("value")))
        .and_then(Value::as_str)
        .unwrap_or("evaluation threw")
        .to_string();
      return Err(FerriError::evaluation(msg));
    }
    Ok(resp)
  }

  /// Evaluate `expression` and return the inlined JSON value.
  async fn eval_value(&self, expression: &str) -> Result<Value> {
    let resp = self.runtime_evaluate(expression, true).await?;
    Ok(
      resp
        .get("result")
        .and_then(|r| r.get("value"))
        .cloned()
        .unwrap_or(Value::Null),
    )
  }

  /// Drop realm-scoped caches — a new document invalidates the engine
  /// injection and the cached global `objectId`.
  fn reset_realm(&self) {
    self.engine_injected.store(false, Ordering::Relaxed);
    *self
      .global_object_id
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
  }

  /// Cached `objectId` of the main-world global, evaluating `"this"`
  /// once. The anchor `Runtime.callFunctionOn` runs against.
  async fn global_anchor(&self) -> Result<String> {
    if let Some(id) = self
      .global_object_id
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone()
    {
      return Ok(id);
    }
    let resp = self.runtime_evaluate("this", false).await?;
    let id = resp
      .get("result")
      .and_then(|r| r.get("objectId"))
      .and_then(Value::as_str)
      .ok_or_else(|| FerriError::protocol("Runtime.evaluate", "global anchor: no objectId"))?
      .to_string();
    *self
      .global_object_id
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(id.clone());
    Ok(id)
  }

  fn ensure_open(&self) -> Result<()> {
    if self.closed.load(Ordering::Relaxed) {
      return Err(FerriError::backend("webkit: page is closed"));
    }
    Ok(())
  }

  // ── Frames ────────────────────────────────────────────────────────────

  pub async fn get_frame_tree(&self) -> Result<Vec<FrameInfo>> {
    let resp = self
      .target_session()
      .send("Page.getResourceTree", json!({}))
      .await
      .map_err(conn_err)?;
    let mut frames = Vec::new();
    if let Some(root) = resp.get("frameTree") {
      collect_frames(root, None, &mut frames);
    }
    Ok(frames)
  }

  #[must_use]
  pub fn peek_main_frame_id(&self) -> Option<String> {
    self
      .main_frame_id_cache
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone()
  }

  pub async fn evaluate_in_frame(&self, expression: &str, frame_id: &str) -> Result<Option<Value>> {
    let ctx_id = self.frame_contexts.read().await.get(frame_id).copied();
    // Selector-engine calls reach a child frame's realm through
    // `contextId`. `window.__fd` only lives in whichever realm the
    // engine was injected into, so inject it into THIS context first
    // when the expression depends on it — otherwise the child realm
    // throws `__fd is undefined` on every poll and the wait wedges.
    if let Some(id) = ctx_id
      && expression.contains("window.__fd")
    {
      self.ensure_engine_in_context(id).await?;
    }
    let mut params = json!({ "expression": expression, "returnByValue": true });
    if let Some(id) = ctx_id {
      params["contextId"] = json!(id);
    }
    let resp = self
      .target_session()
      .send(protocol::RUNTIME_EVALUATE, params)
      .await
      .map_err(conn_err)?;
    if resp.get("wasThrown").and_then(Value::as_bool).unwrap_or(false) {
      let text = resp
        .get("result")
        .and_then(|r| r.get("description").or_else(|| r.get("value")))
        .and_then(Value::as_str)
        .unwrap_or("evaluation threw")
        .to_string();
      return Err(FerriError::evaluation(text));
    }
    Ok(Some(
      resp
        .get("result")
        .and_then(|r| r.get("value"))
        .cloned()
        .unwrap_or(Value::Null),
    ))
  }

  pub async fn content_frame_id(&self, _object_id: &str) -> Result<Option<String>> {
    tokio::task::yield_now().await;
    Ok(None)
  }

  // ── Navigation ────────────────────────────────────────────────────────

  pub async fn goto(
    &self,
    url: &str,
    lifecycle: NavLifecycle,
    timeout_ms: u64,
    referrer: Option<&str>,
  ) -> Result<Option<Response>> {
    self.ensure_open()?;
    self.reset_realm();
    self.nav_request_slot.clear();
    // Reset lifecycle latches BEFORE issuing the navigate -- the listener
    // marks them as events come in. Survives cross-process target swaps
    // (the listener re-subscribes on `Target.didCommitProvisionalTarget`
    // so events from the new process still feed the latches).
    self.lifecycle.reset();
    let params = NavigateParams {
      url: url.to_string(),
      page_proxy_id: self.proxy_id.to_string(),
      frame_id: None,
      referrer: referrer.map(str::to_string),
    };
    let nav = tokio::time::timeout(
      std::time::Duration::from_millis(timeout_ms),
      self
        .browser
        .send(protocol::PLAYWRIGHT_NAVIGATE, serde_json::to_value(&params)?),
    )
    .await
    .map_err(|_| FerriError::timeout(format!("navigating to {url}"), timeout_ms))?
    .map_err(conn_err)?;
    let parsed: NavigateResult = serde_json::from_value(nav).unwrap_or_default();
    if let Some(err) = parsed.error_text {
      if !err.is_empty() {
        return Err(FerriError::backend(format!("webkit navigate: {err}")));
      }
    }
    if parsed.loader_id.is_some() {
      self.wait_for_lifecycle(lifecycle, timeout_ms).await?;
    }
    Ok(self.await_nav_response().await)
  }

  /// Resolve the main-document `Response` captured by the network
  /// listener for the most recent navigation.
  async fn await_nav_response(&self) -> Option<Response> {
    let req = self.nav_request_slot.get()?;
    req.response().await.ok().flatten()
  }

  /// Wait for the target-session lifecycle event matching `lifecycle`,
  /// or for `Network.loadingFailed` on the main-document request.
  /// Uses [`LifecycleSignals`] fed by the target listener so the wait
  /// survives cross-process target swaps (listener re-subscribes to
  /// the new target on `Target.didCommitProvisionalTarget`).
  async fn wait_for_lifecycle(&self, lifecycle: NavLifecycle, timeout_ms: u64) -> Result<()> {
    let signals = Arc::clone(&self.lifecycle);
    let nav_slot = self.nav_request_slot.clone();
    let result = tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), async move {
      loop {
        if signals.seen(lifecycle) {
          return Ok(());
        }
        if let Some((event_request_id, err)) = signals.failure() {
          if let Some(req) = nav_slot.get() {
            if event_request_id == req.id() {
              return Err(FerriError::backend(format!("webkit navigate: {err}")));
            }
          }
        }
        signals.notify.notified().await;
      }
    })
    .await;
    match result {
      Ok(inner) => inner,
      Err(_) => Ok(()),
    }
  }

  pub async fn wait_for_navigation(&self) -> Result<()> {
    self.lifecycle.reset();
    self.wait_for_lifecycle(NavLifecycle::Load, 30_000).await
  }

  pub async fn reload(&self, lifecycle: NavLifecycle, timeout_ms: u64) -> Result<Option<Response>> {
    self.ensure_open()?;
    self.reset_realm();
    self.nav_request_slot.clear();
    self.lifecycle.reset();
    self
      .target_session()
      .send("Page.reload", json!({}))
      .await
      .map_err(conn_err)?;
    let _ = self.wait_for_lifecycle(lifecycle, timeout_ms).await;
    Ok(self.await_nav_response().await)
  }

  pub async fn go_back(&self, lifecycle: NavLifecycle, timeout_ms: u64) -> Result<Option<Response>> {
    self.history_traverse("Page.goBack", lifecycle, timeout_ms).await
  }

  pub async fn go_forward(&self, lifecycle: NavLifecycle, timeout_ms: u64) -> Result<Option<Response>> {
    self.history_traverse("Page.goForward", lifecycle, timeout_ms).await
  }

  async fn history_traverse(&self, method: &str, lifecycle: NavLifecycle, timeout_ms: u64) -> Result<Option<Response>> {
    self.ensure_open()?;
    self.reset_realm();
    self.nav_request_slot.clear();
    self.lifecycle.reset();
    self.target_session().send(method, json!({})).await.map_err(conn_err)?;
    let _ = self.wait_for_lifecycle(lifecycle, timeout_ms).await;
    Ok(self.await_nav_response().await)
  }

  pub async fn url(&self) -> Result<Option<String>> {
    Ok(self.eval_value("location.href").await?.as_str().map(String::from))
  }

  pub async fn title(&self) -> Result<Option<String>> {
    Ok(self.eval_value("document.title").await?.as_str().map(String::from))
  }

  // ── JavaScript ────────────────────────────────────────────────────────

  pub async fn injected_script(&self) -> Result<String> {
    self.ensure_engine_injected().await?;
    Ok("window.__fd".to_string())
  }

  pub async fn ensure_engine_injected(&self) -> Result<()> {
    if self.engine_injected.load(Ordering::Relaxed) {
      return Ok(());
    }
    // The lazy-inject script is an async IIFE returning a promise.
    // `Runtime.evaluate` has no `awaitPromise`; route the call through
    // `Runtime.callFunctionOn` (which does) anchored on the global, so
    // we synchronously block until `window.__fd` is live.
    let anchor = self.global_anchor().await?;
    let js = crate::selectors::build_lazy_inject_js();
    let wrapper = format!("function(){{ return ({js}); }}");
    let resp = self
      .target_session()
      .send(
        protocol::RUNTIME_CALL_FUNCTION_ON,
        json!({
          "objectId": anchor,
          "functionDeclaration": wrapper,
          "returnByValue": false,
          "awaitPromise": true,
        }),
      )
      .await
      .map_err(conn_err)?;
    if resp.get("wasThrown").and_then(Value::as_bool).unwrap_or(false) {
      let text = resp
        .get("result")
        .and_then(|r| r.get("description").or_else(|| r.get("value")))
        .and_then(Value::as_str)
        .unwrap_or("engine injection threw")
        .to_string();
      return Err(FerriError::evaluation(text));
    }
    self.engine_injected.store(true, Ordering::Relaxed);
    Ok(())
  }

  /// Resolve the global-object `objectId` for a specific execution
  /// context (an iframe's realm), evaluating `"this"` with `contextId`
  /// set. Cached per context in [`Self::frame_engine_contexts`].
  async fn frame_context_anchor(&self, context_id: i64) -> Result<String> {
    if let Some(id) = self
      .frame_engine_contexts
      .read()
      .await
      .get(&context_id)
      .and_then(|c| c.global_object_id.clone())
    {
      return Ok(id);
    }
    let resp = self
      .target_session()
      .send(
        protocol::RUNTIME_EVALUATE,
        json!({ "expression": "this", "returnByValue": false, "contextId": context_id }),
      )
      .await
      .map_err(conn_err)?;
    let id = resp
      .get("result")
      .and_then(|r| r.get("objectId"))
      .and_then(Value::as_str)
      .ok_or_else(|| FerriError::protocol("Runtime.evaluate", "frame context anchor: no objectId"))?
      .to_string();
    self
      .frame_engine_contexts
      .write()
      .await
      .entry(context_id)
      .or_default()
      .global_object_id = Some(id.clone());
    Ok(id)
  }

  /// Inject the `window.__fd` selector engine into the iframe realm
  /// identified by `context_id`. Idempotent per context. Mirrors
  /// [`Self::ensure_engine_injected`] but anchors `callFunctionOn` on
  /// the child realm's global so `__fd` lands in THAT realm. Playwright
  /// creates one `FrameExecutionContext` (and lazily injects the
  /// utility script) per `Runtime.executionContextCreated`
  /// (`wkPage.ts::_onExecutionContextCreated`).
  async fn ensure_engine_in_context(&self, context_id: i64) -> Result<()> {
    if self
      .frame_engine_contexts
      .read()
      .await
      .get(&context_id)
      .is_some_and(|c| c.engine_injected)
    {
      return Ok(());
    }
    // Prune engine entries for contexts that have since been replaced
    // (a navigated frame gets a fresh contextId) so the map stays
    // bounded across long-lived pages.
    {
      let live: std::collections::HashSet<i64> = self.frame_contexts.read().await.values().copied().collect();
      self
        .frame_engine_contexts
        .write()
        .await
        .retain(|id, _| *id == context_id || live.contains(id));
    }
    let anchor = self.frame_context_anchor(context_id).await?;
    let js = crate::selectors::build_lazy_inject_js();
    let wrapper = format!("function(){{ return ({js}); }}");
    let resp = self
      .target_session()
      .send(
        protocol::RUNTIME_CALL_FUNCTION_ON,
        json!({
          "objectId": anchor,
          "functionDeclaration": wrapper,
          "returnByValue": false,
          "awaitPromise": true,
        }),
      )
      .await
      .map_err(conn_err)?;
    if resp.get("wasThrown").and_then(Value::as_bool).unwrap_or(false) {
      let text = resp
        .get("result")
        .and_then(|r| r.get("description").or_else(|| r.get("value")))
        .and_then(Value::as_str)
        .unwrap_or("frame engine injection threw")
        .to_string();
      return Err(FerriError::evaluation(text));
    }
    self
      .frame_engine_contexts
      .write()
      .await
      .entry(context_id)
      .or_default()
      .engine_injected = true;
    Ok(())
  }

  pub async fn evaluate(&self, expression: &str) -> Result<Option<Value>> {
    Ok(Some(self.eval_value(expression).await?))
  }

  // ── Elements ──────────────────────────────────────────────────────────

  pub async fn find_element(&self, selector: &str) -> Result<AnyElement> {
    self.ensure_engine_injected().await?;
    let sel_js = crate::selectors::build_selone_js(selector, "window.__fd", false)?;
    self
      .evaluate_to_element(&sel_js, None)
      .await
      .map_err(|_| FerriError::invalid_selector(selector, "no element found"))
  }

  pub async fn evaluate_to_element(&self, js: &str, frame_id: Option<&str>) -> Result<AnyElement> {
    // Resolve element selectors inside the owning frame's realm so the
    // returned `objectId` is bound to the right execution context and
    // `window.__fd` is present there. `None` (or the main frame) uses
    // the page's default realm.
    let ctx_id = match frame_id {
      Some(fid) => self.frame_contexts.read().await.get(fid).copied(),
      None => None,
    };
    let resp = if let Some(id) = ctx_id {
      if js.contains("window.__fd") {
        self.ensure_engine_in_context(id).await?;
      }
      let r = self
        .target_session()
        .send(
          protocol::RUNTIME_EVALUATE,
          json!({ "expression": js, "returnByValue": false, "contextId": id }),
        )
        .await
        .map_err(conn_err)?;
      if r.get("wasThrown").and_then(Value::as_bool).unwrap_or(false) {
        let text = r
          .get("result")
          .and_then(|res| res.get("description").or_else(|| res.get("value")))
          .and_then(Value::as_str)
          .unwrap_or("evaluation threw")
          .to_string();
        return Err(FerriError::evaluation(text));
      }
      r
    } else {
      self.runtime_evaluate(js, false).await?
    };
    let object_id = resp
      .get("result")
      .and_then(|r| r.get("objectId"))
      .and_then(Value::as_str)
      .ok_or_else(|| FerriError::protocol("Runtime.evaluate", "evaluate_to_element: result is not an object"))?;
    Ok(AnyElement::WebKit(WebKitElement::new(
      self.target_session(),
      object_id.to_string(),
    )))
  }

  pub async fn resolve_backend_node(&self, _backend_node_id: i64, ref_id: &str) -> Result<AnyElement> {
    tokio::task::yield_now().await;
    Ok(AnyElement::WebKit(WebKitElement::new(
      self.target_session(),
      ref_id.to_string(),
    )))
  }

  // ── Content ───────────────────────────────────────────────────────────

  pub async fn content(&self) -> Result<String> {
    Ok(
      self
        .eval_value("document.documentElement.outerHTML")
        .await?
        .as_str()
        .unwrap_or_default()
        .to_string(),
    )
  }

  pub async fn set_content(&self, html: &str) -> Result<()> {
    let escaped = serde_json::to_string(html).unwrap_or_else(|_| "\"\"".into());
    self
      .runtime_evaluate(
        &format!("(()=>{{document.open();document.write({escaped});document.close();}})()"),
        true,
      )
      .await?;
    Ok(())
  }

  // ── Screenshots ───────────────────────────────────────────────────────

  pub async fn screenshot(&self, opts: ScreenshotOpts) -> Result<Vec<u8>> {
    // Install the DOM-side overrides (caret hide, user style, animation
    // pause, mask overlays) before the capture, exactly like the CDP
    // backend (`CdpPage::screenshot_install_dom`). WebKit has no
    // protocol-level masking, so the shared `screenshot_js` overlay
    // divs are the only way to honour `mask` / `maskColor`.
    let css = crate::backend::screenshot_js::build_css(&opts);
    let style_installed = if css.is_empty() {
      false
    } else {
      self
        .eval_value(&crate::backend::screenshot_js::install_style_js(&css))
        .await?;
      true
    };
    let mask_installed = if let Some(js) = crate::backend::screenshot_js::install_mask_js(&opts) {
      self.eval_value(&js).await?;
      true
    } else {
      false
    };
    let result = self.capture_rect(&opts).await;
    // Teardown always runs so post-screenshot interaction sees pristine
    // DOM, even when the capture itself failed.
    if style_installed {
      let _ = self
        .eval_value(crate::backend::screenshot_js::uninstall_style_js())
        .await;
    }
    if mask_installed {
      let _ = self
        .eval_value(crate::backend::screenshot_js::uninstall_mask_js())
        .await;
    }
    result
  }

  /// Capture the visible / full-page / clipped rect via `Page.snapshotRect`
  /// and decode the returned data URL. Split out of [`Self::screenshot`]
  /// so the mask/style install + teardown wrap a single capture call.
  async fn capture_rect(&self, opts: &ScreenshotOpts) -> Result<Vec<u8>> {
    let (document_rect, coord_system) = if opts.full_page {
      let dims = self
        .eval_value(
          "JSON.stringify({\
            w: Math.max(document.documentElement.scrollWidth, document.documentElement.clientWidth),\
            h: Math.max(document.documentElement.scrollHeight, document.documentElement.clientHeight)\
          })",
        )
        .await?;
      let parsed: Value = dims
        .as_str()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
      let w = parsed.get("w").and_then(Value::as_f64).unwrap_or(0.0);
      let h = parsed.get("h").and_then(Value::as_f64).unwrap_or(0.0);
      (json!({ "x": 0, "y": 0, "width": w, "height": h }), "Page")
    } else if let Some(rect) = opts.clip {
      (
        json!({ "x": rect.x, "y": rect.y, "width": rect.width, "height": rect.height }),
        "Viewport",
      )
    } else {
      let dims = self
        .eval_value("JSON.stringify({w:window.innerWidth,h:window.innerHeight})")
        .await?;
      let parsed: Value = dims
        .as_str()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
      let w = parsed.get("w").and_then(Value::as_f64).unwrap_or(800.0);
      let h = parsed.get("h").and_then(Value::as_f64).unwrap_or(600.0);
      (json!({ "x": 0, "y": 0, "width": w, "height": h }), "Viewport")
    };
    let mut params = document_rect;
    params["coordinateSystem"] = json!(coord_system);
    params["omitDeviceScaleFactor"] = json!(!matches!(opts.scale, Some(crate::backend::ScreenshotScale::Device)));
    let resp = self
      .target_session()
      .send("Page.snapshotRect", params)
      .await
      .map_err(conn_err)?;
    let data_url = resp.get("dataURL").and_then(Value::as_str).unwrap_or_default();
    let b64 = data_url.split_once(',').map_or(data_url, |(_, d)| d);
    base64::engine::general_purpose::STANDARD
      .decode(b64)
      .map_err(|e| FerriError::backend(format!("screenshot base64: {e}")))
  }

  pub async fn screenshot_element(&self, selector: &str, format: ImageFormat) -> Result<Vec<u8>> {
    let elem = self.find_element(selector).await?;
    match elem {
      AnyElement::WebKit(e) => e.screenshot(format).await,
      _ => Err(FerriError::backend("screenshot_element: non-webkit element")),
    }
  }

  // ── Accessibility ─────────────────────────────────────────────────────

  pub async fn accessibility_tree(&self) -> Result<Vec<AxNodeData>> {
    self.accessibility_tree_with_depth(-1).await
  }

  pub async fn accessibility_tree_with_depth(&self, max_depth: i32) -> Result<Vec<AxNodeData>> {
    let fd = self.injected_script().await?;
    self.eval_value(crate::selectors::AX_SUPPORT_JS).await?;
    let json_str = self
      .eval_value(&format!("JSON.stringify({fd}.accessibilityTree({max_depth}))"))
      .await?;
    let arr: Vec<Value> = json_str
      .as_str()
      .and_then(|s| serde_json::from_str(s).ok())
      .unwrap_or_default();
    Ok(super::events::parse_ax_nodes(&arr))
  }

  // ── Input ─────────────────────────────────────────────────────────────

  pub async fn click_at(&self, x: f64, y: f64) -> Result<()> {
    self
      .click_at_with(x, y, &crate::backend::BackendClickArgs::default_left())
      .await
  }

  pub async fn click_at_opts(&self, x: f64, y: f64, button: &str, click_count: u32) -> Result<()> {
    let mut args = crate::backend::BackendClickArgs::default_left();
    args.button = crate::options::MouseButton::parse(button).unwrap_or_default();
    args.click_count = click_count;
    self.click_at_with(x, y, &args).await
  }

  pub async fn click_at_with(&self, x: f64, y: f64, args: &crate::backend::BackendClickArgs) -> Result<()> {
    super::input::click(self, x, y, args).await
  }

  pub async fn hover_at_with(&self, x: f64, y: f64, args: &crate::backend::BackendHoverArgs) -> Result<()> {
    super::input::hover(self, x, y, args).await
  }

  pub async fn tap_at_with(&self, x: f64, y: f64, args: &crate::backend::BackendTapArgs) -> Result<()> {
    super::input::tap(self, x, y, args).await
  }

  pub async fn press_modifiers(&self, mods: &[crate::options::Modifier]) -> Result<()> {
    super::input::press_modifiers(self, mods).await
  }

  pub async fn release_modifiers(&self, mods: &[crate::options::Modifier]) -> Result<()> {
    super::input::release_modifiers(self, mods).await
  }

  pub async fn move_mouse(&self, x: f64, y: f64) -> Result<()> {
    super::input::move_mouse(self, x, y).await
  }

  pub async fn move_mouse_smooth(&self, from_x: f64, from_y: f64, to_x: f64, to_y: f64, steps: u32) -> Result<()> {
    super::input::move_mouse_smooth(self, from_x, from_y, to_x, to_y, steps).await
  }

  pub async fn mouse_wheel(&self, delta_x: f64, delta_y: f64) -> Result<()> {
    super::input::mouse_wheel(self, delta_x, delta_y).await
  }

  pub async fn mouse_down(&self, x: f64, y: f64, button: &str) -> Result<()> {
    super::input::mouse_down(self, x, y, button).await
  }

  pub async fn mouse_up(&self, x: f64, y: f64, button: &str) -> Result<()> {
    super::input::mouse_up(self, x, y, button).await
  }

  pub async fn click_and_drag(&self, from: (f64, f64), to: (f64, f64), steps: u32) -> Result<()> {
    super::input::click_and_drag(self, from, to, steps).await
  }

  pub async fn type_str(&self, text: &str) -> Result<()> {
    super::input::type_text(self, text).await
  }

  pub async fn key_down(&self, key: &str) -> Result<()> {
    super::input::key_down(self, key).await
  }

  pub async fn key_up(&self, key: &str) -> Result<()> {
    super::input::key_up(self, key).await
  }

  pub async fn press_key(&self, key: &str) -> Result<()> {
    super::input::press_key(self, key).await
  }

  // ── Cookies ───────────────────────────────────────────────────────────

  pub async fn get_cookies(&self) -> Result<Vec<CookieData>> {
    let resp = self
      .target_session()
      .send("Page.getCookies", json!({}))
      .await
      .map_err(conn_err)?;
    let arr = resp
      .get("cookies")
      .and_then(Value::as_array)
      .cloned()
      .unwrap_or_default();
    Ok(arr.iter().map(super::events::parse_cookie).collect())
  }

  pub async fn set_cookie(&self, cookie: CookieData) -> Result<()> {
    super::events::set_cookie(&self.target_session(), cookie).await
  }

  pub async fn delete_cookie(&self, name: &str, domain: Option<&str>) -> Result<()> {
    let mut params = json!({ "cookieName": name });
    if let Some(d) = domain {
      params["url"] = json!(format!("http://{d}/"));
    }
    self
      .target_session()
      .send("Page.deleteCookie", params)
      .await
      .map_err(conn_err)?;
    Ok(())
  }

  pub async fn clear_cookies(&self) -> Result<()> {
    for c in self.get_cookies().await? {
      let _ = self.delete_cookie(&c.name, Some(&c.domain)).await;
    }
    Ok(())
  }

  // ── Emulation ─────────────────────────────────────────────────────────

  pub async fn apply_context_options(&self, opts: &crate::options::BrowserContextOptions) -> Result<()> {
    // Per-page document-time overrides (userAgent, timezone, locale,
    // JS-disabled, bypassCSP, offline, permissions) are now sent inside
    // [`Self::attach`] from the browser's stashed context options, so
    // they're live BEFORE about:blank becomes scriptable. This call
    // re-asserts the runtime-mutable subset (viewport, media,
    // extra headers) plus the geolocation/screen overrides that don't
    // care about document timing. No reload needed.
    self.apply_runtime_overrides(opts).await?;
    self.apply_proxy_session_overrides(opts).await;
    self.apply_browser_session_overrides(opts).await;
    self.apply_target_session_overrides(opts).await;
    Ok(())
  }

  /// Backs [`crate::Page::set_http_credentials`]. The stock `WebKit` IPC
  /// host has no `Fetch.authRequired`-equivalent hook, so dynamic
  /// HTTP-credential mutation is surfaced as a typed Unsupported per
  /// Rule 4 rather than silently dropped.
  pub async fn set_http_credentials(&self, _creds: Option<crate::options::HttpCredentials>) -> Result<()> {
    tokio::task::yield_now().await;
    Err(crate::error::FerriError::Unsupported(
      "BrowserContext.setHTTPCredentials is not supported on the webkit backend: the WKWebView IPC host exposes no auth-challenge hook".into(),
    ))
  }

  async fn apply_runtime_overrides(&self, opts: &crate::options::BrowserContextOptions) -> Result<()> {
    if let Some(vp) = opts.resolved_viewport() {
      self.emulate_viewport(&vp).await?;
    }
    if opts.any_media_override() {
      self.emulate_media(&opts.as_emulate_media()).await?;
    }
    if let Some(h) = opts.extra_http_headers.as_ref() {
      self.set_extra_http_headers(h).await?;
    }
    Ok(())
  }

  async fn apply_target_session_overrides(&self, opts: &crate::options::BrowserContextOptions) {
    if let Some(ua) = opts.user_agent.as_deref() {
      let _ = self
        .target_session()
        .send("Page.overrideUserAgent", json!({ "value": ua }))
        .await;
    }
    if let Some(tz) = opts.timezone_id.as_deref() {
      let _ = self
        .target_session()
        .send("Page.setTimeZone", json!({ "timeZone": tz }))
        .await;
    }
    if let Some(screen) = opts.screen {
      let _ = self
        .target_session()
        .send(
          "Page.setScreenSizeOverride",
          json!({ "width": screen.width, "height": screen.height }),
        )
        .await;
    }
    if let Some(o) = opts.offline {
      let _ = self
        .target_session()
        .send("Network.setEmulateOfflineState", json!({ "offline": o }))
        .await;
    }
    if let Some(true) = opts.bypass_csp {
      let _ = self
        .target_session()
        .send("Page.setBypassCSP", json!({ "enabled": true }))
        .await;
    }
  }

  async fn apply_proxy_session_overrides(&self, opts: &crate::options::BrowserContextOptions) {
    if let Some(true) = opts.java_script_enabled.map(|v| !v) {
      let _ = self
        .proxy
        .send("Emulation.setJavaScriptEnabled", json!({ "enabled": false }))
        .await;
    }
    if let Some(perms) = opts.permissions.as_ref() {
      let _ = self
        .proxy
        .send(
          "Emulation.grantPermissions",
          json!({ "origin": "*", "permissions": perms }),
        )
        .await;
    }
  }

  async fn apply_browser_session_overrides(&self, opts: &crate::options::BrowserContextOptions) {
    let Some(ctx) = &self.context_id else {
      return;
    };
    if let Some(locale) = opts.locale.as_deref() {
      let _ = self
        .browser
        .send(
          "Playwright.setLanguages",
          json!({ "browserContextId": ctx.to_string(), "languages": [locale] }),
        )
        .await;
    }
    if let Some(g) = opts.geolocation {
      let ts: u64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or(0);
      let _ = self
        .browser
        .send(
          "Playwright.setGeolocationOverride",
          json!({
            "browserContextId": ctx.to_string(),
            "geolocation": {
              "latitude": g.latitude,
              "longitude": g.longitude,
              "accuracy": g.accuracy,
              "timestamp": ts,
            },
          }),
        )
        .await;
    }
  }

  pub async fn emulate_viewport(&self, config: &crate::options::ViewportConfig) -> Result<()> {
    // Per `wkPage.ts::_updateViewport`: device metrics override goes
    // on the page-proxy session, screen-size override on the inner
    // target session.
    self
      .proxy
      .send(
        "Emulation.setDeviceMetricsOverride",
        json!({
          "width": config.width,
          "height": config.height,
          "fixedLayout": false,
          "deviceScaleFactor": if config.device_scale_factor > 0.0 { config.device_scale_factor } else { 1.0 },
        }),
      )
      .await
      .map_err(conn_err)?;
    let _ = self
      .target_session()
      .send(
        "Page.setScreenSizeOverride",
        json!({ "width": config.width, "height": config.height }),
      )
      .await;
    Ok(())
  }

  pub async fn emulate_media(&self, opts: &crate::options::EmulateMediaOptions) -> Result<()> {
    use crate::options::MediaOverride;
    fn user_pref(name: &str, value: Option<&str>) -> Value {
      let mut params = json!({ "name": name });
      match value {
        Some(v) => params["value"] = json!(v),
        None => params["value"] = Value::Null,
      }
      params
    }
    // Apply user-preference overrides BEFORE `Page.setEmulatedMedia`:
    // WPE WebKit resets `PrefersColorScheme` whenever the media override
    // is (re)assigned, so the media call has to come last.
    if let MediaOverride::Set(v) = &opts.color_scheme {
      let value = match v.as_str() {
        "light" => "Light",
        "dark" => "Dark",
        other => other,
      };
      self
        .target_session()
        .send(
          "Page.overrideUserPreference",
          user_pref("PrefersColorScheme", Some(value)),
        )
        .await
        .map_err(conn_err)?;
    } else if matches!(opts.color_scheme, MediaOverride::Disabled) {
      self
        .target_session()
        .send("Page.overrideUserPreference", user_pref("PrefersColorScheme", None))
        .await
        .map_err(conn_err)?;
    }
    self.emulate_media_remaining(opts).await?;
    match &opts.media {
      MediaOverride::Set(v) => {
        self
          .target_session()
          .send("Page.setEmulatedMedia", json!({ "media": v }))
          .await
          .map_err(conn_err)?;
      },
      MediaOverride::Disabled => {
        self
          .target_session()
          .send("Page.setEmulatedMedia", json!({ "media": "" }))
          .await
          .map_err(conn_err)?;
      },
      MediaOverride::Unchanged => {},
    }
    // Re-assert the color-scheme override after `setEmulatedMedia` —
    // a media change resets `PrefersColorScheme` in WPE; the second
    // call restores the dark/light/no-preference state we want.
    if let MediaOverride::Set(v) = &opts.color_scheme {
      let value = match v.as_str() {
        "light" => "Light",
        "dark" => "Dark",
        other => other,
      };
      self
        .target_session()
        .send(
          "Page.overrideUserPreference",
          user_pref("PrefersColorScheme", Some(value)),
        )
        .await
        .map_err(conn_err)?;
    }
    Ok(())
  }

  async fn emulate_media_remaining(&self, opts: &crate::options::EmulateMediaOptions) -> Result<()> {
    use crate::options::MediaOverride;
    fn user_pref(name: &str, value: Option<&str>) -> Value {
      let mut params = json!({ "name": name });
      match value {
        Some(v) => params["value"] = json!(v),
        None => params["value"] = Value::Null,
      }
      params
    }
    // reduced_motion -> PrefersReducedMotion: "Reduce" | "NoPreference".
    match &opts.reduced_motion {
      MediaOverride::Set(v) => {
        let val = match v.as_str() {
          "reduce" => "Reduce",
          "no-preference" => "NoPreference",
          other => other,
        };
        self
          .target_session()
          .send(
            "Page.overrideUserPreference",
            user_pref("PrefersReducedMotion", Some(val)),
          )
          .await
          .map_err(conn_err)?;
      },
      MediaOverride::Disabled => {
        self
          .target_session()
          .send("Page.overrideUserPreference", user_pref("PrefersReducedMotion", None))
          .await
          .map_err(conn_err)?;
      },
      MediaOverride::Unchanged => {},
    }
    // forced_colors -> Page.setForcedColors { forcedColors: "Active" | "None" | null }.
    match &opts.forced_colors {
      MediaOverride::Set(v) => {
        let val = match v.as_str() {
          "active" => "Active",
          "none" => "None",
          other => other,
        };
        self
          .target_session()
          .send("Page.setForcedColors", json!({ "forcedColors": val }))
          .await
          .map_err(conn_err)?;
      },
      MediaOverride::Disabled => {
        self
          .target_session()
          .send("Page.setForcedColors", json!({ "forcedColors": Value::Null }))
          .await
          .map_err(conn_err)?;
      },
      MediaOverride::Unchanged => {},
    }
    // contrast -> PrefersContrast: "More" | "NoPreference".
    match &opts.contrast {
      MediaOverride::Set(v) => {
        let val = match v.as_str() {
          "more" => "More",
          "no-preference" => "NoPreference",
          other => other,
        };
        self
          .target_session()
          .send("Page.overrideUserPreference", user_pref("PrefersContrast", Some(val)))
          .await
          .map_err(conn_err)?;
      },
      MediaOverride::Disabled => {
        self
          .target_session()
          .send("Page.overrideUserPreference", user_pref("PrefersContrast", None))
          .await
          .map_err(conn_err)?;
      },
      MediaOverride::Unchanged => {},
    }
    Ok(())
  }

  pub async fn set_extra_http_headers(&self, headers: &rustc_hash::FxHashMap<String, String>) -> Result<()> {
    let map: serde_json::Map<String, Value> = headers
      .iter()
      .map(|(k, v)| (k.clone(), Value::String(v.clone())))
      .collect();
    self
      .target_session()
      .send("Network.setExtraHTTPHeaders", json!({ "headers": map }))
      .await
      .map_err(conn_err)?;
    Ok(())
  }

  pub async fn reset_permissions(&self) -> Result<()> {
    if let Some(ctx) = &self.context_id {
      self
        .browser
        .send(
          "Playwright.resetPermissions",
          json!({ "browserContextId": ctx.to_string() }),
        )
        .await
        .map_err(conn_err)?;
    }
    Ok(())
  }

  // ── Tracing ───────────────────────────────────────────────────────────

  pub async fn start_tracing(&self) -> Result<()> {
    tokio::task::yield_now().await;
    Ok(())
  }

  pub async fn stop_tracing(&self) -> Result<()> {
    tokio::task::yield_now().await;
    Ok(())
  }

  pub async fn metrics(&self) -> Result<Vec<MetricData>> {
    tokio::task::yield_now().await;
    Ok(Vec::new())
  }

  // ── Listeners ─────────────────────────────────────────────────────────

  /// Re-route the listener (already spawned in [`Self::attach`]) to
  /// write captured console / network / dialog events into the
  /// caller's [`tokio::sync::RwLock`] sinks. Cheap pointer swap — the
  /// running listener picks up the new sinks on its next event.
  pub fn attach_listeners(
    &self,
    console_log: Arc<tokio::sync::RwLock<Vec<ConsoleMessage>>>,
    network_log: Arc<tokio::sync::RwLock<Vec<NetworkRequest>>>,
    dialog_log: Arc<tokio::sync::RwLock<Vec<DialogEvent>>>,
  ) {
    self.console_log.store(console_log);
    self.network_log.store(network_log);
    self.dialog_log.store(dialog_log);
  }

  // ── PDF / screencast ──────────────────────────────────────────────────

  pub async fn pdf(&self, _opts: crate::options::PdfOptions) -> Result<Vec<u8>> {
    tokio::task::yield_now().await;
    Err(FerriError::unsupported(
      "PDF generation is not supported on the webkit backend — Playwright's WebKit \
       protocol exposes no `Page.pdf` / `Page.printToPDF` command. Use cdp-pipe or cdp-raw for PDF.",
    ))
  }

  pub async fn start_screencast(
    &self,
    quality: u8,
    max_width: u32,
    max_height: u32,
  ) -> Result<tokio::sync::mpsc::UnboundedReceiver<(Vec<u8>, f64)>> {
    use base64::Engine as _;
    // Subscribe BEFORE startScreencast so we don't drop frames that
    // fire between the call returning and the listener spawning.
    let mut events = self.proxy.events();
    self
      .proxy
      .send(
        "Screencast.startScreencast",
        json!({
          "width": max_width,
          "height": max_height,
          "toolbarHeight": 0,
          "quality": quality,
        }),
      )
      .await
      .map_err(conn_err)?;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<(Vec<u8>, f64)>();
    tokio::spawn(async move {
      loop {
        let env = match events.recv().await {
          Ok(e) => e,
          Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
          Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        };
        if env.method.as_deref() != Some("Screencast.screencastFrame") {
          continue;
        }
        let data = env.params.get("data").and_then(Value::as_str).unwrap_or("");
        let ts = env.params.get("timestamp").and_then(Value::as_f64).unwrap_or(0.0);
        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) else {
          continue;
        };
        if tx.send((bytes, ts)).is_err() {
          break;
        }
      }
    });
    Ok(rx)
  }

  pub async fn stop_screencast(&self) -> Result<()> {
    let _ = self.proxy.send("Screencast.stopScreencast", json!({})).await;
    Ok(())
  }

  // ── File upload / interception ────────────────────────────────────────

  pub async fn set_file_input(&self, selector: &str, paths: &[String]) -> Result<()> {
    let elem = self.find_element(selector).await?;
    let AnyElement::WebKit(e) = elem else {
      return Err(FerriError::backend("set_file_input: non-webkit element"));
    };
    self
      .target_session()
      .send(
        "DOM.setInputFiles",
        json!({ "objectId": e.object_id(), "paths": paths }),
      )
      .await
      .map_err(conn_err)?;
    Ok(())
  }

  pub async fn route(
    &self,
    matcher: crate::url_matcher::UrlMatcher,
    handler: crate::route::RouteHandler,
    times: Option<u32>,
  ) -> Result<()> {
    self.ensure_interception_enabled().await?;
    self
      .routes
      .write()
      .await
      .push(crate::route::RegisteredRoute::new(matcher, handler, times));
    Ok(())
  }

  async fn ensure_interception_enabled(&self) -> Result<()> {
    if self.intercept_enabled.swap(true, Ordering::SeqCst) {
      return Ok(());
    }
    self
      .target_session()
      .send("Network.setInterceptionEnabled", json!({ "enabled": true }))
      .await
      .map_err(conn_err)?;
    self
      .target_session()
      .send(
        "Network.addInterception",
        json!({ "url": ".*", "stage": "request", "isRegex": true }),
      )
      .await
      .map_err(conn_err)?;
    Ok(())
  }

  pub async fn unroute(&self, matcher: &crate::url_matcher::UrlMatcher) -> Result<()> {
    self.routes.write().await.retain(|r| !r.matcher.equivalent(matcher));
    Ok(())
  }

  pub async fn unroute_all(&self, _behavior: crate::options::UnrouteBehavior) -> Result<()> {
    self.routes.write().await.clear();
    Ok(())
  }

  pub async fn enable_file_chooser_intercept(&self) -> Result<()> {
    let _ = self
      .target_session()
      .send("Page.setInterceptFileChooserDialog", json!({ "enabled": true }))
      .await;
    Ok(())
  }

  pub async fn enable_download_behavior(&self) -> Result<()> {
    tokio::task::yield_now().await;
    Ok(())
  }

  // ── Exposed functions / init scripts ──────────────────────────────────

  pub async fn expose_function(&self, name: &str, func: crate::events::ExposedFn) -> Result<()> {
    self.ensure_binding_channel().await?;
    self.exposed_fns.write().await.insert(name.to_string(), func);
    let register_js = format!("globalThis.__fd_bc.add('{}')", crate::steps::js_escape(name));
    self.add_init_script(&register_js).await?;
    self.runtime_evaluate(&register_js, true).await?;
    Ok(())
  }

  pub async fn remove_exposed_function(&self, name: &str) -> Result<()> {
    self.exposed_fns.write().await.remove(name);
    let js = format!(
      "if(globalThis.__fd_bc)globalThis.__fd_bc.del('{}')",
      crate::steps::js_escape(name)
    );
    let _ = self.runtime_evaluate(&js, true).await;
    Ok(())
  }

  /// Install `__fd_binding__` binding + `__fd_bc` controller JS on
  /// first use, then spawn the listener task that dispatches
  /// `Runtime.bindingCalled` events back through registered callbacks.
  async fn ensure_binding_channel(&self) -> Result<()> {
    if self.binding_initialized.swap(true, Ordering::SeqCst) {
      return Ok(());
    }
    // Subscribe BEFORE evaluating the controller. User JS that calls
    // the binding can run as soon as `add_init_script` lands, so the
    // listener must already be live by then.
    let target = self.target_session();
    let mut rx = target.events();
    self
      .target_session()
      .send("Runtime.addBinding", json!({ "name": "__fd_binding__" }))
      .await
      .map_err(conn_err)?;
    self
      .add_init_script(crate::backend::cdp::CdpPage::<crate::backend::cdp::pipe::PipeTransport>::BINDING_CONTROLLER_JS)
      .await?;
    self
      .runtime_evaluate(
        crate::backend::cdp::CdpPage::<crate::backend::cdp::pipe::PipeTransport>::BINDING_CONTROLLER_JS,
        true,
      )
      .await?;

    let fns = self.exposed_fns.clone();
    tokio::spawn(async move {
      loop {
        let env = match rx.recv().await {
          Ok(e) => e,
          Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
          Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        };
        if env.method.as_deref() != Some("Runtime.bindingCalled") {
          continue;
        }
        if env.params.get("name").and_then(Value::as_str) != Some("__fd_binding__") {
          continue;
        }
        let payload_str = env.params.get("argument").and_then(Value::as_str).unwrap_or("{}");
        let payload: Value = serde_json::from_str(payload_str).unwrap_or_default();
        let fn_name = payload.get("name").and_then(Value::as_str).unwrap_or("").to_string();
        let seq = payload.get("seq").and_then(Value::as_u64).unwrap_or(0);
        let args: Vec<Value> = payload
          .get("args")
          .and_then(Value::as_array)
          .cloned()
          .unwrap_or_default();
        let maybe_fn = fns.read().await.get(&fn_name).cloned();
        let deliver_js = if let Some(callback) = maybe_fn {
          let result = callback(args).await;
          format!(
            "globalThis.__fd_bc.resolve({}, {})",
            seq,
            serde_json::to_string(&result).unwrap_or_else(|_| "null".into())
          )
        } else {
          format!("globalThis.__fd_bc.reject({seq}, 'Function not found: {fn_name}')")
        };
        let _ = target
          .send(protocol::RUNTIME_EVALUATE, json!({ "expression": deliver_js }))
          .await;
      }
    });
    Ok(())
  }

  pub async fn add_init_script(&self, source: &str) -> Result<String> {
    let id = self.init_script_seq.fetch_add(1, Ordering::Relaxed).to_string();
    {
      let mut scripts = self.init_scripts.lock().await;
      scripts.push((id.clone(), source.to_string()));
    }
    self.flush_bootstrap_script().await?;
    Ok(id)
  }

  pub async fn remove_init_script(&self, identifier: &str) -> Result<()> {
    {
      let mut scripts = self.init_scripts.lock().await;
      scripts.retain(|(id, _)| id != identifier);
    }
    self.flush_bootstrap_script().await?;
    Ok(())
  }

  /// Re-send `Page.setBootstrapScript` with the JOINED accumulation of
  /// every registered init-script fragment. `Page.setBootstrapScript`
  /// holds a single source that each call overwrites, so the only way
  /// to keep N scripts live across navigations is to concatenate them
  /// (mirrors `wkPage.ts::_calculateBootstrapScript`).
  async fn flush_bootstrap_script(&self) -> Result<()> {
    let joined = {
      let scripts = self.init_scripts.lock().await;
      scripts
        .iter()
        .map(|(_, src)| src.as_str())
        .collect::<Vec<_>>()
        .join(";\n")
    };
    self
      .target_session()
      .send("Page.setBootstrapScript", json!({ "source": joined }))
      .await
      .map_err(conn_err)?;
    Ok(())
  }

  // ── Handle / utility evaluate ─────────────────────────────────────────

  /// ferridriver's analogue of Playwright's `evaluateExpression` — runs
  /// the shared `UTILITY_EVAL_WRAPPER` against the page's `UtilityScript`.
  /// `Runtime.callFunctionOn` (which has `awaitPromise`) anchored on the
  /// main-world global, or on the first handle's `objectId` when handles
  /// are supplied (free context anchor, same realm).
  pub async fn call_utility_evaluate(
    &self,
    fn_source: &str,
    args: &[crate::protocol::SerializedValue],
    handles: &[crate::protocol::HandleId],
    _frame_id: Option<&str>,
    is_function: Option<bool>,
    return_by_value: bool,
  ) -> Result<crate::js_handle::EvaluateResult> {
    self.ensure_engine_injected().await?;

    let args_json = serde_json::to_string(args)?;
    let is_fn: Value = match is_function {
      Some(b) => Value::Bool(b),
      None => Value::Null,
    };
    let count = args.len();
    let mut arguments: Vec<Value> = vec![
      json!({ "value": is_fn }),
      json!({ "value": return_by_value }),
      json!({ "value": fn_source }),
      json!({ "value": count }),
      json!({ "value": args_json }),
    ];
    let mut anchor: Option<String> = None;
    for handle in handles {
      match handle {
        crate::protocol::HandleId::WebKit(obj) => {
          if anchor.is_none() {
            anchor = Some(obj.clone());
          }
          arguments.push(json!({ "objectId": obj }));
        },
        other => {
          return Err(FerriError::invalid_argument(
            "handles",
            format!("call_utility_evaluate: non-webkit handle {other:?} on webkit backend"),
          ));
        },
      }
    }
    let anchor = match anchor {
      Some(a) => {
        // The wrapper runs in the realm the anchor handle belongs to.
        // A handle resolved in a child frame carries that frame's
        // `injectedScriptId` (== execution context id) inside its
        // objectId JSON; `window.__fd` must exist in THAT realm for the
        // UtilityScript wrapper to work. Inject it there if needed.
        if let Some(ctx_id) = object_id_context(&a)
          && self.frame_contexts.read().await.values().any(|&v| v == ctx_id)
        {
          self.ensure_engine_in_context(ctx_id).await?;
        }
        a
      },
      None => self.global_anchor().await?,
    };
    let resp = self
      .target_session()
      .send(
        protocol::RUNTIME_CALL_FUNCTION_ON,
        json!({
          "objectId": anchor,
          "functionDeclaration": crate::backend::cdp::UTILITY_EVAL_WRAPPER,
          "arguments": arguments,
          "returnByValue": return_by_value,
          "awaitPromise": true,
        }),
      )
      .await
      .map_err(conn_err)?;
    parse_eval_response(&resp, return_by_value)
  }

  pub async fn release_object(&self, object_id: &str) -> Result<()> {
    let _ = self
      .target_session()
      .send(protocol::RUNTIME_RELEASE_OBJECT, json!({ "objectId": object_id }))
      .await;
    Ok(())
  }

  #[must_use]
  pub fn element_from_object_id(&self, object_id: String) -> WebKitElement {
    WebKitElement::new(self.target_session(), object_id)
  }

  // ── Lifecycle ─────────────────────────────────────────────────────────

  pub async fn close_page(&self, _opts: crate::options::PageCloseOptions) -> Result<()> {
    if self.closed.swap(true, Ordering::Relaxed) {
      return Ok(());
    }
    let target_id = self.target_id();
    let send = self.proxy.send(
      "Target.close",
      json!({ "targetId": target_id.to_string(), "runBeforeUnload": false }),
    );
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), send).await;
    // Reject pending in-flight calls bound to this page's proxy + target
    // routes so successive callers don't pile up indefinitely.
    let conn = self.proxy.connection_handle();
    conn.close_route(Some(&self.proxy_id), Some(&target_id));
    conn.close_route(Some(&self.proxy_id), None);
    Ok(())
  }

  #[must_use]
  pub fn is_closed(&self) -> bool {
    self.closed.load(Ordering::Relaxed)
  }
}

/// Apply per-page context overrides via the page's target + proxy
/// sessions BEFORE the about:blank document becomes scriptable.
///
/// Mirrors `wkPage.ts::_initializeSessionMayThrow`: userAgent /
/// timezone / bypassCSP / offline / screen / permissions live on the
/// target session; JS-disabled lives on the page-proxy. Anything that
/// hits the browser-root session (locale, geolocation) is sent from
/// `WebKitBrowser::new_context_with_options` instead — that runs
/// once per context, not once per page.
async fn apply_pre_page_overrides(target: &Session, proxy: &Session, opts: &crate::options::BrowserContextOptions) {
  if let Some(ua) = opts.user_agent.as_deref() {
    let _ = target.send("Page.overrideUserAgent", json!({ "value": ua })).await;
  }
  if let Some(tz) = opts.timezone_id.as_deref() {
    let _ = target.send("Page.setTimeZone", json!({ "timeZone": tz })).await;
  }
  if let Some(true) = opts.bypass_csp {
    let _ = target.send("Page.setBypassCSP", json!({ "enabled": true })).await;
  }
  if let Some(o) = opts.offline {
    let _ = target
      .send("Network.setEmulateOfflineState", json!({ "offline": o }))
      .await;
  }
  if let Some(screen) = opts.screen {
    let _ = target
      .send(
        "Page.setScreenSizeOverride",
        json!({ "width": screen.width, "height": screen.height }),
      )
      .await;
  }
  if let Some(headers) = opts.extra_http_headers.as_ref() {
    let map: serde_json::Map<String, Value> = headers
      .iter()
      .map(|(k, v)| (k.clone(), Value::String(v.clone())))
      .collect();
    let _ = target
      .send("Network.setExtraHTTPHeaders", json!({ "headers": map }))
      .await;
  }
  if matches!(opts.java_script_enabled, Some(false)) {
    let _ = proxy
      .send("Emulation.setJavaScriptEnabled", json!({ "enabled": false }))
      .await;
  }
  if let Some(perms) = opts.permissions.as_ref() {
    let _ = proxy
      .send(
        "Emulation.grantPermissions",
        json!({ "origin": "*", "permissions": perms }),
      )
      .await;
  }
}

/// Wait for the first non-provisional `Target.targetCreated` of type
/// `page` on the proxy session.
async fn wait_for_first_page_target(proxy: &Session) -> std::result::Result<String, BrowserError> {
  let mut rx = proxy.events();
  loop {
    match rx.recv().await {
      Ok(env) => {
        if let Some(id) = page_target_id(&env) {
          return Ok(id);
        }
      },
      Err(RecvError::Lagged(_)) => {},
      Err(RecvError::Closed) => {
        return Err(BrowserError::Protocol("page proxy closed before target".into()));
      },
    }
  }
}

fn page_target_id(env: &Envelope) -> Option<String> {
  if env.method.as_deref()? != "Target.targetCreated" {
    return None;
  }
  let info = env.params.get("targetInfo")?;
  if info.get("type").and_then(Value::as_str) != Some("page")
    || info.get("isProvisional").and_then(Value::as_bool).unwrap_or(false)
  {
    return None;
  }
  Some(info.get("targetId")?.as_str()?.to_string())
}

/// Walk a PW `Page.FrameResourceTree` into flat [`FrameInfo`]s.
fn collect_frames(node: &Value, parent: Option<&str>, out: &mut Vec<FrameInfo>) {
  let Some(frame) = node.get("frame") else {
    return;
  };
  let frame_id = frame.get("id").and_then(Value::as_str).unwrap_or("").to_string();
  out.push(FrameInfo {
    frame_id: frame_id.clone(),
    parent_frame_id: parent.map(str::to_string),
    name: frame.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
    url: frame.get("url").and_then(Value::as_str).unwrap_or("").to_string(),
  });
  if let Some(children) = node.get("childFrames").and_then(Value::as_array) {
    for child in children {
      collect_frames(child, Some(&frame_id), out);
    }
  }
}

/// Map a [`super::connection::ConnectionError`] to a [`FerriError`].
fn conn_err(e: super::connection::ConnectionError) -> FerriError {
  e.into()
}

/// Extract the execution-context id (`injectedScriptId`) from a PW
/// `WebKit` `objectId`. The wire form is a JSON string like
/// `{"injectedScriptId":6,"id":42}`; the `injectedScriptId` is the
/// `Runtime.ExecutionContextId` the object lives in.
fn object_id_context(object_id: &str) -> Option<i64> {
  serde_json::from_str::<Value>(object_id)
    .ok()?
    .get("injectedScriptId")
    .and_then(Value::as_i64)
}

/// Decode a `Runtime.callFunctionOn` reply produced by the
/// `UTILITY_EVAL_WRAPPER` into an [`EvaluateResult`]. PW `WebKit`
/// signals errors via `wasThrown` (not CDP's `exceptionDetails`).
fn parse_eval_response(resp: &Value, return_by_value: bool) -> Result<crate::js_handle::EvaluateResult> {
  use crate::js_handle::{EvaluateResult, HandleRemote, JSHandleBacking};
  use crate::protocol::{SerializationContext, SerializedValue, SpecialValue};

  let result = resp
    .get("result")
    .ok_or_else(|| FerriError::protocol("Runtime.callFunctionOn", "call_utility_evaluate: no result"))?;
  if resp.get("wasThrown").and_then(Value::as_bool).unwrap_or(false) {
    let text = result
      .get("description")
      .or_else(|| result.get("value"))
      .and_then(Value::as_str)
      .unwrap_or("evaluation threw");
    return Err(FerriError::evaluation(text.to_string()));
  }

  if return_by_value {
    // The wrapper `JSON.stringify`d the isomorphic wire shape, so the
    // backend ships back a plain string; `null` is the `undefined`
    // sentinel.
    let wire = result.get("value").cloned().unwrap_or(Value::Null);
    let parsed: SerializedValue = match wire {
      Value::Null => SerializedValue::Special(SpecialValue::Undefined),
      Value::String(ref s) => {
        let inner: Value = serde_json::from_str(s)
          .map_err(|e| FerriError::backend(format!("call_utility_evaluate: inner JSON: {e}")))?;
        serde_json::from_value(inner)
          .map_err(|e| FerriError::backend(format!("call_utility_evaluate: parse result: {e}")))?
      },
      other => serde_json::from_value(other)
        .map_err(|e| FerriError::backend(format!("call_utility_evaluate: parse result: {e}")))?,
    };
    return Ok(EvaluateResult::Value(parsed));
  }

  if let Some(obj_id) = result.get("objectId").and_then(Value::as_str) {
    let is_node = result.get("subtype").and_then(Value::as_str) == Some("node");
    return Ok(EvaluateResult::Handle(
      JSHandleBacking::Remote(HandleRemote::WebKit(Arc::from(obj_id))),
      is_node,
    ));
  }

  let value = result.get("value").cloned().unwrap_or(Value::Null);
  let serialized = if value.is_null() {
    if result.get("type").and_then(Value::as_str) == Some("undefined") {
      SerializedValue::Special(SpecialValue::Undefined)
    } else {
      SerializedValue::Special(SpecialValue::Null)
    }
  } else {
    SerializedValue::from_json(&value, &mut SerializationContext::default())
  };
  Ok(EvaluateResult::Handle(JSHandleBacking::Value(serialized), false))
}
