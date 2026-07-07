//! Event system for Page, Frame, and `BrowserContext`.
//!
//! Playwright-compatible event emitter. Supports `on()`, `once()`,
//! `waitForEvent()` patterns.
//!
//! Events flow from backend (CDP/BiDi/WebKit) -> [`Emitter`] -> listeners.
//! Delivery is lossless and ordered: `emit()` pushes onto an unbounded
//! queue consumed by a single dispatcher task per emitter, which invokes
//! matching listeners in registration order (Playwright's semantics) and
//! forwards to raw subscriptions. Listener callbacks must not block — they
//! run on the dispatcher task; anything slow belongs in a spawned task.

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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tokio::sync::{broadcast, mpsc};

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

/// Whether `event` matches `name` under Playwright's lowercase
/// page-event vocabulary (`'request'`, `'response'`, `'pageerror'`, …).
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
///
/// Used by backend transport consumers (CDP / `BiDi` / `WebKit`
/// protocol broadcast channels); the user-facing emitters in this
/// module are lossless and never lag.
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

// ── Emitter core ─────────────────────────────────────────────────────────────

/// Implemented by every event type an [`Emitter`] can dispatch. Maps
/// Playwright's lowercase event-name vocabulary onto the enum variants.
pub trait EmitterEvent: Clone + Send + 'static {
  /// Whether `event` is the variant named by `name`.
  fn matches_name(name: &str, event: &Self) -> bool;
}

impl EmitterEvent for PageEvent {
  fn matches_name(name: &str, event: &Self) -> bool {
    event_name_matches(name, event)
  }
}

struct ListenerSlot<E> {
  id: u64,
  name: String,
  once: bool,
  callback: Arc<dyn Fn(E) + Send + Sync>,
}

struct SubscriberSlot<E> {
  tx: mpsc::UnboundedSender<E>,
}

/// Listener + subscription registries shared with the dispatcher task.
struct EmitterShared<E> {
  listeners: std::sync::Mutex<Vec<ListenerSlot<E>>>,
  subscribers: std::sync::Mutex<Vec<SubscriberSlot<E>>>,
}

struct EmitterInner<E> {
  queue_tx: mpsc::UnboundedSender<E>,
  shared: Arc<EmitterShared<E>>,
  /// `Some(rx)` until the dispatcher task is spawned; taken exactly once.
  pending_rx: std::sync::Mutex<Option<mpsc::UnboundedReceiver<E>>>,
  dispatcher_running: AtomicBool,
  /// Runtime handle captured at construction so listener registration
  /// works from non-async contexts (NAPI).
  runtime_handle: std::sync::Mutex<Option<tokio::runtime::Handle>>,
  next_id: AtomicU64,
}

/// Lock a `std::sync::Mutex`, recovering from poisoning. Poisoning only
/// happens when a panicking thread held the lock; the registries stay
/// structurally valid, so continuing beats propagating the panic.
fn lock_or_recover<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
  m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

async fn dispatch_loop<E: EmitterEvent>(shared: Arc<EmitterShared<E>>, mut rx: mpsc::UnboundedReceiver<E>) {
  let mut fired: Vec<Arc<dyn Fn(E) + Send + Sync>> = Vec::new();
  while let Some(event) = rx.recv().await {
    {
      let mut listeners = lock_or_recover(&shared.listeners);
      // Collect matching callbacks in registration order and drop
      // `once` slots before invoking, so a recursive emit from inside
      // a callback can never re-fire them.
      listeners.retain(|slot| {
        if E::matches_name(&slot.name, &event) {
          fired.push(Arc::clone(&slot.callback));
          !slot.once
        } else {
          true
        }
      });
    }
    for cb in fired.drain(..) {
      cb(event.clone());
    }
    let mut subscribers = lock_or_recover(&shared.subscribers);
    subscribers.retain(|slot| slot.tx.send(event.clone()).is_ok());
  }
}

/// Lossless, ordered event emitter. Cheap to clone (Arc'd internally).
///
/// One dispatcher task per emitter consumes an unbounded queue and
/// invokes matching listeners in registration order, then forwards to
/// raw [`EventSubscription`]s. Nothing is ever dropped; a subscriber
/// that stops draining accumulates queued events until it is dropped.
pub struct Emitter<E: EmitterEvent> {
  inner: Arc<EmitterInner<E>>,
}

impl<E: EmitterEvent> Clone for Emitter<E> {
  fn clone(&self) -> Self {
    Self {
      inner: Arc::clone(&self.inner),
    }
  }
}

impl<E: EmitterEvent> Default for Emitter<E> {
  fn default() -> Self {
    Self::new()
  }
}

impl<E: EmitterEvent> Emitter<E> {
  #[must_use]
  pub fn new() -> Self {
    let (queue_tx, queue_rx) = mpsc::unbounded_channel();
    let inner = Arc::new(EmitterInner {
      queue_tx,
      shared: Arc::new(EmitterShared {
        listeners: std::sync::Mutex::new(Vec::new()),
        subscribers: std::sync::Mutex::new(Vec::new()),
      }),
      pending_rx: std::sync::Mutex::new(Some(queue_rx)),
      dispatcher_running: AtomicBool::new(false),
      runtime_handle: std::sync::Mutex::new(tokio::runtime::Handle::try_current().ok()),
      next_id: AtomicU64::new(1),
    });
    let emitter = Self { inner };
    emitter.ensure_dispatcher();
    emitter
  }

  /// Spawn the dispatcher task if it isn't running yet. Queued events
  /// buffer in the channel until a runtime is available, so nothing is
  /// lost when the emitter is constructed outside a runtime.
  fn ensure_dispatcher(&self) {
    if self.inner.dispatcher_running.load(Ordering::Acquire) {
      return;
    }
    let mut pending = lock_or_recover(&self.inner.pending_rx);
    let Some(rx) = pending.take() else {
      return;
    };
    let shared = Arc::clone(&self.inner.shared);
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
      handle.spawn(dispatch_loop(shared, rx));
    } else {
      let stored = lock_or_recover(&self.inner.runtime_handle);
      if let Some(handle) = stored.as_ref() {
        handle.spawn(dispatch_loop(shared, rx));
      } else {
        // No runtime anywhere: dedicate a thread. Exits when every
        // emitter clone is dropped (queue sender closes).
        std::thread::spawn(move || {
          if let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() {
            rt.block_on(dispatch_loop(shared, rx));
          }
        });
      }
    }
    self.inner.dispatcher_running.store(true, Ordering::Release);
  }

  /// Emit an event. Never blocks and never drops: the event is queued
  /// for the dispatcher task.
  pub fn emit(&self, event: E) {
    self.ensure_dispatcher();
    let _ = self.inner.queue_tx.send(event);
  }

  /// Whether any named listener registered via [`Self::on`] or
  /// [`Self::once`] is filtering for `event_name`. Used by backend
  /// dialog / filechooser / download listeners to decide auto-close
  /// behaviour when nobody is attached. Raw [`Self::subscribe`]
  /// subscribers (`waitForEvent`) don't count here — they are covered
  /// by the grace-period `is_handled` check in the backend listener.
  #[must_use]
  pub fn has_listener(&self, event_name: &str) -> bool {
    lock_or_recover(&self.inner.shared.listeners)
      .iter()
      .any(|slot| slot.name == event_name)
  }

  /// Number of attached consumers: named listeners plus live raw
  /// subscriptions (zero = nobody is listening).
  #[must_use]
  pub fn receiver_count(&self) -> usize {
    lock_or_recover(&self.inner.shared.listeners).len() + lock_or_recover(&self.inner.shared.subscribers).len()
  }

  /// Subscribe to the raw ordered event stream. The subscription is
  /// lossless — events queue until received — and closes when the
  /// emitter is dropped.
  #[must_use]
  pub fn subscribe(&self) -> EventSubscription<E> {
    self.ensure_dispatcher();
    let (tx, rx) = mpsc::unbounded_channel();
    lock_or_recover(&self.inner.shared.subscribers).push(SubscriberSlot { tx });
    EventSubscription { rx }
  }

  /// Wait for an event matching a predicate, with timeout.
  ///
  /// The subscription happens synchronously inside this call — before
  /// the returned future is first polled — so an event emitted on the
  /// very next line cannot be missed.
  ///
  /// # Errors
  ///
  /// Returns an error if the timeout elapses or the emitter is dropped.
  pub fn wait_for<F>(&self, predicate: F, timeout_ms: u64) -> impl Future<Output = crate::error::Result<E>> + Send
  where
    F: Fn(&E) -> bool + Send,
  {
    let mut sub = self.subscribe();
    async move { sub.drain_until(predicate, timeout_ms).await }
  }

  /// Wait for the next event of a specific type, with timeout.
  /// Subscribes synchronously, like [`Self::wait_for`].
  ///
  /// # Errors
  ///
  /// Returns an error if the timeout elapses or the emitter is dropped.
  pub fn wait_for_event(
    &self,
    event_name: &str,
    timeout_ms: u64,
  ) -> impl Future<Output = crate::error::Result<E>> + Send {
    let name = event_name.to_string();
    self.wait_for(move |e| E::matches_name(&name, e), timeout_ms)
  }

  /// Subscribe to events matching a name. The callback runs on the
  /// dispatcher task for each matching event, in registration order
  /// relative to other listeners. Returns a [`ListenerId`] for later
  /// removal with [`Self::off`].
  pub fn on(&self, event_name: &str, callback: Arc<dyn Fn(E) + Send + Sync>) -> ListenerId {
    self.register(event_name, callback, false)
  }

  /// Subscribe to a single event, then auto-remove the listener.
  pub fn once(&self, event_name: &str, callback: Arc<dyn Fn(E) + Send + Sync>) -> ListenerId {
    self.register(event_name, callback, true)
  }

  fn register(&self, event_name: &str, callback: Arc<dyn Fn(E) + Send + Sync>, once: bool) -> ListenerId {
    self.ensure_dispatcher();
    let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
    lock_or_recover(&self.inner.shared.listeners).push(ListenerSlot {
      id,
      name: event_name.to_string(),
      once,
      callback,
    });
    ListenerId(id)
  }

  /// Remove an event listener by ID.
  pub fn off(&self, id: ListenerId) {
    lock_or_recover(&self.inner.shared.listeners).retain(|slot| slot.id != id.0);
  }

  /// Remove all event listeners.
  pub fn remove_all_listeners(&self) {
    lock_or_recover(&self.inner.shared.listeners).clear();
  }

  /// Remove every listener registered for `event_name`, leaving other
  /// events' listeners attached (Playwright's
  /// `removeAllListeners(type)` with a type argument).
  pub fn remove_listeners_named(&self, event_name: &str) {
    lock_or_recover(&self.inner.shared.listeners).retain(|slot| slot.name != event_name);
  }
}

/// Lossless ordered subscription to an [`Emitter`]'s event stream.
/// Returned by [`Emitter::subscribe`]; used by `waitForEvent`-style
/// waiters that need a synchronous subscription point.
pub struct EventSubscription<E> {
  rx: mpsc::UnboundedReceiver<E>,
}

impl<E: EmitterEvent> EventSubscription<E> {
  /// Receive the next event. Returns `None` once the emitter is
  /// dropped. Cancel-safe.
  pub async fn recv(&mut self) -> Option<E> {
    self.rx.recv().await
  }

  /// Drain this subscription until `predicate` matches an event, the
  /// emitter is dropped, or `timeout_ms` elapses.
  ///
  /// # Errors
  ///
  /// Returns an error if the timeout elapses or the emitter is dropped.
  pub async fn drain_until<F>(&mut self, predicate: F, timeout_ms: u64) -> crate::error::Result<E>
  where
    F: Fn(&E) -> bool,
  {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
      let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
      if remaining.is_zero() {
        return Err(crate::error::FerriError::timeout("waiting for event", timeout_ms));
      }
      match tokio::time::timeout(remaining, self.rx.recv()).await {
        Ok(Some(event)) if predicate(&event) => return Ok(event),
        Ok(Some(_)) => {},
        Ok(None) => {
          return Err(crate::error::FerriError::target_closed(Some(
            "event channel closed".into(),
          )));
        },
        Err(_) => return Err(crate::error::FerriError::timeout("waiting for event", timeout_ms)),
      }
    }
  }
}

/// Drain a pre-acquired subscription until `predicate` matches an
/// event, the emitter is dropped, or `timeout_ms` elapses.
///
/// Pairs with [`Emitter::subscribe`]: callers that need a synchronous
/// subscription point (so a downstream `.await` in JS can't race the
/// listener registration) call `subscribe()` first and then
/// `drain_until` inside the spawned future. Playwright implements
/// `helper.waitForEvent` with the same shape — listener registered
/// before the Promise is returned to JS, see
/// `/tmp/playwright/packages/playwright-core/src/server/helper.ts:58`.
///
/// # Errors
///
/// Returns an error if the timeout elapses or the emitter is dropped.
pub async fn drain_until<F>(
  rx: &mut EventSubscription<PageEvent>,
  predicate: F,
  timeout_ms: u64,
) -> crate::error::Result<PageEvent>
where
  F: Fn(&PageEvent) -> bool,
{
  rx.drain_until(predicate, timeout_ms).await
}

/// Broadcast-style page-event emitter. See [`Emitter`].
pub type EventEmitter = Emitter<PageEvent>;

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

impl EmitterEvent for ContextEvent {
  fn matches_name(name: &str, event: &Self) -> bool {
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
}

/// Callback type for context-level event listeners.
pub type ContextEventCallback = Arc<dyn Fn(ContextEvent) + Send + Sync>;

/// Context-event emitter. See [`Emitter`].
pub type ContextEventEmitter = Emitter<ContextEvent>;

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

impl EmitterEvent for BrowserEvent {
  fn matches_name(name: &str, event: &Self) -> bool {
    matches!((name, event), ("context", BrowserEvent::Context(_)))
  }
}

/// Callback type for browser-level event listeners.
pub type BrowserEventCallback = Arc<dyn Fn(BrowserEvent) + Send + Sync>;

/// Browser-event emitter. See [`Emitter`].
pub type BrowserEventEmitter = Emitter<BrowserEvent>;

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::atomic::AtomicUsize;

  fn counting(counter: &Arc<AtomicUsize>) -> EventCallback {
    let counter = Arc::clone(counter);
    Arc::new(move |_| {
      counter.fetch_add(1, Ordering::SeqCst);
    })
  }

  async fn settle(counter: &Arc<AtomicUsize>, expect: usize) {
    for _ in 0..200 {
      if counter.load(Ordering::SeqCst) >= expect {
        return;
      }
      tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn storm_is_lossless() {
    let emitter = EventEmitter::new();
    let counter = Arc::new(AtomicUsize::new(0));
    emitter.on("load", counting(&counter));
    for _ in 0..10_000 {
      emitter.emit(PageEvent::Load);
    }
    settle(&counter, 10_000).await;
    assert_eq!(counter.load(Ordering::SeqCst), 10_000);
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn listeners_fire_in_registration_order() {
    let emitter = EventEmitter::new();
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    for tag in 0..8usize {
      let order = Arc::clone(&order);
      emitter.on(
        "load",
        Arc::new(move |_| {
          lock_or_recover(&order).push(tag);
        }),
      );
    }
    for _ in 0..100 {
      emitter.emit(PageEvent::Load);
    }
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while lock_or_recover(&order).len() < 800 && tokio::time::Instant::now() < deadline {
      tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let seen = lock_or_recover(&order);
    assert_eq!(seen.len(), 800);
    for chunk in seen.chunks(8) {
      assert_eq!(chunk, &[0, 1, 2, 3, 4, 5, 6, 7]);
    }
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn once_fires_exactly_once() {
    let emitter = EventEmitter::new();
    let counter = Arc::new(AtomicUsize::new(0));
    emitter.once("load", counting(&counter));
    for _ in 0..50 {
      emitter.emit(PageEvent::Load);
    }
    settle(&counter, 1).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    assert!(!emitter.has_listener("load"));
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn off_removes_listener() {
    let emitter = EventEmitter::new();
    let counter = Arc::new(AtomicUsize::new(0));
    let id = emitter.on("load", counting(&counter));
    emitter.emit(PageEvent::Load);
    settle(&counter, 1).await;
    emitter.off(id);
    emitter.emit(PageEvent::Load);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(counter.load(Ordering::SeqCst), 1);
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn wait_for_subscribes_synchronously() {
    let emitter = EventEmitter::new();
    // Emit immediately after wait_for returns the future, BEFORE the
    // future is polled. The old emitter subscribed on first poll and
    // missed this event.
    let fut = emitter.wait_for_event("load", 1_000);
    emitter.emit(PageEvent::Load);
    let got = fut.await;
    assert!(got.is_ok());
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn subscription_is_lossless_and_ordered() {
    let emitter = EventEmitter::new();
    let mut sub = emitter.subscribe();
    for _ in 0..1_000 {
      emitter.emit(PageEvent::Load);
    }
    emitter.emit(PageEvent::DomContentLoaded);
    let mut loads = 0;
    loop {
      match sub.recv().await {
        Some(PageEvent::Load) => loads += 1,
        Some(PageEvent::DomContentLoaded) => break,
        other => {
          drop(other);
          break;
        },
      }
    }
    assert_eq!(loads, 1_000);
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn subscription_closes_on_emitter_drop() {
    let emitter = EventEmitter::new();
    let mut sub = emitter.subscribe();
    drop(emitter);
    assert!(sub.recv().await.is_none());
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn callback_can_reenter_emitter() {
    let emitter = EventEmitter::new();
    let counter = Arc::new(AtomicUsize::new(0));
    let em2 = emitter.clone();
    let counter2 = Arc::clone(&counter);
    emitter.on(
      "load",
      Arc::new(move |_| {
        // Reentrant registration + emit from inside a callback must not
        // deadlock the dispatcher.
        em2.once("domcontentloaded", counting(&counter2));
        em2.emit(PageEvent::DomContentLoaded);
      }),
    );
    emitter.emit(PageEvent::Load);
    settle(&counter, 1).await;
    assert_eq!(counter.load(Ordering::SeqCst), 1);
  }
}
