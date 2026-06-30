//! Event system for Page, Frame, and `BrowserContext`.
//!
//! Playwright-compatible event emitter using tokio broadcast channels.
//! Supports `on()`, `once()`, `waitForEvent()` patterns.
//!
//! Events flow from backend (CDP/WebKit) -> `EventEmitter` -> subscribers.
//! The existing log accumulation (`console_log`, `network_log`, `dialog_log`)
//! continues working alongside the new event system.

use crate::backend::FrameInfo;
use crate::console_message::ConsoleMessage;
use crate::dialog::Dialog;
use crate::download::Download;
use crate::file_chooser::FileChooser;
use crate::network::{Request, Response, WebSocket};
use crate::web_error::WebError;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::broadcast;

// ── Event Types ──────────────────────────────────────────────────────────────

/// Events emitted by pages. Mirrors Playwright's page event types.
#[derive(Debug, Clone)]
pub enum PageEvent {
  /// Console message from the page (`console.log`, `console.error`, etc.)
  /// Carries a live [`ConsoleMessage`] with `args` / `location` /
  /// `text` / `type` / `timestamp` / `page` accessors; matches
  /// Playwright's `page.on('console', (msg: ConsoleMessage) => ...)`.
  Console(ConsoleMessage),
  /// Network request started — Playwright `page.on('request')`.
  Request(Request),
  /// Network response received — Playwright `page.on('response')`.
  Response(Response),
  /// Network request finished (`loadingFinished`) —
  /// Playwright `page.on('requestfinished')`.
  RequestFinished(Request),
  /// Network request failed (`loadingFailed`) —
  /// Playwright `page.on('requestfailed')`.
  RequestFailed(Request),
  /// WebSocket opened — Playwright `page.on('websocket')`.
  WebSocket(WebSocket),
  /// Dialog appeared (alert, confirm, prompt, beforeunload). Carries
  /// a live [`Dialog`] handle — listeners are expected to call
  /// `dialog.accept(...)` or `dialog.dismiss()` exactly once. If no
  /// listener is registered when the dialog opens, the backend
  /// auto-closes it (accept for `beforeunload`, dismiss otherwise).
  Dialog(Dialog),
  /// File chooser opened on the page — intercepted via CDP
  /// `Page.fileChooserOpened` / `BiDi` `input.fileDialogOpened`.
  /// Carries a live [`FileChooser`] whose `setFiles(...)` uploads via
  /// the captured `<input type=file>`. If no listener is registered,
  /// the backend disposes the underlying element handle (matches
  /// Playwright's `server/page.ts::_onFileChooserOpened` no-listener
  /// branch).
  FileChooser(FileChooser),
  /// Frame attached to the page.
  FrameAttached(FrameInfo),
  /// Frame detached from the page.
  FrameDetached { frame_id: String },
  /// Frame navigated to a new URL.
  FrameNavigated(FrameInfo),
  /// Page load event fired.
  Load,
  /// `DOMContentLoaded` event fired.
  DomContentLoaded,
  /// Page was closed.
  Close,
  /// Uncaught exception or unhandled rejection in page. Carries a live
  /// [`WebError`] with `{ name, message, stack }` matching JS `Error`
  /// shape plus a weak back-reference to the owning page. Observed via
  /// `page.on('pageerror', cb)` (page-scoped) and fanned out to
  /// `context.on('weberror', cb)` by the per-page → per-context
  /// bridge registered in `ContextRef::new_page`.
  PageError(WebError),
  /// Browser-initiated download. Carries a live
  /// [`crate::download::Download`] handle with `path` / `save_as` /
  /// `cancel` / `delete` / `failure` methods; mirrors Playwright's
  /// `page.on('download', (download: Download) => ...)`.
  Download(Download),
}

impl PageEvent {
  /// Project the event into the compact JSON shape the binding layers
  /// (NAPI threadsafe-function listeners, `QuickJS` cross-task dispatch,
  /// `waitForEvent`) hand to user callbacks. Centralised here so every
  /// binding sees one shape — the `pageerror` consumer in NAPI still
  /// wraps the `{ name, message, stack }` object into a native JS
  /// `Error`, but the field set originates here.
  #[must_use]
  pub fn to_snapshot(&self) -> serde_json::Value {
    match self {
      PageEvent::Console(msg) => {
        let loc = msg.location();
        serde_json::json!({
          "type": msg.type_str(),
          "text": msg.text(),
          "location": {
            "url": loc.url,
            "lineNumber": loc.line_number,
            "columnNumber": loc.column_number,
          },
          "timestamp": msg.timestamp(),
          "argsCount": msg.args().len(),
        })
      },
      PageEvent::Response(r) => serde_json::json!({
        "url": r.url(),
        "status": r.status(),
        "statusText": r.status_text(),
        "ok": r.ok(),
        "fromServiceWorker": r.is_from_service_worker(),
        "headers": r.headers(),
      }),
      PageEvent::Request(r) | PageEvent::RequestFinished(r) | PageEvent::RequestFailed(r) => serde_json::json!({
        "url": r.url(),
        "method": r.method(),
        "resourceType": r.resource_type(),
        "isNavigationRequest": r.is_navigation_request(),
        "headers": r.headers(),
        "postData": r.post_data(),
      }),
      PageEvent::WebSocket(ws) => serde_json::json!({ "url": ws.url(), "isClosed": ws.is_closed() }),
      PageEvent::Dialog(d) => serde_json::json!({
        "type": d.dialog_type().as_str(),
        "message": d.message(),
        "defaultValue": d.default_value(),
      }),
      PageEvent::FileChooser(fc) => serde_json::json!({ "isMultiple": fc.is_multiple() }),
      PageEvent::FrameAttached(f) | PageEvent::FrameNavigated(f) => serde_json::to_value(f).unwrap_or_default(),
      PageEvent::FrameDetached { frame_id } => serde_json::json!({ "frameId": frame_id }),
      PageEvent::Download(d) => serde_json::json!({
        "url": d.url(),
        "suggestedFilename": d.suggested_filename(),
      }),
      PageEvent::Load => serde_json::json!({ "type": "load" }),
      PageEvent::DomContentLoaded => serde_json::json!({ "type": "domcontentloaded" }),
      PageEvent::Close => serde_json::json!({ "type": "close" }),
      PageEvent::PageError(err) => {
        let d = err.error();
        serde_json::json!({
          "name": d.name,
          "message": d.message,
          "stack": d.stack,
        })
      },
    }
  }
}

/// Future returned by an [`ExposedFn`] — resolves to the page-visible
/// result of the bound callback.
pub type ExposedFnFuture = Pin<Box<dyn Future<Output = serde_json::Value> + Send>>;

/// Callback type for exposed functions (`page.exposeFunction`).
///
/// Receives the page-side call arguments as a `Vec` (the binding
/// layers SPREAD them into the user JS callback — `window.fn(a, b)` ->
/// `callback(a, b)` — matching Playwright) and returns a future
/// resolving to the serialized JSON result. It is async (not a plain
/// `-> Value`)
/// because the binding layers (NAPI threadsafe-function, `QuickJS`
/// cross-task `async_with`) can only produce the callback's real
/// return value asynchronously — Playwright delivers that value (and
/// awaits a returned Promise) to the page-side caller, so the backend
/// dispatch must `await` this before resolving the page binding.
pub type ExposedFn = Arc<dyn Fn(Vec<serde_json::Value>) -> ExposedFnFuture + Send + Sync>;

/// The `source` object Playwright passes as the first argument to an
/// `exposeBinding` callback. Mirrors
/// `BindingSource = { context, page, frame }` from
/// `/tmp/playwright/packages/playwright-core/types/structs.d.ts:45`.
///
/// ferridriver delivers identity strings rather than live handles
/// because the backend binding dispatch runs outside the
/// `BrowserContext`/`Page` handle lifetime; the binding layers
/// reconstruct the JS-visible `{ context, page, frame }` object from
/// these identifiers. `context` is the composite session key,
/// `page` the page's stable id, `frame` the calling frame id.
#[derive(Debug, Clone, Default)]
pub struct BindingSource {
  /// Composite session key of the context the call originated from.
  pub context: String,
  /// Stable page identifier (target id) of the calling page.
  pub page: String,
  /// Frame id of the calling frame (the main frame when the call
  /// comes from the top document).
  pub frame: String,
}

/// Callback type for context/page bindings registered via
/// `exposeBinding`. Like [`ExposedFn`] but receives a [`BindingSource`]
/// as its first argument, matching Playwright's
/// `(source: BindingSource, ...args) => any`.
///
/// `exposeFunction` is `exposeBinding` minus the source argument: the
/// binding layers wrap a source-less callback by discarding the
/// [`BindingSource`] before invoking it.
pub type ExposedBinding = Arc<dyn Fn(BindingSource, Vec<serde_json::Value>) -> ExposedFnFuture + Send + Sync>;

/// Event listener callback type.
pub type EventCallback = Arc<dyn Fn(PageEvent) + Send + Sync>;

/// Handle for removing an event listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ListenerId(pub u64);

// `DialogHandler` / `DialogAction` / `default_dialog_handler` /
// `PendingDialog` were removed — dialogs are now observed via
// `page.on('dialog', ...)` with live [`crate::dialog::Dialog`]
// handles. See that module for the new API.

/// Check if an event matches a named event type.
/// Drain a pre-acquired broadcast receiver until `predicate` matches an
/// event, the channel closes, or `timeout_ms` elapses.
///
/// Pairs with [`EventEmitter::subscribe`]: callers that need a
/// synchronous subscription point (so a downstream `.await` in JS
/// can't race the listener registration) call `subscribe()` first
/// and then `drain_until` inside the spawned future. Playwright
/// implements `helper.waitForEvent` with the same shape — listener
/// registered before the Promise is returned to JS, see
/// `/tmp/playwright/packages/playwright-core/src/server/helper.ts:58`.
///
/// # Errors
///
/// Returns an error if the timeout elapses or the event channel is closed.
pub async fn drain_until<F>(
  rx: &mut tokio::sync::broadcast::Receiver<PageEvent>,
  predicate: F,
  timeout_ms: u64,
) -> crate::error::Result<PageEvent>
where
  F: Fn(&PageEvent) -> bool,
{
  let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
  loop {
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero() {
      return Err(crate::error::FerriError::timeout("waiting for event", timeout_ms));
    }
    match tokio::time::timeout(remaining, rx.recv()).await {
      Ok(Ok(event)) if predicate(&event) => return Ok(event),
      Ok(Ok(_)) => {},
      Ok(Err(_)) => {
        return Err(crate::error::FerriError::target_closed(Some(
          "event channel closed".into(),
        )));
      },
      Err(_) => return Err(crate::error::FerriError::timeout("waiting for event", timeout_ms)),
    }
  }
}

/// Whether `event` matches `name` under Playwright's lowercase
/// page-event vocabulary (`'request'`, `'response'`, `'pageerror'`, …).
/// Exposed so NAPI / `QuickJS` bindings that need a synchronous
/// listener pre-arm (see [`drain_until`]) can pass the predicate in
/// without re-implementing the match.
#[must_use]
pub fn event_name_matches(name: &str, event: &PageEvent) -> bool {
  matches!(
    (name, event),
    ("console", PageEvent::Console(_))
      | ("request", PageEvent::Request(_))
      | ("response", PageEvent::Response(_))
      | ("requestfinished", PageEvent::RequestFinished(_))
      | ("requestfailed", PageEvent::RequestFailed(_))
      | ("websocket", PageEvent::WebSocket(_))
      | ("dialog", PageEvent::Dialog(_))
      | ("filechooser", PageEvent::FileChooser(_))
      | ("frameattached", PageEvent::FrameAttached(_))
      | ("framedetached", PageEvent::FrameDetached { .. })
      | ("framenavigated", PageEvent::FrameNavigated(_))
      | ("load", PageEvent::Load)
      | ("domcontentloaded", PageEvent::DomContentLoaded)
      | ("close", PageEvent::Close)
      | ("pageerror", PageEvent::PageError(_))
      | ("download", PageEvent::Download(_))
  )
}

/// Receive the next value from a broadcast receiver, surviving
/// `Lagged`. A lapped receiver loses the dropped events but keeps the
/// subscription alive — exiting the listener loop instead (the old
/// `while let Ok(..)` shape) silently disabled the listener for the
/// rest of the page's life after one event storm. Returns `None` once
/// the channel closes.
pub async fn recv_tolerant<T: Clone>(rx: &mut broadcast::Receiver<T>) -> Option<T> {
  loop {
    match rx.recv().await {
      Ok(v) => return Some(v),
      Err(broadcast::error::RecvError::Lagged(n)) => {
        tracing::warn!(dropped = n, "broadcast listener lagged; dropped {n} event(s)");
      },
      Err(broadcast::error::RecvError::Closed) => return None,
    }
  }
}

// ── Event Emitter ────────────────────────────────────────────────────────────

/// Broadcast-based event emitter. Cheap to clone (Arc'd internally).
#[derive(Clone)]
pub struct EventEmitter {
  tx: broadcast::Sender<PageEvent>,
  /// Active listeners with their abort handles and the event name
  /// each one filters for. The event name is retained so
  /// [`Self::has_listener`] can answer per-name — dialog and
  /// filechooser dispatch depend on knowing whether a listener
  /// exists for the specific event, not just whether any listener
  /// is attached.
  listeners: Arc<std::sync::Mutex<rustc_hash::FxHashMap<u64, ListenerEntry>>>,
  next_listener_id: Arc<std::sync::atomic::AtomicU64>,
  /// Stored runtime handle so `on()` works from non-async contexts (NAPI).
  runtime_handle: Arc<std::sync::Mutex<Option<tokio::runtime::Handle>>>,
}

/// Per-registered-listener bookkeeping.
struct ListenerEntry {
  abort: tokio::task::AbortHandle,
  event_name: String,
}

impl EventEmitter {
  #[must_use]
  pub fn new() -> Self {
    let (tx, _) = broadcast::channel(512);
    // Try to capture the current runtime handle at creation time.
    let handle = tokio::runtime::Handle::try_current().ok();
    Self {
      tx,
      listeners: Arc::new(std::sync::Mutex::new(rustc_hash::FxHashMap::default())),
      next_listener_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
      runtime_handle: Arc::new(std::sync::Mutex::new(handle)),
    }
  }

  /// Whether any named listener registered via [`Self::on`] or
  /// [`Self::once`] is filtering for `event_name`. Used by backend
  /// dialog / filechooser listeners purely as an optimisation to
  /// short-circuit the grace-period wait when there is demonstrably
  /// nobody attached; auto-close behaviour is primarily driven by
  /// the `Dialog::is_handled` check after the grace period in the
  /// backend listener, which also covers the
  /// `page.waitForEvent('dialog')` path (raw broadcast subscribers
  /// don't increment the named count but do receive the emitted
  /// event within the grace window).
  #[must_use]
  pub fn has_listener(&self, event_name: &str) -> bool {
    let Ok(listeners) = self.listeners.lock() else {
      return false;
    };
    listeners.values().any(|entry| entry.event_name == event_name)
  }

  /// Spawn a task on the captured runtime (works from non-async contexts).
  fn spawn_listener(&self, future: impl std::future::Future<Output = ()> + Send + 'static) -> tokio::task::AbortHandle {
    // Try current runtime first, then fall back to stored handle
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
      return handle.spawn(future).abort_handle();
    }
    if let Ok(guard) = self.runtime_handle.lock() {
      if let Some(handle) = guard.as_ref() {
        return handle.spawn(future).abort_handle();
      }
    }
    // Last resort: spawn a thread with its own runtime
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
      let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
        return;
      };
      let handle = rt.spawn(future);
      let _ = tx.send(handle.abort_handle());
      rt.block_on(handle).ok();
    });
    rx.recv().unwrap_or_else(|_| {
      // Fallback: return a no-op abort handle
      tokio::runtime::Handle::current().spawn(async {}).abort_handle()
    })
  }

  /// Emit an event to all current subscribers.
  pub fn emit(&self, event: PageEvent) {
    let _ = self.tx.send(event);
  }

  /// Number of active broadcast subscribers (zero = nobody is listening).
  #[must_use]
  pub fn receiver_count(&self) -> usize {
    self.tx.receiver_count()
  }

  /// Subscribe to events. Returns a receiver that gets all future events.
  #[must_use]
  pub fn subscribe(&self) -> broadcast::Receiver<PageEvent> {
    self.tx.subscribe()
  }

  /// Wait for an event matching a predicate, with timeout.
  ///
  /// # Errors
  ///
  /// Returns an error if the timeout elapses or the event channel is closed.
  ///
  /// ```ignore
  /// let response = emitter.wait_for(|e| matches!(e, PageEvent::Response(r) if r.url.contains("/api")), 5000).await?;
  /// ```
  ///
  /// The subscription to the broadcast channel happens when this
  /// future is first polled, which on `async fn` boundaries can be
  /// AFTER the caller's next line — long enough for the triggering
  /// event to fire and be missed. Callers that need a synchronous
  /// subscription point (NAPI's `Promise` construction, BDD step
  /// pre-arming) should call [`Self::subscribe`] first and then
  /// [`drain_until`] with the pre-acquired receiver.
  pub async fn wait_for<F>(&self, predicate: F, timeout_ms: u64) -> crate::error::Result<PageEvent>
  where
    F: Fn(&PageEvent) -> bool,
  {
    let mut rx = self.tx.subscribe();
    drain_until(&mut rx, predicate, timeout_ms).await
  }

  /// Wait for the next event of a specific type, with timeout.
  ///
  /// # Errors
  ///
  /// Returns an error if the timeout elapses or the event channel is closed.
  pub async fn wait_for_event(&self, event_name: &str, timeout_ms: u64) -> crate::error::Result<PageEvent> {
    let name = event_name.to_string();
    self.wait_for(move |e| event_name_matches(&name, e), timeout_ms).await
  }

  /// Subscribe to events matching a name. Calls the callback for each matching event.
  /// Returns a `ListenerId` for later removal with `off()`.
  ///
  /// # Panics
  ///
  /// Panics if the internal listeners mutex is poisoned.
  pub fn on(&self, event_name: &str, callback: EventCallback) -> ListenerId {
    let id = self.next_listener_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut rx = self.tx.subscribe();
    let name = event_name.to_string();
    let filter_name = name.clone();

    let abort_handle = self.spawn_listener(async move {
      while let Some(event) = crate::events::recv_tolerant(&mut rx).await {
        if event_name_matches(&filter_name, &event) {
          callback(event);
        }
      }
    });

    if let Ok(mut guard) = self.listeners.lock() {
      guard.insert(
        id,
        ListenerEntry {
          abort: abort_handle,
          event_name: name,
        },
      );
    }
    ListenerId(id)
  }

  /// Subscribe to a single event, then auto-remove the listener.
  pub fn once(&self, event_name: &str, callback: EventCallback) -> ListenerId {
    let listeners = self.listeners.clone();
    let id = self.next_listener_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut rx = self.tx.subscribe();
    let name = event_name.to_string();
    let filter_name = name.clone();

    let abort_handle = self.spawn_listener(async move {
      while let Some(event) = crate::events::recv_tolerant(&mut rx).await {
        if event_name_matches(&filter_name, &event) {
          callback(event);
          if let Ok(mut guard) = listeners.lock() {
            guard.remove(&id);
          }
          break;
        }
      }
    });

    if let Ok(mut guard) = self.listeners.lock() {
      guard.insert(
        id,
        ListenerEntry {
          abort: abort_handle,
          event_name: name,
        },
      );
    }
    ListenerId(id)
  }

  /// Remove an event listener by ID.
  pub fn off(&self, id: ListenerId) {
    if let Ok(mut guard) = self.listeners.lock() {
      if let Some(entry) = guard.remove(&id.0) {
        entry.abort.abort();
      }
    }
  }

  /// Remove all event listeners.
  pub fn remove_all_listeners(&self) {
    if let Ok(mut listeners) = self.listeners.lock() {
      for (_, entry) in listeners.drain() {
        entry.abort.abort();
      }
    }
  }

  /// Remove every listener registered for `event_name`, leaving other
  /// events' listeners attached (Playwright's
  /// `removeAllListeners(type)` with a type argument).
  pub fn remove_listeners_named(&self, event_name: &str) {
    if let Ok(mut listeners) = self.listeners.lock() {
      listeners.retain(|_, entry| {
        if entry.event_name == event_name {
          entry.abort.abort();
          false
        } else {
          true
        }
      });
    }
  }
}

impl Default for EventEmitter {
  fn default() -> Self {
    Self::new()
  }
}

// ── Context-level event system ─────────────────────────────────────────

/// Events emitted by browser contexts. Mirrors the subset of
/// Playwright's `BrowserContextEventMap` that ferridriver supports:
/// `'weberror'` plus the page-lifecycle mirror events added in
/// Playwright 1.60 (`'download'`, `'frameattached'`, `'framedetached'`,
/// `'framenavigated'`, `'pageclose'`, `'pageload'`).
#[derive(Debug, Clone)]
pub enum ContextEvent {
  /// Unhandled error / rejection in any page in this context. Mirrors
  /// Playwright's `browserContext.on('weberror', (webError: WebError) => ...)`
  /// from `server/browserContext.ts:54`.
  WebError(crate::web_error::WebError),
  /// Browser-initiated download on any page in this context. Mirrors
  /// `browserContext.on('download', (download: Download) => ...)`.
  Download(Download),
  /// A frame attached on a page in this context. Carries the owning
  /// page so the binding can mint a live `Frame` for `frame_id`.
  /// Mirrors `browserContext.on('frameattached', (frame: Frame) => ...)`.
  FrameAttached {
    page: Arc<crate::page::Page>,
    frame_id: String,
  },
  /// A frame detached. `browserContext.on('framedetached', ...)`.
  FrameDetached {
    page: Arc<crate::page::Page>,
    frame_id: String,
  },
  /// A frame navigated. `browserContext.on('framenavigated', ...)`.
  FrameNavigated {
    page: Arc<crate::page::Page>,
    frame_id: String,
  },
  /// A page in this context was closed.
  /// `browserContext.on('pageclose', (page: Page) => ...)`.
  PageClose(Arc<crate::page::Page>),
  /// A page in this context fired `load`.
  /// `browserContext.on('pageload', (page: Page) => ...)`.
  PageLoad(Arc<crate::page::Page>),
}

/// Callback type for context-level event listeners.
pub type ContextEventCallback = Arc<dyn Fn(ContextEvent) + Send + Sync>;

fn context_event_name_matches(name: &str, event: &ContextEvent) -> bool {
  matches!(
    (name, event),
    ("weberror", ContextEvent::WebError(_))
      | ("download", ContextEvent::Download(_))
      | ("frameattached", ContextEvent::FrameAttached { .. })
      | ("framedetached", ContextEvent::FrameDetached { .. })
      | ("framenavigated", ContextEvent::FrameNavigated { .. })
      | ("pageclose", ContextEvent::PageClose(_))
      | ("pageload", ContextEvent::PageLoad(_))
  )
}

/// Broadcast-based context-event emitter. Mirrors [`EventEmitter`] but
/// for `ContextEvent`. Cheap to clone (Arc'd internally). Registered
/// listeners run on the captured tokio runtime handle so `on()` works
/// from non-async contexts (NAPI).
#[derive(Clone)]
pub struct ContextEventEmitter {
  tx: broadcast::Sender<ContextEvent>,
  listeners: Arc<std::sync::Mutex<rustc_hash::FxHashMap<u64, ContextListenerEntry>>>,
  next_listener_id: Arc<std::sync::atomic::AtomicU64>,
  runtime_handle: Arc<std::sync::Mutex<Option<tokio::runtime::Handle>>>,
}

struct ContextListenerEntry {
  abort: tokio::task::AbortHandle,
}

impl ContextEventEmitter {
  #[must_use]
  pub fn new() -> Self {
    let (tx, _) = broadcast::channel(512);
    let handle = tokio::runtime::Handle::try_current().ok();
    Self {
      tx,
      listeners: Arc::new(std::sync::Mutex::new(rustc_hash::FxHashMap::default())),
      next_listener_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
      runtime_handle: Arc::new(std::sync::Mutex::new(handle)),
    }
  }

  fn spawn_listener(&self, future: impl std::future::Future<Output = ()> + Send + 'static) -> tokio::task::AbortHandle {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
      return handle.spawn(future).abort_handle();
    }
    if let Ok(guard) = self.runtime_handle.lock() {
      if let Some(handle) = guard.as_ref() {
        return handle.spawn(future).abort_handle();
      }
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
      let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
        return;
      };
      let handle = rt.spawn(future);
      let _ = tx.send(handle.abort_handle());
      rt.block_on(handle).ok();
    });
    rx.recv()
      .unwrap_or_else(|_| tokio::runtime::Handle::current().spawn(async {}).abort_handle())
  }

  /// Emit a context event to all current subscribers.
  pub fn emit(&self, event: ContextEvent) {
    let _ = self.tx.send(event);
  }

  /// Subscribe to raw broadcast events (for `waitForEvent` callers).
  #[must_use]
  pub fn subscribe(&self) -> broadcast::Receiver<ContextEvent> {
    self.tx.subscribe()
  }

  /// Wait for the next event matching `event_name`, with timeout.
  ///
  /// # Errors
  ///
  /// Returns an error if the timeout elapses or the channel is closed.
  pub async fn wait_for_event(&self, event_name: &str, timeout_ms: u64) -> crate::error::Result<ContextEvent> {
    let mut rx = self.tx.subscribe();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let name = event_name.to_string();
    loop {
      let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
      if remaining.is_zero() {
        return Err(crate::error::FerriError::timeout(
          "waiting for context event",
          timeout_ms,
        ));
      }
      match tokio::time::timeout(remaining, rx.recv()).await {
        Ok(Ok(event)) if context_event_name_matches(&name, &event) => return Ok(event),
        Ok(Ok(_)) => {},
        Ok(Err(_)) => {
          return Err(crate::error::FerriError::target_closed(Some(
            "context event channel closed".into(),
          )));
        },
        Err(_) => {
          return Err(crate::error::FerriError::timeout(
            "waiting for context event",
            timeout_ms,
          ));
        },
      }
    }
  }

  /// Register a context-level event listener. Returns a
  /// [`ListenerId`] for later removal with [`Self::off`].
  pub fn on(&self, event_name: &str, callback: ContextEventCallback) -> ListenerId {
    let id = self.next_listener_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut rx = self.tx.subscribe();
    let filter_name = event_name.to_string();
    let abort_handle = self.spawn_listener(async move {
      while let Some(event) = crate::events::recv_tolerant(&mut rx).await {
        if context_event_name_matches(&filter_name, &event) {
          callback(event);
        }
      }
    });
    if let Ok(mut guard) = self.listeners.lock() {
      guard.insert(id, ContextListenerEntry { abort: abort_handle });
    }
    ListenerId(id)
  }

  /// Register a single-shot context-level event listener.
  pub fn once(&self, event_name: &str, callback: ContextEventCallback) -> ListenerId {
    let listeners = self.listeners.clone();
    let id = self.next_listener_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut rx = self.tx.subscribe();
    let filter_name = event_name.to_string();
    let abort_handle = self.spawn_listener(async move {
      while let Some(event) = crate::events::recv_tolerant(&mut rx).await {
        if context_event_name_matches(&filter_name, &event) {
          callback(event);
          if let Ok(mut guard) = listeners.lock() {
            guard.remove(&id);
          }
          break;
        }
      }
    });
    if let Ok(mut guard) = self.listeners.lock() {
      guard.insert(id, ContextListenerEntry { abort: abort_handle });
    }
    ListenerId(id)
  }

  /// Remove a context-level listener by id.
  pub fn off(&self, id: ListenerId) {
    if let Ok(mut guard) = self.listeners.lock() {
      if let Some(entry) = guard.remove(&id.0) {
        entry.abort.abort();
      }
    }
  }

  /// Drop every registered listener.
  pub fn remove_all_listeners(&self) {
    if let Ok(mut listeners) = self.listeners.lock() {
      for (_, entry) in listeners.drain() {
        entry.abort.abort();
      }
    }
  }
}

impl Default for ContextEventEmitter {
  fn default() -> Self {
    Self::new()
  }
}

// ── Browser-level event system ─────────────────────────────────────────

/// Events emitted by a `Browser`. Mirrors the subset of Playwright's
/// `BrowserEventMap` that ferridriver supports — `'context'`, fired when
/// a new browser context is created (`browser.on('context', ...)`,
/// added in Playwright 1.60).
#[derive(Clone)]
pub enum BrowserEvent {
  /// A new browser context was created on this browser. Carries the
  /// live [`crate::context::ContextRef`]. Mirrors
  /// `browser.on('context', (context: BrowserContext) => ...)`.
  Context(crate::context::ContextRef),
}

/// Callback type for browser-level event listeners.
pub type BrowserEventCallback = Arc<dyn Fn(BrowserEvent) + Send + Sync>;

fn browser_event_name_matches(name: &str, event: &BrowserEvent) -> bool {
  matches!((name, event), ("context", BrowserEvent::Context(_)))
}

/// Broadcast-based browser-event emitter. Mirrors [`ContextEventEmitter`]
/// but for [`BrowserEvent`]. Cheap to clone (Arc'd internally).
#[derive(Clone)]
pub struct BrowserEventEmitter {
  tx: broadcast::Sender<BrowserEvent>,
  listeners: Arc<std::sync::Mutex<rustc_hash::FxHashMap<u64, ContextListenerEntry>>>,
  next_listener_id: Arc<std::sync::atomic::AtomicU64>,
  runtime_handle: Arc<std::sync::Mutex<Option<tokio::runtime::Handle>>>,
}

impl BrowserEventEmitter {
  #[must_use]
  pub fn new() -> Self {
    let (tx, _) = broadcast::channel(256);
    let handle = tokio::runtime::Handle::try_current().ok();
    Self {
      tx,
      listeners: Arc::new(std::sync::Mutex::new(rustc_hash::FxHashMap::default())),
      next_listener_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
      runtime_handle: Arc::new(std::sync::Mutex::new(handle)),
    }
  }

  fn spawn_listener(&self, future: impl std::future::Future<Output = ()> + Send + 'static) -> tokio::task::AbortHandle {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
      return handle.spawn(future).abort_handle();
    }
    if let Ok(guard) = self.runtime_handle.lock() {
      if let Some(handle) = guard.as_ref() {
        return handle.spawn(future).abort_handle();
      }
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
      let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
        return;
      };
      let handle = rt.spawn(future);
      let _ = tx.send(handle.abort_handle());
      rt.block_on(handle).ok();
    });
    rx.recv()
      .unwrap_or_else(|_| tokio::runtime::Handle::current().spawn(async {}).abort_handle())
  }

  /// Emit a browser event to all current subscribers.
  pub fn emit(&self, event: BrowserEvent) {
    let _ = self.tx.send(event);
  }

  /// Wait for the next event matching `event_name`, with timeout.
  ///
  /// # Errors
  ///
  /// Returns an error if the timeout elapses or the channel is closed.
  pub async fn wait_for_event(&self, event_name: &str, timeout_ms: u64) -> crate::error::Result<BrowserEvent> {
    let mut rx = self.tx.subscribe();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let name = event_name.to_string();
    loop {
      let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
      if remaining.is_zero() {
        return Err(crate::error::FerriError::timeout(
          "waiting for browser event",
          timeout_ms,
        ));
      }
      match tokio::time::timeout(remaining, rx.recv()).await {
        Ok(Ok(event)) if browser_event_name_matches(&name, &event) => return Ok(event),
        Ok(Ok(_)) => {},
        Ok(Err(_)) => {
          return Err(crate::error::FerriError::target_closed(Some(
            "browser event channel closed".into(),
          )));
        },
        Err(_) => {
          return Err(crate::error::FerriError::timeout(
            "waiting for browser event",
            timeout_ms,
          ));
        },
      }
    }
  }

  /// Register a browser-level event listener. Returns a [`ListenerId`].
  pub fn on(&self, event_name: &str, callback: BrowserEventCallback) -> ListenerId {
    let id = self.next_listener_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut rx = self.tx.subscribe();
    let filter_name = event_name.to_string();
    let abort_handle = self.spawn_listener(async move {
      while let Some(event) = crate::events::recv_tolerant(&mut rx).await {
        if browser_event_name_matches(&filter_name, &event) {
          callback(event);
        }
      }
    });
    if let Ok(mut guard) = self.listeners.lock() {
      guard.insert(id, ContextListenerEntry { abort: abort_handle });
    }
    ListenerId(id)
  }

  /// Register a single-shot browser-level event listener.
  pub fn once(&self, event_name: &str, callback: BrowserEventCallback) -> ListenerId {
    let listeners = self.listeners.clone();
    let id = self.next_listener_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut rx = self.tx.subscribe();
    let filter_name = event_name.to_string();
    let abort_handle = self.spawn_listener(async move {
      while let Some(event) = crate::events::recv_tolerant(&mut rx).await {
        if browser_event_name_matches(&filter_name, &event) {
          callback(event);
          if let Ok(mut guard) = listeners.lock() {
            guard.remove(&id);
          }
          break;
        }
      }
    });
    if let Ok(mut guard) = self.listeners.lock() {
      guard.insert(id, ContextListenerEntry { abort: abort_handle });
    }
    ListenerId(id)
  }

  /// Remove a browser-level listener by id.
  pub fn off(&self, id: ListenerId) {
    if let Ok(mut guard) = self.listeners.lock() {
      if let Some(entry) = guard.remove(&id.0) {
        entry.abort.abort();
      }
    }
  }
}

impl Default for BrowserEventEmitter {
  fn default() -> Self {
    Self::new()
  }
}
