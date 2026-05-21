//! Three-level JSON-RPC routing for Playwright's `WebKit` Inspector
//! protocol:
//!
//! 1. **Browser session** — the root. `Playwright.enable`,
//!    `Playwright.createContext`, `Playwright.createPage`, …. Outbound
//!    messages go straight onto the wire.
//! 2. **Page-proxy session** — keyed by `pageProxyId`. Methods
//!    `Target.sendMessageToTarget`, `Target.activate`, `Target.close`,
//!    `Dialog.handleJavaScriptDialog`. Outbound messages get
//!    `pageProxyId` added to the envelope.
//! 3. **Target session** — keyed by `targetId` (a.k.a. the inner
//!    `sessionId`). Methods `Page.*`, `Runtime.*`, `DOM.*`,
//!    `Network.*`, `Input.*`, `Console.*`. Outbound messages are
//!    serialized as JSON, wrapped in
//!    `Target.sendMessageToTarget({ message, targetId })` on the
//!    parent page-proxy session. Inbound messages arrive on the
//!    parent page-proxy session as
//!    `Target.dispatchMessageFromTarget` events.
//!
//! Each level has its own monotonic id space + callback table so
//! responses route back to the originating call regardless of how
//! deep the nesting goes.

use super::protocol::{Envelope, ErrorPayload};
use super::transport::{Transport, TransportError, WriterHandle};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use thiserror::Error;
use tokio::sync::{broadcast, oneshot};

#[derive(Debug, Error)]
pub enum ConnectionError {
  #[error("transport: {0}")]
  Transport(#[from] TransportError),
  #[error("protocol error: {0}")]
  Protocol(String),
  #[error("connection closed before reply for {method:?}")]
  Closed { method: String },
  #[error("json: {0}")]
  Json(#[from] serde_json::Error),
}

type ResponseSlot = oneshot::Sender<Result<Value, ErrorPayload>>;

/// Shared callback table + id counter. Each session owns its own
/// instance — responses are keyed by the session's local id, NOT a
/// global one.
struct SessionState {
  next_id: AtomicI64,
  callbacks: Mutex<HashMap<i64, ResponseSlot>>,
  events: broadcast::Sender<Envelope>,
  /// Events the connection's reader buffered before the owning
  /// session was registered. Drained on the first
  /// [`PageProxySession::drain_pending`] call.
  pending: Mutex<Vec<Envelope>>,
}

impl SessionState {
  fn new() -> Arc<Self> {
    let (events, _) = broadcast::channel(256);
    Arc::new(SessionState {
      next_id: AtomicI64::new(1),
      callbacks: Mutex::new(HashMap::new()),
      events,
      pending: Mutex::new(Vec::new()),
    })
  }

  fn alloc_callback(&self) -> (i64, oneshot::Receiver<Result<Value, ErrorPayload>>) {
    let id = self.next_id.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = oneshot::channel();
    if let Ok(mut g) = self.callbacks.lock() {
      g.insert(id, tx);
    }
    (id, rx)
  }

  fn complete(&self, id: i64, result: Result<Value, ErrorPayload>) {
    if let Ok(mut g) = self.callbacks.lock() {
      if let Some(tx) = g.remove(&id) {
        let _ = tx.send(result);
      }
    }
  }

  fn dispatch_event(&self, env: Envelope) {
    let _ = self.events.send(env);
  }

  fn drain_with_error(&self, message: &str) {
    if let Ok(mut g) = self.callbacks.lock() {
      for (_, tx) in g.drain() {
        let _ = tx.send(Err(ErrorPayload {
          message: message.into(),
          code: None,
          data: None,
        }));
      }
    }
  }
}

/// Owns the transport + the browser-level session state. Holds weak
/// pointers to live page-proxy sessions so inbound traffic can be
/// routed to the right callback table.
pub struct Connection {
  writer: Arc<WriterHandle>,
  browser: Arc<SessionState>,
  page_proxies: Mutex<HashMap<String, Arc<SessionState>>>,
  targets: Mutex<HashMap<String, Arc<SessionState>>>,
  /// Events that arrive with a `pageProxyId` we haven't registered
  /// yet. The server can emit `Target.targetCreated` before our
  /// `Playwright.createPage` response races back to us — without
  /// buffering, those events drop on the floor and the caller hangs
  /// waiting for them. On `open_page_proxy(id)` we drain the buffer
  /// into the new session.
  pending_proxy_events: Mutex<HashMap<String, Vec<Envelope>>>,
  /// Same race for inner target sessions: `Target.dispatchMessageFromTarget`
  /// can arrive before `open_target` registers the target session.
  pending_target_events: Mutex<HashMap<String, Vec<Envelope>>>,
}

impl Connection {
  /// Spawn the reader task and return a shared connection handle.
  pub fn spawn(transport: Transport) -> Arc<Self> {
    let Transport { reader, writer } = transport;
    let writer = Arc::new(writer);
    let conn = Arc::new(Connection {
      writer,
      browser: SessionState::new(),
      page_proxies: Mutex::new(HashMap::new()),
      targets: Mutex::new(HashMap::new()),
      pending_proxy_events: Mutex::new(HashMap::new()),
      pending_target_events: Mutex::new(HashMap::new()),
    });
    let reader_conn = Arc::clone(&conn);
    tokio::spawn(reader_loop(reader_conn, reader));
    conn
  }

  /// Handle on the root browser session.
  #[must_use]
  pub fn browser(self: &Arc<Self>) -> BrowserSession {
    BrowserSession {
      conn: Arc::clone(self),
      state: Arc::clone(&self.browser),
    }
  }

  /// Register a [`PageProxySession`] for `page_proxy_id`. Subsequent
  /// inbound messages with that `pageProxyId` envelope field route
  /// to its callback table.
  pub fn open_page_proxy(self: &Arc<Self>, page_proxy_id: impl Into<String>) -> PageProxySession {
    let id = page_proxy_id.into();
    let state = SessionState::new();
    if let Ok(mut g) = self.page_proxies.lock() {
      g.insert(id.clone(), Arc::clone(&state));
    }
    // Stash any events the reader buffered before this session was
    // registered. `route_to_session`-ing them here would broadcast on
    // an empty subscriber set (we have no receivers yet) and the
    // events would disappear. Caller drains via [`PageProxySession::drain_pending`]
    // BEFORE subscribing to live events.
    let drained: Vec<Envelope> = self
      .pending_proxy_events
      .lock()
      .ok()
      .and_then(|mut g| g.remove(&id))
      .unwrap_or_default();
    if let Ok(mut p) = state.pending.lock() {
      *p = drained;
    }
    PageProxySession {
      conn: Arc::clone(self),
      state,
      page_proxy_id: id,
    }
  }

  /// Unregister a page-proxy session by id. Pending callbacks are
  /// flushed with a transport-closed error.
  pub fn close_page_proxy(&self, page_proxy_id: &str) {
    let removed = self.page_proxies.lock().ok().and_then(|mut g| g.remove(page_proxy_id));
    if let Some(state) = removed {
      state.drain_with_error("page proxy closed");
    }
  }

  /// Register a [`TargetSession`] for `target_id`, child of the given
  /// page-proxy session. Outbound messages get wrapped via
  /// `Target.sendMessageToTarget` on the parent proxy.
  pub fn open_target(self: &Arc<Self>, parent: &PageProxySession, target_id: impl Into<String>) -> TargetSession {
    let id = target_id.into();
    let state = SessionState::new();
    if let Ok(mut g) = self.targets.lock() {
      g.insert(id.clone(), Arc::clone(&state));
    }
    let drained: Vec<Envelope> = self
      .pending_target_events
      .lock()
      .ok()
      .and_then(|mut g| g.remove(&id))
      .unwrap_or_default();
    if let Ok(mut p) = state.pending.lock() {
      *p = drained;
    }
    TargetSession {
      state,
      parent: parent.clone(),
      target_id: id,
    }
  }

  /// Unregister a target session.
  pub fn close_target(&self, target_id: &str) {
    let removed = self.targets.lock().ok().and_then(|mut g| g.remove(target_id));
    if let Some(state) = removed {
      state.drain_with_error("target closed");
    }
  }

  fn writer(&self) -> &Arc<WriterHandle> {
    &self.writer
  }

  /// Fire a raw envelope onto the wire without expecting a response.
  /// Used by `Playwright.close` and any other call that the child
  /// answers by closing the pipe rather than sending a result.
  pub fn send_raw(&self, envelope: &Value) -> Result<(), ConnectionError> {
    self.writer.send(envelope).map_err(ConnectionError::from)
  }
}

async fn reader_loop(conn: Arc<Connection>, mut reader: super::transport::ReaderHandle) {
  while let Some(frame) = reader.recv().await {
    let raw = match frame {
      Ok(v) => v,
      Err(e) => {
        tracing::error!(target: "ferridriver::pw_webkit", "reader: {e}");
        break;
      },
    };
    tracing::debug!(target: "ferridriver::pw_webkit", "recv: {raw}");
    let env: Envelope = match serde_json::from_value(raw) {
      Ok(e) => e,
      Err(e) => {
        tracing::warn!(target: "ferridriver::pw_webkit", "skip un-parseable frame: {e}");
        continue;
      },
    };
    dispatch_frame(&conn, env);
  }
  // EOF / error — flush every callback table.
  conn.browser.drain_with_error("transport closed");
  if let Ok(g) = conn.page_proxies.lock() {
    for state in g.values() {
      state.drain_with_error("transport closed");
    }
  }
  if let Ok(g) = conn.targets.lock() {
    for state in g.values() {
      state.drain_with_error("transport closed");
    }
  }
}

/// Route an inbound envelope to the right session. Order matters:
/// target-wrapped messages arrive AS events on a page-proxy session
/// (`Target.dispatchMessageFromTarget`), so we have to inspect the
/// method name BEFORE forwarding to the proxy's event bus.
fn dispatch_frame(conn: &Connection, env: Envelope) {
  // Target-wrapped response: `pageProxyId` set + event method is
  // `Target.dispatchMessageFromTarget`. Unwrap the inner JSON and
  // route through the target session.
  if let Some(ref method) = env.method {
    if method == "Target.dispatchMessageFromTarget" {
      if let Some(target_id) = env.params.get("targetId").and_then(Value::as_str) {
        if let Some(message_str) = env.params.get("message").and_then(Value::as_str) {
          if let Ok(inner) = serde_json::from_str::<Envelope>(message_str) {
            route_to_target(conn, target_id, inner);
            return;
          }
        }
      }
    }
  }

  // Response / event routing by envelope shape.
  if let Some(page_proxy_id) = env.page_proxy_id.clone() {
    let state = conn
      .page_proxies
      .lock()
      .ok()
      .and_then(|g| g.get(&page_proxy_id).cloned());
    if let Some(state) = state {
      route_to_session(&state, env);
      return;
    }
    // Buffer for the proxy to claim once it's registered. Without
    // this, `Target.targetCreated` events that race ahead of the
    // `Playwright.createPage` response (and our subsequent
    // `open_page_proxy` call) get dropped.
    if let Ok(mut g) = conn.pending_proxy_events.lock() {
      g.entry(page_proxy_id).or_default().push(env);
    }
    return;
  }

  route_to_session(&conn.browser, env);
}

fn route_to_target(conn: &Connection, target_id: &str, env: Envelope) {
  let state = conn.targets.lock().ok().and_then(|g| g.get(target_id).cloned());
  if let Some(state) = state {
    route_to_session(&state, env);
    return;
  }
  // Same race as the proxy buffer above: `Target.dispatchMessageFromTarget`
  // can arrive before `open_target` registers the session.
  if let Ok(mut g) = conn.pending_target_events.lock() {
    g.entry(target_id.to_string()).or_default().push(env);
  }
}

fn route_to_session(state: &Arc<SessionState>, env: Envelope) {
  if let Some(id) = env.id {
    let result = if let Some(err) = env.error.clone() {
      Err(err)
    } else {
      Ok(env.result.clone().unwrap_or(Value::Null))
    };
    state.complete(id, result);
    return;
  }
  if env.method.is_some() {
    state.dispatch_event(env);
  }
}

// ── Session handles ───────────────────────────────────────────────────

/// Root browser session. One per [`Connection`].
#[derive(Clone)]
pub struct BrowserSession {
  conn: Arc<Connection>,
  state: Arc<SessionState>,
}

impl BrowserSession {
  pub async fn send(&self, method: &str, params: Value) -> Result<Value, ConnectionError> {
    let (id, rx) = self.state.alloc_callback();
    let envelope = json!({ "id": id, "method": method, "params": params });
    self.conn.writer().send(&envelope)?;
    wait_for(rx, method).await
  }

  #[must_use]
  pub fn events(&self) -> broadcast::Receiver<Envelope> {
    self.state.events.subscribe()
  }
}

/// Per-page-proxy session. Outbound calls add `pageProxyId` to the
/// envelope; inbound is routed by the connection's
/// `dispatch_frame`.
#[derive(Clone)]
pub struct PageProxySession {
  conn: Arc<Connection>,
  state: Arc<SessionState>,
  page_proxy_id: String,
}

impl PageProxySession {
  #[must_use]
  pub fn page_proxy_id(&self) -> &str {
    &self.page_proxy_id
  }

  /// Drain events the connection's reader buffered between the
  /// `Playwright.createPage` request hitting the wire and the
  /// corresponding [`Connection::open_page_proxy`] call. Returns the
  /// stashed envelopes in arrival order, then clears the buffer.
  /// Callers that need to observe pre-subscription events (e.g.
  /// `Target.targetCreated`) MUST call this before [`Self::events`].
  #[must_use]
  pub fn drain_pending(&self) -> Vec<Envelope> {
    self
      .state
      .pending
      .lock()
      .map(|mut g| std::mem::take(&mut *g))
      .unwrap_or_default()
  }

  pub async fn send(&self, method: &str, params: Value) -> Result<Value, ConnectionError> {
    let (id, rx) = self.state.alloc_callback();
    let envelope = json!({
      "id": id,
      "method": method,
      "params": params,
      "pageProxyId": self.page_proxy_id,
    });
    self.conn.writer().send(&envelope)?;
    wait_for(rx, method).await
  }

  #[must_use]
  pub fn events(&self) -> broadcast::Receiver<Envelope> {
    self.state.events.subscribe()
  }
}

/// Target session — inner page session reached via
/// `Target.sendMessageToTarget` on the parent [`PageProxySession`].
/// Each outbound call is JSON-encoded and sent as the `message` field
/// of a `Target.sendMessageToTarget` call on the parent.
#[derive(Clone)]
pub struct TargetSession {
  state: Arc<SessionState>,
  parent: PageProxySession,
  target_id: String,
}

impl TargetSession {
  #[must_use]
  pub fn target_id(&self) -> &str {
    &self.target_id
  }

  /// Drain events buffered before [`Connection::open_target`]
  /// registered the session. Mirrors [`PageProxySession::drain_pending`].
  #[must_use]
  pub fn drain_pending(&self) -> Vec<Envelope> {
    self
      .state
      .pending
      .lock()
      .map(|mut g| std::mem::take(&mut *g))
      .unwrap_or_default()
  }

  pub async fn send(&self, method: &str, params: Value) -> Result<Value, ConnectionError> {
    let (id, rx) = self.state.alloc_callback();
    let inner = json!({ "id": id, "method": method, "params": params });
    let inner_str = serde_json::to_string(&inner)?;
    // The wrapper call to the parent proxy session must complete
    // before we await the inner response — if the parent rejects
    // (e.g. target closed), we need to surface that synchronously
    // rather than block forever on `rx`.
    self
      .parent
      .send(
        "Target.sendMessageToTarget",
        json!({ "message": inner_str, "targetId": self.target_id }),
      )
      .await?;
    wait_for(rx, method).await
  }

  #[must_use]
  pub fn events(&self) -> broadcast::Receiver<Envelope> {
    self.state.events.subscribe()
  }
}

async fn wait_for(rx: oneshot::Receiver<Result<Value, ErrorPayload>>, method: &str) -> Result<Value, ConnectionError> {
  match rx.await {
    Ok(Ok(v)) => Ok(v),
    Ok(Err(err)) => Err(ConnectionError::Protocol(err.message)),
    Err(_) => Err(ConnectionError::Closed { method: method.into() }),
  }
}

// Backwards-compatible aliases for the earlier two-level skeleton —
// downstream code can use `Session` to mean the page-proxy level until
// it's updated to the typed forms.
pub type Session = PageProxySession;
