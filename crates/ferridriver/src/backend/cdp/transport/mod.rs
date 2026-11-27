//! Transport trait and shared CDP message dispatch logic.
//!
//! The dispatch logic (response correlation, document-accurate lifecycle
//! tracking, event broadcast) is identical for pipe and WebSocket
//! transports. It lives here as `CdpDispatcher` — both transports embed
//! it and call `dispatch_message`.
//!
//! Navigation waits are driven entirely off the per-session
//! [`super::LifecycleState`] (commit + lifecycle, both gated on the
//! navigation's `loaderId`). `Page.loadEventFired` / `Page.domContentEventFired`
//! are deliberately ignored — they're page-level events with no
//! loaderId, so a late-arriving `loadEventFired` from a previous
//! document can resolve a fresh wait before the new document has even
//! committed (see `Frame.gotoImpl` in
//! `/tmp/playwright/packages/playwright-core/src/server/frames.ts` —
//! Playwright likewise drives navigation off `Page.lifecycleEvent` only).

use dashmap::DashMap;
use rustc_hash::FxHashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Instant;
use tokio::sync::{broadcast, oneshot};

use crate::backend::json_scan;
use crate::error::{FerriError, Result};

/// Truncate a string for logging, appending "..." if truncated.
fn truncate_for_log(s: &str, max: usize) -> String {
  let (head, tail) = s.split_at(s.floor_char_boundary(max));
  if tail.is_empty() {
    head.to_string()
  } else {
    format!("{head}...")
  }
}

/// Result of a single CDP command: either the response value or a typed error.
type CdpResult = Result<serde_json::Value>;

/// In-flight CDP command entry. Carries the response oneshot plus the
/// method name and send timestamp so [`RttStats`] can attribute the
/// observed round-trip latency to the right CDP method when the
/// response lands. The method name is stored as `String` because CDP
/// method strings come from arbitrary callers (`&str`) and we need
/// owned storage to outlive the borrow; the alloc is amortised
/// against the per-command serialization cost (~µs scale, much
/// larger than the alloc itself).
pub(crate) struct PendingEntry {
  tx: oneshot::Sender<CdpResult>,
  session_id: Option<Box<str>>,
  method: String,
  send_at: Option<Instant>,
}

/// Pending-command map: command ID -> [`PendingEntry`].
///
/// Sharded via [`dashmap::DashMap`] so concurrent senders don't
/// serialise on a single global mutex. Each shard has its own
/// internal `RwLock`; insert/remove on different keys are wait-free
/// vs each other. Replaced `Arc<std::sync::Mutex<FxHashMap>>` —
/// uncontended insert went from ~100ns (mutex acq + `HashMap` insert)
/// to ~50ns (shard lookup + per-shard insert), and contention at
/// 4+ concurrent senders no longer serialises. The per-key mutex
/// model would be even cheaper but requires `parking_lot::Mutex`
/// per entry which complicates the lifetime story for one-shot
/// senders. `DashMap` is the right balance.
pub(crate) type PendingMap = DashMap<u64, PendingEntry>;

mod dispatcher;
use dispatcher::frame_event;
mod rtt;
#[cfg(test)]
mod tests;

pub(crate) use rtt::RttStats;
pub use rtt::dump_global_rtt_stats;
use rtt::{global_rtt_stats, rtt_stats_enabled};
#[cfg(test)]
pub(crate) use tests::verify_operation_budgets;

/// Outbound wire sink for [`CdpTransport`].
pub(crate) enum WireSender {
  Pipe(tokio::sync::mpsc::Sender<Vec<u8>>),
  Ws(tokio::sync::mpsc::Sender<tokio_tungstenite::tungstenite::Message>),
  #[cfg(test)]
  Null,
}

/// Concrete CDP transport over either Unix pipes (`--remote-debugging-pipe`)
/// or WebSocket (`--remote-debugging-port` / `connectOverCDP`).
///
/// Both transports share `CdpDispatcher` for response correlation, lifecycle
/// tracking, and event taps; only the outbound wire framing differs.
pub struct CdpTransport {
  dispatcher: Arc<CdpDispatcher>,
  sender: WireSender,
  tasks: Option<crate::backend::transport_tasks::TransportTasks>,
  endpoint: Option<Arc<str>>,
}

impl CdpTransport {
  pub(crate) fn from_pipe(
    write_tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    dispatcher: Arc<CdpDispatcher>,
    tasks: crate::backend::transport_tasks::TransportTasks,
  ) -> Self {
    Self {
      dispatcher,
      sender: WireSender::Pipe(write_tx),
      tasks: Some(tasks),
      endpoint: None,
    }
  }

  pub(crate) fn from_ws(
    write_tx: tokio::sync::mpsc::Sender<tokio_tungstenite::tungstenite::Message>,
    dispatcher: Arc<CdpDispatcher>,
    tasks: crate::backend::transport_tasks::TransportTasks,
    endpoint: Arc<str>,
  ) -> Self {
    Self {
      dispatcher,
      sender: WireSender::Ws(write_tx),
      tasks: Some(tasks),
      endpoint: Some(endpoint),
    }
  }

  #[cfg(test)]
  pub(crate) fn from_test_dispatcher(dispatcher: Arc<CdpDispatcher>) -> Self {
    Self {
      dispatcher,
      sender: WireSender::Null,
      tasks: None,
      endpoint: None,
    }
  }

  /// Whether this transport runs over Unix pipes (`CdpPipe`) or WebSocket (`CdpWs`).
  #[must_use]
  pub fn kind(&self) -> crate::backend::BackendKind {
    match self.sender {
      WireSender::Pipe(_) => crate::backend::BackendKind::CdpPipe,
      WireSender::Ws(_) => crate::backend::BackendKind::CdpWs,
      #[cfg(test)]
      WireSender::Null => crate::backend::BackendKind::CdpPipe,
    }
  }

  /// The browser's `DevTools` WebSocket endpoint when connected over WebSocket.
  #[must_use]
  pub fn endpoint(&self) -> Option<&str> {
    self.endpoint.as_deref()
  }

  /// # Errors
  ///
  /// Returns an error if stopping the transport tasks fails.
  pub async fn close(&self) -> Result<()> {
    self.dispatcher.fail_all_pending("CDP transport closed");
    if let Some(tasks) = &self.tasks {
      tasks.close().await?;
    }
    Ok(())
  }

  /// Whether the browser connection has ended (EOF on the pipe/socket).
  #[must_use]
  pub fn is_disconnected(&self) -> bool {
    self.dispatcher.is_disconnected()
  }

  /// Resolves once the browser connection has ended.
  pub fn closed(&self) -> futures::future::BoxFuture<'static, ()> {
    match &self.tasks {
      Some(tasks) => tasks.closed(),
      None => Box::pin(std::future::pending()),
    }
  }

  async fn dispatch_command(
    &self,
    id: u64,
    method: &str,
    mut data: Vec<u8>,
    rx: oneshot::Receiver<CdpResult>,
    budget: crate::operation_budget::OperationBudget,
  ) -> Result<serde_json::Value> {
    let mut pending = self.dispatcher.pending_command(id);
    match &self.sender {
      WireSender::Pipe(tx) => {
        budget.send(tx, data).await?;
      },
      WireSender::Ws(tx) => {
        if data.last() == Some(&0) {
          data.pop();
        }
        let text = String::from_utf8(data).map_err(|error| FerriError::backend(format!("UTF-8: {error}")))?;
        budget
          .send(tx, tokio_tungstenite::tungstenite::Message::Text(text.into()))
          .await?;
      },
      #[cfg(test)]
      WireSender::Null => {
        pending.completed();
        return Ok(serde_json::Value::Null);
      },
    }
    let result = budget
      .wait(rx)
      .await
      .map_err(|error| error.error(format!("waiting for {method} response")))?
      .map_err(|_| FerriError::backend(format!("Response channel dropped for {method}")))?;
    pending.completed();
    result
  }

  /// Send a strongly typed CDP command, serializing `cmd` directly onto the
  /// wire without constructing an intermediate `serde_json::Value`, and decode
  /// the response into `C::Response`.
  ///
  /// # Errors
  ///
  /// Returns an error if the transport is disconnected, serialization or
  /// response decoding fails, the command outlives its operation budget, or
  /// Chrome rejects the command.
  #[tracing::instrument(skip(self, session_id, cmd), fields(method = C::METHOD))]
  pub async fn send<C: ferridriver_cdp_proto::Command>(
    &self,
    session_id: Option<&str>,
    cmd: &C,
  ) -> Result<C::Response> {
    let budget = crate::operation_budget::OperationBudget::current(30_000)?;
    let (id, data, rx) = self.dispatcher.build_typed_command(session_id, C::METHOD, cmd)?;
    let value = self.dispatch_command(id, C::METHOD, data, rx, budget).await?;
    serde_json::from_value(value).map_err(|e| FerriError::protocol(C::METHOD, format!("decode response: {e}")))
  }

  /// # Errors
  ///
  /// Returns an error if the transport is disconnected, serialization fails, the command
  /// outlives its operation budget, or Chrome rejects the command.
  #[tracing::instrument(skip(self, session_id, params), fields(method))]
  pub async fn send_command(
    &self,
    session_id: Option<&str>,
    method: &str,
    params: &serde_json::Value,
  ) -> Result<serde_json::Value> {
    let budget = crate::operation_budget::OperationBudget::current(30_000)?;
    let (id, data, rx) = self.dispatcher.build_command(session_id, method, params)?;
    self.dispatch_command(id, method, data, rx, budget).await
  }

  /// Send a CDP command whose `params` object is already valid serialized JSON.
  /// Avoids intermediate `serde_json::Value` trees, re-escaping large static
  /// scripts (like `engine.min.js`), and incremental buffer growth.
  ///
  /// # Errors
  ///
  /// Returns an error if the transport is disconnected, the command outlives its operation
  /// budget, or Chrome rejects the command.
  #[tracing::instrument(skip(self, session_id, raw_params_json), fields(method))]
  pub async fn send_command_raw(
    &self,
    session_id: Option<&str>,
    method: &str,
    raw_params_json: &str,
  ) -> Result<serde_json::Value> {
    let budget = crate::operation_budget::OperationBudget::current(30_000)?;
    let (id, data, rx) = self.dispatcher.build_command_raw(session_id, method, raw_params_json)?;
    self.dispatch_command(id, method, data, rx, budget).await
  }

  #[must_use]
  pub fn subscribe_events(&self) -> broadcast::Receiver<Arc<serde_json::Value>> {
    self.dispatcher.subscribe_events()
  }

  #[must_use]
  pub fn subscribe_event_method(&self, method: &'static str) -> broadcast::Receiver<Arc<serde_json::Value>> {
    self.dispatcher.subscribe_event_method(method)
  }

  #[must_use]
  pub fn subscribe_event_domain(&self, domain: &'static str) -> broadcast::Receiver<Arc<serde_json::Value>> {
    self.dispatcher.subscribe_event_domain(domain)
  }

  /// Lossless, wire-ordered event tap for the given exact CDP methods,
  /// optionally filtered to one session. Unlike the broadcast
  /// subscriptions above, a tap never drops events — state-mutating
  /// consumers (frame/context maps, network correlation, dialog /
  /// download / file-chooser registries, Fetch interception) MUST use
  /// taps; a dropped event there corrupts the map or hangs the page.
  #[must_use]
  pub fn tap_event_methods(
    &self,
    methods: &'static [&'static str],
    session_id: Option<&str>,
  ) -> tokio::sync::mpsc::UnboundedReceiver<Arc<serde_json::Value>> {
    self.dispatcher.tap_event_methods(methods, session_id)
  }

  /// Lossless, wire-ordered event tap for whole CDP domains
  /// (`Network`, `Runtime`, ...). Registering several domains on one
  /// tap yields a single stream ordered across those domains exactly
  /// as the events arrived on the wire.
  #[must_use]
  pub fn tap_event_domains(
    &self,
    domains: &'static [&'static str],
    session_id: Option<&str>,
  ) -> tokio::sync::mpsc::UnboundedReceiver<Arc<serde_json::Value>> {
    self.dispatcher.tap_event_domains(domains, session_id)
  }

  /// Lossless, wire-ordered tap of EVERY event carried by exactly one
  /// session. Strict session match: browser-scoped events do not leak
  /// into a page-session stream. Backs the public `CDPSession` events.
  #[must_use]
  pub fn tap_all_events(&self, session_id: &str) -> tokio::sync::mpsc::UnboundedReceiver<Arc<serde_json::Value>> {
    self.dispatcher.tap_all_events(session_id)
  }

  #[must_use]
  pub fn tap_iframe_targets(&self) -> IframeTargetSubscription {
    self.dispatcher.tap_iframe_targets()
  }

  pub fn register_lifecycle_tracker(
    &self,
    session_id: &str,
    state: Arc<std::sync::Mutex<super::LifecycleState>>,
    notify: Arc<tokio::sync::Notify>,
    frame_observer: FrameStateObserver,
  ) {
    self
      .dispatcher
      .register_lifecycle_tracker(session_id, state, notify, frame_observer);
  }

  /// Release everything the dispatcher holds for a page session that is
  /// going away. Taps are otherwise only pruned when an event of that
  /// exact method arrives, and the lifecycle tracker was never removed
  /// at all — both grew by an entry per page for the life of the
  /// browser.
  pub fn unregister_session(&self, session_id: &str) {
    self.dispatcher.unregister_session(session_id);
  }

  /// Per-session dirty flags tracked by the transport so context recycling
  /// knows whether the session can be kept alive and which clears can be skipped.
  #[must_use]
  pub fn take_session_dirty(&self, session_id: &str) -> SessionDirty {
    self.dispatcher.take_session_dirty(session_id)
  }

  /// Clear all dirty flags for `session_id` after bootstrap or recycle setup.
  pub fn reset_session_dirty(&self, session_id: &str) {
    self.dispatcher.reset_session_dirty(session_id);
  }

  /// Mark `session_id`'s pointer state dirty (used when a keep-session check
  /// consumed the flags before falling back to session replacement).
  pub fn mark_pointer_dirty(&self, session_id: &str) {
    self.dispatcher.mark_pointer_dirty(session_id);
  }

  /// Whether `Browser.grantPermissions` was called for `browser_context_id`,
  /// clearing the flag.
  #[must_use]
  pub fn take_permissions_dirty(&self, browser_context_id: &str) -> bool {
    self.dispatcher.take_permissions_dirty(browser_context_id)
  }

  /// Whether browser-scoped `Storage.setCookies` was called for
  /// `browser_context_id`, clearing the flag.
  #[must_use]
  pub fn take_cookies_dirty(&self, browser_context_id: &str) -> bool {
    self.dispatcher.take_cookies_dirty(browser_context_id)
  }
}

/// Dirty state accumulated on a CDP session since its last clean setup.
#[derive(Debug, Clone, Copy, Default)]
pub struct SessionDirty {
  /// Mouse or touch input was dispatched on this session.
  pub pointer: bool,
  /// A session-mutating command (emulation, init script, binding, Fetch,
  /// custom headers, OOPIF attach, etc.) ran on this session, so recycling
  /// must detach and replace the session instead of keeping it alive.
  pub session_state: bool,
  /// Real network traffic, cookie writes, or non-`about:blank` navigation
  /// occurred on this session, so cache and cookies must be cleared.
  pub network: bool,
}

/// Strict allowlist of CDP session methods that leave NO sticky session-level
/// overrides behind (no emulation, init scripts, bindings, Fetch interception,
/// headers, CSP bypass, etc.). Anything outside this set marks `session_state`
/// dirty so recycling falls back to full session detachment.
fn is_clean_session_command(method: &str) -> bool {
  matches!(
    method,
    "Runtime.evaluate"
      | "Runtime.callFunctionOn"
      | "Runtime.releaseObject"
      | "Runtime.getProperties"
      | "Page.navigate"
      | "Page.reload"
      | "Page.setDocumentContent"
      | "Page.getFrameTree"
      | "Page.getNavigationHistory"
      | "Page.navigateToHistoryEntry"
      | "Page.resetNavigationHistory"
      | "Page.captureScreenshot"
      | "Page.printToPDF"
      | "Page.bringToFront"
      | "Page.handleJavaScriptDialog"
      | "Page.createIsolatedWorld"
      | "Page.getLayoutMetrics"
      | "Page.stopLoading"
      | "Input.dispatchMouseEvent"
      | "Input.dispatchKeyEvent"
      | "Input.insertText"
      | "Input.dispatchTouchEvent"
      | "Input.imeSetComposition"
      | "DOM.describeNode"
      | "DOM.resolveNode"
      | "DOM.getDocument"
      | "DOM.querySelector"
      | "DOM.querySelectorAll"
      | "DOM.getBoxModel"
      | "DOM.getContentQuads"
      | "DOM.scrollIntoViewIfNeeded"
      | "DOM.focus"
      | "DOM.getOuterHTML"
      | "DOM.setFileInputFiles"
      | "DOM.getFrameOwner"
      | "DOM.requestNode"
      | "Network.getCookies"
      | "Network.setCookie"
      | "Network.setCookies"
      | "Network.deleteCookies"
      | "Network.clearBrowserCookies"
      | "Network.clearBrowserCache"
      | "Storage.getCookies"
      | "Storage.setCookies"
      | "Storage.clearCookies"
      | "Storage.clearDataForOrigin"
      | "Accessibility.getFullAXTree"
      | "Accessibility.getPartialAXTree"
      | "CSS.getComputedStyleForNode"
  )
}

// ── Shared dispatch state ──────────────────────────────────────────────────

pub type FrameStateObserver = Arc<dyn Fn(&[u8], &str) + Send + Sync>;

pub(crate) fn frame_state_observer(
  cache: Arc<std::sync::Mutex<crate::frame_cache::FrameCache>>,
  events: crate::events::EventEmitter,
) -> FrameStateObserver {
  make_frame_state_observer(cache, events, None)
}

pub(crate) fn frame_state_observer_for_renderer(
  cache: Arc<std::sync::Mutex<crate::frame_cache::FrameCache>>,
  events: crate::events::EventEmitter,
  frame: String,
  parent: String,
) -> FrameStateObserver {
  make_frame_state_observer(cache, events, Some((frame, parent)))
}

fn make_frame_state_observer(
  cache: Arc<std::sync::Mutex<crate::frame_cache::FrameCache>>,
  events: crate::events::EventEmitter,
  renderer: Option<(String, String)>,
) -> FrameStateObserver {
  Arc::new(move |raw, method| {
    if method == "Page.frameDetached" {
      let params = json_scan::json_field(raw, b"params");
      if json_scan::json_string(json_scan::json_field(params, b"reason")) == b"swap" {
        if let Ok(frame) = std::str::from_utf8(json_scan::json_string(json_scan::json_field(params, b"frameId"))) {
          lock_or_recover(&cache).detach_descendants(frame);
        }
        return;
      }
    }
    if let Some(mut event) = frame_event(raw, method) {
      if let Some((frame, parent)) = &renderer
        && let crate::events::PageEvent::FrameNavigated(info) = &mut event
        && info.frame_id == *frame
      {
        info.parent_frame_id = Some(parent.clone());
      }
      {
        let mut cache = lock_or_recover(&cache);
        match &event {
          crate::events::PageEvent::FrameAttached(info) => cache.attach(info.clone()),
          crate::events::PageEvent::FrameDetached { frame_id } => cache.detach(frame_id),
          crate::events::PageEvent::FrameNavigated(info) => cache.navigated(info.clone()),
          crate::events::PageEvent::FrameNavigatedWithinDocument(info) => {
            cache.navigated_within(&info.frame_id, &info.url);
          },
          _ => {},
        }
      }
      events.emit(event);
    }
  })
}

pub(crate) struct LifecycleTracker {
  pub state: Arc<std::sync::Mutex<super::LifecycleState>>,
  pub notify: Arc<tokio::sync::Notify>,
  pub frame_observer: super::transport::FrameStateObserver,
}

pub(crate) struct PendingCommand<'a> {
  dispatcher: &'a CdpDispatcher,
  id: Option<u64>,
}

impl PendingCommand<'_> {
  pub(crate) fn completed(&mut self) {
    self.id = None;
  }
}

impl Drop for PendingCommand<'_> {
  fn drop(&mut self) {
    if let Some(id) = self.id {
      self.dispatcher.forget_pending(id);
    }
  }
}

#[derive(Default)]
struct ContextSnapshots(FxHashMap<String, FxHashMap<(String, bool), CachedContext>>);

struct CachedContext {
  id: i64,
  event: Vec<u8>,
}

impl ContextSnapshots {
  fn observe(&mut self, session: &str, method: &str, raw: &[u8]) {
    if method == "Runtime.executionContextsCleared" {
      self.0.remove(session);
      return;
    }
    let Ok(event) = serde_json::from_slice::<serde_json::Value>(raw) else {
      return;
    };
    let params = &event["params"];
    match method {
      "Runtime.executionContextCreated" => {
        let context = &params["context"];
        let is_default = context["auxData"]["isDefault"] == true;
        let is_utility = !is_default && context["name"].as_str() == Some(super::UTILITY_WORLD);
        if (is_default || is_utility)
          && let (Some(frame), Some(id)) = (context["auxData"]["frameId"].as_str(), context["id"].as_i64())
        {
          self.0.entry(session.to_owned()).or_default().insert(
            (frame.to_owned(), is_utility),
            CachedContext {
              id,
              event: raw.to_vec(),
            },
          );
        }
      },
      "Runtime.executionContextDestroyed" => {
        if let Some(id) = params["executionContextId"].as_i64()
          && let Some(contexts) = self.0.get_mut(session)
        {
          contexts.retain(|_, context| context.id != id);
        }
      },
      "Page.frameDetached" => {
        if let Some(frame) = params["frameId"].as_str()
          && let Some(contexts) = self.0.get_mut(session)
        {
          contexts.retain(|(f, _), _| f != frame);
        }
      },
      _ => {},
    }
    if self.0.get(session).is_some_and(FxHashMap::is_empty) {
      self.0.remove(session);
    }
  }
}

/// Shared CDP message dispatch state. Embedded by both `PipeTransport` and `WsTransport`.
pub(crate) struct CdpDispatcher {
  /// Set once by the reader task on EOF — see [`Self::is_disconnected`].
  disconnected: std::sync::atomic::AtomicBool,
  pub next_id: AtomicU64,
  pub pending: Arc<PendingMap>,
  /// Per-session lifecycle trackers (keyed by sessionId). Sharded
  /// via `DashMap` so events firing on N sessions don't contend on
  /// the same mutex.
  lifecycle_trackers: Arc<DashMap<String, LifecycleTracker>>,
  iframe_targets: std::sync::Mutex<IframeTargets>,
  context_snapshots: std::sync::Mutex<ContextSnapshots>,
  /// Per-message broadcast channel. Wraps the message in `Arc` so
  /// fanout to N subscribers is N refcount bumps (~5ns each)
  /// instead of N deep `serde_json::Value` clones (~400ns + ~10
  /// allocs each). At 200 events/s × ~12 subscribers per page this
  /// is the single biggest hot-loop CPU win in transport.
  event_tx: arc_swap::ArcSwapOption<broadcast::Sender<Arc<serde_json::Value>>>,
  /// Routed event channels keyed by exact CDP method, for listeners
  /// that should not wake up for unrelated traffic.
  method_event_txs: Arc<DashMap<&'static str, broadcast::Sender<Arc<serde_json::Value>>>>,
  /// Routed event channels keyed by CDP domain (`Network`, `Fetch`,
  /// `Runtime`, ...), for listeners that legitimately consume many
  /// methods in one domain but should not receive the rest of CDP.
  domain_event_txs: Arc<DashMap<&'static str, broadcast::Sender<Arc<serde_json::Value>>>>,
  /// Lossless per-consumer taps keyed by exact CDP method. Fed
  /// synchronously by the reader task in wire order; the unbounded
  /// senders never drop, so tap consumers are structurally immune to
  /// the `Lagged` loss that broadcast subscribers tolerate.
  method_taps: Arc<DashMap<&'static str, Vec<EventTap>>>,
  /// Lossless per-consumer taps keyed by CDP domain.
  domain_taps: Arc<DashMap<&'static str, Vec<EventTap>>>,
  /// Lossless wildcard taps — every event of one exact session, in wire
  /// order. Backs the public `CDPSession` event stream.
  wildcard_taps: Arc<std::sync::Mutex<Vec<EventTap>>>,
  /// Per-method RTT statistics. Only populated when
  /// `FERRIDRIVER_RTT_STATS=1` is set; otherwise the entry insert /
  /// remove path skips the bookkeeping for zero-cost-when-unused.
  rtt_stats: Arc<std::sync::Mutex<RttStats>>,
  /// Per-session dirty flags tracked for fast context recycling.
  session_dirty: std::sync::Mutex<FxHashMap<String, SessionDirty>>,
  /// Browser contexts that received `Browser.grantPermissions`.
  permissions_dirty_contexts: std::sync::Mutex<rustc_hash::FxHashSet<String>>,
  /// Set if `Browser.grantPermissions` was called without an explicit context id.
  permissions_dirty_global: std::sync::atomic::AtomicBool,
  /// Browser contexts that received browser-scoped `Storage.setCookies`.
  cookies_dirty_contexts: std::sync::Mutex<rustc_hash::FxHashSet<String>>,
  /// Set if `Storage.setCookies` was called without an explicit context id.
  cookies_dirty_global: std::sync::atomic::AtomicBool,
}

/// Lock a `std::sync::Mutex`, recovering from poisoning.
///
/// `std::sync::Mutex` only fails when a thread panicked while holding the lock.
/// In the CDP dispatcher this is non-fatal -- we recover the inner data and continue.
fn lock_or_recover<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
  m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// One registered lossless tap: an unbounded sender plus the session it
/// is scoped to (`None` = every session).
pub(crate) struct EventTap {
  session: Option<String>,
  tx: tokio::sync::mpsc::UnboundedSender<Arc<serde_json::Value>>,
  /// Strict taps match the event's `sessionId` exactly — browser-scoped
  /// events (empty sid) do NOT pass a page-session filter. Used by the
  /// public `CDPSession` wildcard stream, where an attached session must
  /// only observe its own events (Playwright `CRSession` semantics).
  strict: bool,
}

impl EventTap {
  /// Whether an event carrying `sid` (empty when the event is
  /// browser-scoped) belongs to this tap. For non-strict taps,
  /// browser-scoped events pass every session filter — per-page
  /// consumers of browser-level domains (`Browser.download*`) decide
  /// relevance themselves.
  fn matches_session(&self, sid: &str) -> bool {
    if self.strict {
      return self.session.as_deref() == Some(sid);
    }
    self.session.as_deref().is_none_or(|s| sid.is_empty() || sid == s)
  }
}

/// Deliver `msg` to every matching tap in `taps`, pruning taps whose
/// receiver was dropped.
fn send_to_taps(taps: &mut Vec<EventTap>, sid: &str, msg: &Arc<serde_json::Value>) {
  taps.retain(|tap| {
    if !tap.matches_session(sid) {
      return true;
    }
    tap.tx.send(Arc::clone(msg)).is_ok()
  });
}

/// Broadcast capacity for the per-transport event channel.
///
/// Every event subscriber (frame-cache listener, console drain,
/// network tracker, file-chooser listener, screencast tap, NAPI
/// `page.on(...)` registrations, ...) shares this single fan-out
/// queue. A slow subscriber that lags behind the producer makes
/// `tokio::sync::broadcast` drop the oldest queued message and
/// surface `RecvError::Lagged` to that subscriber. The frame
/// listener cannot recover from a dropped `Page.frameNavigated` —
/// the page's frame cache stays stale, and every subsequent
/// `locator(...)` waits for an element on the wrong frame.
///
/// 4096 is large enough to absorb a worst-case page load
/// (network requests + lifecycle + DOM events) for multiple
/// concurrent subscribers without dropping events. The memory
/// cost is bounded by `Arc<serde_json::Value>` * capacity per
/// transport, i.e. <1MB even at full saturation.
const EVENT_BROADCAST_CAPACITY: usize = 4096;

#[derive(Default)]
struct IframeTargets {
  attached: Vec<Arc<serde_json::Value>>,
  taps: Vec<tokio::sync::mpsc::UnboundedSender<IframeTargetEvent>>,
}

pub enum IframeTargetEvent {
  Target(Arc<serde_json::Value>),
  Checkpoint(tokio::sync::oneshot::Sender<()>),
  Closed,
}

pub struct IframeTargetSubscription {
  pub events: tokio::sync::mpsc::UnboundedReceiver<IframeTargetEvent>,
  pub checkpoints: tokio::sync::mpsc::UnboundedSender<IframeTargetEvent>,
}

impl IframeTargets {
  fn dispatch(&mut self, event: &Arc<serde_json::Value>) {
    let session = &event["params"]["sessionId"];
    if event["method"] == "Target.attachedToTarget" {
      if event["params"]["targetInfo"]["type"] != "iframe" {
        return;
      }
      self.attached.retain(|old| old["params"]["sessionId"] != *session);
      self.attached.push(Arc::clone(event));
    } else if let Some(session) = session.as_str() {
      self.forget_session(session);
    }
    self
      .taps
      .retain(|tap| tap.send(IframeTargetEvent::Target(Arc::clone(event))).is_ok());
  }

  fn forget_session(&mut self, session: &str) {
    let mut removed = vec![session.to_owned()];
    while let Some(session) = removed.pop() {
      self.attached.retain(|event| {
        if event["sessionId"] == session {
          if let Some(child) = event["params"]["sessionId"].as_str() {
            removed.push(child.to_owned());
          }
          false
        } else {
          event["params"]["sessionId"] != session
        }
      });
    }
  }
}
