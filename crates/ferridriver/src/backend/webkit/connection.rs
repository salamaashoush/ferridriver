//! JSON-RPC routing for Playwright's `WebKit` Inspector protocol.
//!
//! The protocol nests three logical levels, but they are NOT three
//! types — they are three ways of *wrapping an outbound message* and
//! one *routing key* for inbound events:
//!
//! - **Browser** — root. `Playwright.*`. Envelope: `{id, method, params}`.
//! - **Page proxy** — keyed by `pageProxyId`. `Target.*`, `Dialog.*`,
//!   `Emulation.*`. Envelope gains a `pageProxyId` field.
//! - **Target** — the inner page session. `Page.*`, `Runtime.*`,
//!   `DOM.*`, `Network.*`, `Input.*`, `Console.*`. The message is
//!   JSON-encoded and shipped as the `message` field of a
//!   `Target.sendMessageToTarget` call on the parent page proxy;
//!   replies arrive wrapped in `Target.dispatchMessageFromTarget`.
//!
//! Per `wkConnection.ts`, message ids come from a single connection-wide
//! counter, so a response routes back purely by `id` — no per-level id
//! space. Only *events* need routing, by `RouteKey`.

use super::protocol::{Envelope, ErrorPayload};
use super::transport::{ReaderHandle, Transport, TransportError, WriterHandle};
use rustc_hash::FxHashMap;
use serde_json::{Value, json};
use std::collections::hash_map::Entry;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

/// Sentinel id `Playwright.close` is sent with — the child never
/// answers it, so inbound frames carrying it are dropped.
const BROWSER_CLOSE_ID: i64 = -9999;

#[derive(Debug, Error)]
pub enum ConnectionError {
  #[error("transport: {0}")]
  Transport(#[from] TransportError),
  #[error("protocol error: {0}")]
  Protocol(String),
  #[error("connection closed before reply for {method:?}")]
  Closed { method: String },
  #[error("timed out after {ms}ms waiting for reply to {method:?}")]
  Timeout { method: String, ms: u64 },
  #[error("json: {0}")]
  Json(#[from] serde_json::Error),
}

impl From<ConnectionError> for crate::error::FerriError {
  fn from(e: ConnectionError) -> Self {
    match e {
      ConnectionError::Timeout { method, ms } => {
        crate::error::FerriError::timeout(format!("webkit: waiting for {method} reply"), ms)
      },
      other => crate::error::FerriError::backend(format!("webkit: {other}")),
    }
  }
}

type ResponseSlot = oneshot::Sender<Result<Value, ErrorPayload>>;

/// Routing key for inbound events. Responses do not need this — they
/// route by global id — but a pending callback is tagged with it so
/// closing a route can reject the calls still waiting on it.
#[derive(Clone, PartialEq, Eq, Hash)]
enum RouteKey {
  Browser,
  PageProxy(String),
  Target(String),
}

/// One event stream. Starts `Buffering` because the child can emit
/// events (`Target.targetCreated`) before our code subscribes; the
/// reader auto-creates the entry and stashes them. The first
/// [`Connection::subscribe`] flips it `Live` and replays the buffer
/// into that subscriber's queue. Subscribers get unbounded lossless
/// queues: state-mutating consumers (frame cache, network correlation,
/// lifecycle signals) ride these streams, and a dropped
/// `Page.loadEventFired` / `Page.frameNavigated` wedges
/// `wait_for_lifecycle` or leaves the frame cache stale.
enum Route {
  Buffering(Vec<Envelope>),
  Live(Vec<mpsc::UnboundedSender<Envelope>>),
}

pub struct Connection {
  writer: Arc<WriterHandle>,
  next_id: AtomicI64,
  callbacks: Mutex<FxHashMap<i64, (RouteKey, ResponseSlot)>>,
  routes: Mutex<FxHashMap<RouteKey, Route>>,
}

impl Connection {
  /// Spawn the reader task and return a shared connection handle.
  #[must_use]
  pub fn spawn(transport: Transport) -> Arc<Self> {
    let Transport { reader, writer } = transport;
    let conn = Arc::new(Connection {
      writer: Arc::new(writer),
      next_id: AtomicI64::new(1),
      callbacks: Mutex::new(FxHashMap::default()),
      routes: Mutex::new(FxHashMap::default()),
    });
    tokio::spawn(reader_loop(Arc::clone(&conn), reader));
    conn
  }

  /// Handle on the root browser session.
  #[must_use]
  pub fn browser_session(self: &Arc<Self>) -> Session {
    Session {
      conn: Arc::clone(self),
      kind: SessionKind::Browser,
    }
  }

  /// Handle on the page-proxy session for `page_proxy_id`.
  #[must_use]
  pub fn page_proxy_session(self: &Arc<Self>, page_proxy_id: impl Into<String>) -> Session {
    Session {
      conn: Arc::clone(self),
      kind: SessionKind::PageProxy {
        page_proxy_id: page_proxy_id.into(),
      },
    }
  }

  /// Underlying `Arc<Connection>` for a given [`Session`]. Used by
  /// page close paths that need to drain pending callbacks for the
  /// page's proxy/target routes.
  #[must_use]
  pub fn arc(self: &Arc<Self>) -> Arc<Self> {
    Arc::clone(self)
  }

  /// Handle on the inner target session reached through `page_proxy_id`.
  #[must_use]
  pub fn target_session(self: &Arc<Self>, page_proxy_id: impl Into<String>, target_id: impl Into<String>) -> Session {
    Session {
      conn: Arc::clone(self),
      kind: SessionKind::Target {
        page_proxy_id: page_proxy_id.into(),
        target_id: target_id.into(),
      },
    }
  }

  /// Reject every pending call on a route and drop its event stream.
  /// Called when a page proxy or target goes away.
  pub fn close_route(&self, page_proxy_id: Option<&str>, target_id: Option<&str>) {
    let key = match (page_proxy_id, target_id) {
      (_, Some(t)) => RouteKey::Target(t.to_string()),
      (Some(p), None) => RouteKey::PageProxy(p.to_string()),
      (None, None) => RouteKey::Browser,
    };
    let mut callbacks = self.callbacks.lock().unwrap_or_else(PoisonError::into_inner);
    let ids: Vec<i64> = callbacks
      .iter()
      .filter(|(_, (k, _))| *k == key)
      .map(|(id, _)| *id)
      .collect();
    let drained: Vec<ResponseSlot> = ids
      .iter()
      .filter_map(|id| callbacks.remove(id))
      .map(|(_, slot)| slot)
      .collect();
    drop(callbacks);
    for slot in drained {
      let _ = slot.send(Err(closed_error()));
    }
    self.routes.lock().unwrap_or_else(PoisonError::into_inner).remove(&key);
  }

  /// Fire a raw envelope onto the wire without expecting a response.
  /// Used by `Playwright.close`, which the child answers by closing
  /// the pipe rather than replying.
  pub fn send_raw(&self, envelope: &Value) -> Result<(), ConnectionError> {
    self.writer.send(envelope).map_err(ConnectionError::from)
  }

  fn alloc_callback(&self, key: RouteKey) -> (i64, oneshot::Receiver<Result<Value, ErrorPayload>>) {
    let id = self.next_id.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = oneshot::channel();
    self
      .callbacks
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .insert(id, (key, tx));
    (id, rx)
  }

  /// Drop a callback slot after a send failure or reply timeout so the
  /// entry doesn't sit in the table until the whole route closes.
  fn forget_callback(&self, id: i64) {
    self
      .callbacks
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .remove(&id);
  }

  fn complete(&self, id: i64, result: Result<Value, ErrorPayload>) {
    let slot = self
      .callbacks
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .remove(&id)
      .map(|(_, slot)| slot);
    if let Some(slot) = slot {
      let _ = slot.send(result);
    }
  }

  /// Deliver an event to its route, creating a `Buffering` entry if no
  /// subscriber has claimed the route yet.
  fn route_event(&self, key: RouteKey, env: Envelope) {
    match self
      .routes
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .entry(key)
      .or_insert_with(|| Route::Buffering(Vec::new()))
    {
      Route::Buffering(buf) => buf.push(env),
      Route::Live(txs) => {
        txs.retain(|tx| tx.send(env.clone()).is_ok());
      },
    }
  }

  /// Subscribe to a route's events. The first subscriber flips the
  /// route `Live` and replays whatever the reader buffered before the
  /// route had an owner. The stream is lossless — events queue
  /// unbounded until received.
  fn subscribe(&self, key: RouteKey) -> mpsc::UnboundedReceiver<Envelope> {
    let mut routes = self.routes.lock().unwrap_or_else(PoisonError::into_inner);
    let (tx, rx) = mpsc::unbounded_channel();
    match routes.entry(key) {
      Entry::Occupied(mut e) => match e.get_mut() {
        Route::Live(txs) => txs.push(tx),
        Route::Buffering(buf) => {
          for env in std::mem::take(buf) {
            let _ = tx.send(env);
          }
          e.insert(Route::Live(vec![tx]));
        },
      },
      Entry::Vacant(e) => {
        e.insert(Route::Live(vec![tx]));
      },
    }
    rx
  }

  /// Reject every pending call. Invoked on transport EOF.
  fn drain_all(&self) {
    let drained: Vec<ResponseSlot> = self
      .callbacks
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .drain()
      .map(|(_, (_, slot))| slot)
      .collect();
    for slot in drained {
      let _ = slot.send(Err(closed_error()));
    }
  }
}

fn closed_error() -> ErrorPayload {
  ErrorPayload {
    message: "transport closed".into(),
    code: None,
    data: None,
  }
}

async fn reader_loop(conn: Arc<Connection>, mut reader: ReaderHandle) {
  while let Some(frame) = reader.recv().await {
    let raw = match frame {
      Ok(v) => v,
      Err(e) => {
        tracing::error!(target: "ferridriver::webkit", "reader: {e}");
        break;
      },
    };
    tracing::debug!(target: "ferridriver::webkit", "recv: {raw}");
    match serde_json::from_value::<Envelope>(raw) {
      Ok(env) => dispatch(&conn, env),
      Err(e) => tracing::warn!(target: "ferridriver::webkit", "skip un-parseable frame: {e}"),
    }
  }
  conn.drain_all();
}

/// Route one inbound envelope. A frame carrying an `id` is a response
/// (route by global id); a frame carrying a `method` is an event
/// (route by [`RouteKey`]). `Target.dispatchMessageFromTarget` is
/// transport plumbing — its inner message is unwrapped and re-routed.
fn dispatch(conn: &Connection, env: Envelope) {
  if env.id == Some(BROWSER_CLOSE_ID) {
    return;
  }
  if let Some(id) = env.id {
    conn.complete(id, response_of(env));
    return;
  }
  let Some(method) = env.method.as_deref() else {
    return;
  };
  if method == "Target.dispatchMessageFromTarget" {
    if let Some((target_id, inner)) = unwrap_target_message(&env) {
      route_target_inner(conn, &target_id, inner);
    }
    return;
  }
  match env.page_proxy_id.clone() {
    Some(proxy) => conn.route_event(RouteKey::PageProxy(proxy), env),
    None => conn.route_event(RouteKey::Browser, env),
  }
}

/// Decode the JSON payload nested inside `Target.dispatchMessageFromTarget`.
fn unwrap_target_message(env: &Envelope) -> Option<(String, Envelope)> {
  let target_id = env.params.get("targetId").and_then(Value::as_str)?.to_string();
  let message = env.params.get("message").and_then(Value::as_str)?;
  let inner = serde_json::from_str::<Envelope>(message).ok()?;
  Some((target_id, inner))
}

fn route_target_inner(conn: &Connection, target_id: &str, env: Envelope) {
  if let Some(id) = env.id {
    conn.complete(id, response_of(env));
  } else if env.method.is_some() {
    conn.route_event(RouteKey::Target(target_id.to_string()), env);
  }
}

/// Consumes the envelope so large result payloads (screenshot base64,
/// resource bodies) move into the caller's oneshot instead of being
/// deep-cloned.
fn response_of(env: Envelope) -> Result<Value, ErrorPayload> {
  match env.error {
    Some(err) => Err(err),
    None => Ok(env.result.unwrap_or(Value::Null)),
  }
}

/// Which level of the protocol a [`Session`] speaks. Determines how
/// outbound messages are wrapped and which [`RouteKey`] events route to.
#[derive(Clone)]
enum SessionKind {
  Browser,
  PageProxy { page_proxy_id: String },
  Target { page_proxy_id: String, target_id: String },
}

/// A protocol session — one handle, three flavours. Cloning is cheap
/// (an `Arc` bump plus a couple of `String`s) and every clone shares
/// the connection's id space and callback table.
#[derive(Clone)]
pub struct Session {
  conn: Arc<Connection>,
  kind: SessionKind,
}

impl Session {
  /// The underlying [`Connection`] handle.
  #[must_use]
  pub fn connection_handle(&self) -> Arc<Connection> {
    Arc::clone(&self.conn)
  }

  /// `pageProxyId` for page-proxy and target sessions; `None` for the
  /// root browser session.
  #[must_use]
  pub fn page_proxy_id(&self) -> Option<&str> {
    match &self.kind {
      SessionKind::Browser => None,
      SessionKind::PageProxy { page_proxy_id } | SessionKind::Target { page_proxy_id, .. } => Some(page_proxy_id),
    }
  }

  /// `targetId` for target sessions; `None` otherwise.
  #[must_use]
  pub fn target_id(&self) -> Option<&str> {
    match &self.kind {
      SessionKind::Target { target_id, .. } => Some(target_id),
      _ => None,
    }
  }

  /// Send `method` and await its reply.
  pub async fn send(&self, method: &str, params: Value) -> Result<Value, ConnectionError> {
    match &self.kind {
      SessionKind::Browser => {
        let (id, rx) = self.conn.alloc_callback(RouteKey::Browser);
        if let Err(e) = self
          .conn
          .writer
          .send(&json!({ "id": id, "method": method, "params": params }))
        {
          self.conn.forget_callback(id);
          return Err(e.into());
        }
        wait_for(&self.conn, id, rx, method).await
      },
      SessionKind::PageProxy { page_proxy_id } => {
        let (id, rx) = self.conn.alloc_callback(RouteKey::PageProxy(page_proxy_id.clone()));
        if let Err(e) = self.conn.writer.send(&json!({
          "id": id, "method": method, "params": params, "pageProxyId": page_proxy_id,
        })) {
          self.conn.forget_callback(id);
          return Err(e.into());
        }
        wait_for(&self.conn, id, rx, method).await
      },
      SessionKind::Target {
        page_proxy_id,
        target_id,
      } => {
        // The inner call gets its own (global) id; its reply arrives
        // wrapped in `Target.dispatchMessageFromTarget` and routes
        // back by that id. The `Target.sendMessageToTarget` wrapper
        // gets a second id on the page-proxy level — we await it so a
        // wrapper-level rejection (target gone) surfaces instead of
        // hanging on the inner reply.
        let (id, rx) = self.conn.alloc_callback(RouteKey::Target(target_id.clone()));
        let inner = match serde_json::to_string(&json!({ "id": id, "method": method, "params": params })) {
          Ok(s) => s,
          Err(e) => {
            self.conn.forget_callback(id);
            return Err(e.into());
          },
        };
        let (wrap_id, wrap_rx) = self.conn.alloc_callback(RouteKey::PageProxy(page_proxy_id.clone()));
        if let Err(e) = self.conn.writer.send(&json!({
          "id": wrap_id,
          "method": "Target.sendMessageToTarget",
          "params": { "message": inner, "targetId": target_id },
          "pageProxyId": page_proxy_id,
        })) {
          self.conn.forget_callback(id);
          self.conn.forget_callback(wrap_id);
          return Err(e.into());
        }
        if let Err(e) = wait_for(&self.conn, wrap_id, wrap_rx, "Target.sendMessageToTarget").await {
          // The inner reply will never arrive once the wrapper is
          // rejected — drop its slot instead of waiting for the
          // route to close.
          self.conn.forget_callback(id);
          return Err(e);
        }
        wait_for(&self.conn, id, rx, method).await
      },
    }
  }

  /// Subscribe to this session's events.
  #[must_use]
  pub fn events(&self) -> mpsc::UnboundedReceiver<Envelope> {
    self.conn.subscribe(self.route_key())
  }

  fn route_key(&self) -> RouteKey {
    match &self.kind {
      SessionKind::Browser => RouteKey::Browser,
      SessionKind::PageProxy { page_proxy_id } => RouteKey::PageProxy(page_proxy_id.clone()),
      SessionKind::Target { target_id, .. } => RouteKey::Target(target_id.clone()),
    }
  }
}

/// Reply timeout for a single protocol call. Matches the CDP
/// transport's 30s cap — without it a wedged (alive but unresponsive)
/// child hangs the caller forever, since `drain_all` only fires on
/// pipe EOF.
const REPLY_TIMEOUT_MS: u64 = 30_000;
const REPLY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(REPLY_TIMEOUT_MS);

async fn wait_for(
  conn: &Connection,
  id: i64,
  rx: oneshot::Receiver<Result<Value, ErrorPayload>>,
  method: &str,
) -> Result<Value, ConnectionError> {
  match tokio::time::timeout(REPLY_TIMEOUT, rx).await {
    Ok(Ok(Ok(v))) => Ok(v),
    Ok(Ok(Err(err))) => Err(ConnectionError::Protocol(err.message)),
    Ok(Err(_)) => Err(ConnectionError::Closed { method: method.into() }),
    Err(_) => {
      conn.forget_callback(id);
      Err(ConnectionError::Timeout {
        method: method.into(),
        ms: REPLY_TIMEOUT_MS,
      })
    },
  }
}
