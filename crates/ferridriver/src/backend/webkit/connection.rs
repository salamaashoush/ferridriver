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
use crate::operation_budget::OperationBudget;
use rustc_hash::FxHashMap;
use serde_json::{Value, json};
use std::collections::hash_map::Entry;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
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

struct PendingCallback<'a> {
  connection: &'a Connection,
  id: Option<i64>,
}

impl Drop for PendingCallback<'_> {
  fn drop(&mut self) {
    if let Some(id) = self.id {
      self.connection.forget_callback(id);
    }
  }
}

pub struct Connection {
  writer: Arc<WriterHandle>,
  next_id: AtomicI64,
  callbacks: Mutex<FxHashMap<i64, (RouteKey, ResponseSlot)>>,
  routes: Mutex<FxHashMap<RouteKey, Route>>,
  closed: AtomicBool,
  tasks: crate::backend::transport_tasks::TransportTasks,
}

impl Connection {
  /// Spawn the reader task and return a shared connection handle.
  #[must_use]
  pub fn spawn(transport: Transport) -> Arc<Self> {
    let Transport {
      reader,
      writer,
      shutdown,
      mut tasks,
    } = transport;
    let (ready, connection) = oneshot::channel();
    tasks.push(tokio::spawn(async move {
      if let Ok(connection) = connection.await {
        reader_loop(connection, reader).await;
      }
    }));
    let conn = Arc::new(Connection {
      writer: Arc::new(writer),
      next_id: AtomicI64::new(1),
      callbacks: Mutex::new(FxHashMap::default()),
      routes: Mutex::new(FxHashMap::default()),
      closed: AtomicBool::new(false),
      tasks: crate::backend::transport_tasks::TransportTasks::new(shutdown, tasks),
    });
    let _ = ready.send(Arc::downgrade(&conn));
    conn
  }

  pub(crate) fn start_close(&self) {
    self.drain_all();
    self.tasks.start_close();
  }

  pub(crate) fn request_browser_close(&self) -> Result<(), ConnectionError> {
    let result = self.send_raw(&json!({
      "id": BROWSER_CLOSE_ID, "method": super::protocol::PLAYWRIGHT_CLOSE, "params": {},
    }));
    self.drain_all();
    result
  }

  pub async fn close(&self) -> crate::Result<()> {
    self.start_close();
    self.tasks.close().await
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
    if self.closed.load(Ordering::Acquire) {
      return Err(TransportError::Closed.into());
    }
    self.writer.send(envelope).map_err(ConnectionError::from)
  }

  fn alloc_callback(&self, key: RouteKey) -> (i64, oneshot::Receiver<Result<Value, ErrorPayload>>) {
    let id = self.next_id.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = oneshot::channel();
    let mut callbacks = self.callbacks.lock().unwrap_or_else(PoisonError::into_inner);
    if self.closed.load(Ordering::Acquire) {
      let _ = tx.send(Err(closed_error()));
    } else {
      callbacks.insert(id, (key, tx));
    }
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
    let mut routes = self.routes.lock().unwrap_or_else(PoisonError::into_inner);
    if self.closed.load(Ordering::Acquire) {
      return;
    }
    match routes.entry(key).or_insert_with(|| Route::Buffering(Vec::new())) {
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
    if self.closed.load(Ordering::Acquire) {
      return rx;
    }
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
    self.closed.store(true, Ordering::Release);
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
    self.routes.lock().unwrap_or_else(PoisonError::into_inner).clear();
  }
}

fn closed_error() -> ErrorPayload {
  ErrorPayload {
    message: "transport closed".into(),
    code: None,
    data: None,
  }
}

async fn reader_loop(conn: std::sync::Weak<Connection>, mut reader: ReaderHandle) {
  while let Some(frame) = reader.recv().await {
    let Some(conn) = conn.upgrade() else { return };
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
  if let Some(conn) = conn.upgrade() {
    conn.start_close();
  }
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
    let budget =
      OperationBudget::current(REPLY_TIMEOUT_MS).map_err(|error| ConnectionError::Protocol(error.to_string()))?;
    budget.remaining_ms().map_err(|error| ConnectionError::Timeout {
      method: method.into(),
      ms: error.timeout_ms,
    })?;
    match &self.kind {
      SessionKind::Browser => {
        let (id, rx) = self.conn.alloc_callback(RouteKey::Browser);
        let pending = PendingCallback {
          connection: &self.conn,
          id: Some(id),
        };
        self.send_envelope(method, budget, &json!({ "id": id, "method": method, "params": params }))?;
        wait_for(pending, rx, method, &budget).await
      },
      SessionKind::PageProxy { page_proxy_id } => {
        let (id, rx) = self.conn.alloc_callback(RouteKey::PageProxy(page_proxy_id.clone()));
        let pending = PendingCallback {
          connection: &self.conn,
          id: Some(id),
        };
        self.send_envelope(
          method,
          budget,
          &json!({
            "id": id, "method": method, "params": params, "pageProxyId": page_proxy_id,
          }),
        )?;
        wait_for(pending, rx, method, &budget).await
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
        let pending = PendingCallback {
          connection: &self.conn,
          id: Some(id),
        };
        let inner = serde_json::to_string(&json!({ "id": id, "method": method, "params": params }))?;
        let (wrap_id, wrap_rx) = self.conn.alloc_callback(RouteKey::PageProxy(page_proxy_id.clone()));
        let wrapper = PendingCallback {
          connection: &self.conn,
          id: Some(wrap_id),
        };
        self.send_envelope(
          method,
          budget,
          &json!({
            "id": wrap_id,
            "method": "Target.sendMessageToTarget",
            "params": { "message": inner, "targetId": target_id },
            "pageProxyId": page_proxy_id,
          }),
        )?;
        wait_for(wrapper, wrap_rx, "Target.sendMessageToTarget", &budget).await?;
        wait_for(pending, rx, method, &budget).await
      },
    }
  }

  fn send_envelope(&self, method: &str, budget: OperationBudget, envelope: &Value) -> Result<(), ConnectionError> {
    self.conn.writer.send_checked(envelope, || {
      if self.conn.closed.load(Ordering::Acquire) {
        return Err(ConnectionError::Closed { method: method.into() });
      }
      budget
        .remaining_ms()
        .map(|_| ())
        .map_err(|error| ConnectionError::Timeout {
          method: method.into(),
          ms: error.timeout_ms,
        })
    })
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

/// Default reply budget without an explicit operation scope. A wedged child
/// otherwise leaves calls pending until `drain_all` runs on pipe EOF.
const REPLY_TIMEOUT_MS: u64 = 30_000;

async fn wait_for(
  mut pending: PendingCallback<'_>,
  rx: oneshot::Receiver<Result<Value, ErrorPayload>>,
  method: &str,
  budget: &OperationBudget,
) -> Result<Value, ConnectionError> {
  match budget.wait(rx).await {
    Ok(Ok(result)) => {
      pending.id = None;
      result.map_err(|error| ConnectionError::Protocol(error.message))
    },
    Ok(Err(_)) => Err(ConnectionError::Closed { method: method.into() }),
    Err(error) => Err(ConnectionError::Timeout {
      method: method.into(),
      ms: error.timeout_ms,
    }),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn close_cancels_blocked_io_and_rejects_retained_sessions() {
    use tokio::io::AsyncReadExt;
    let (read, _input) = tokio::io::duplex(1);
    let (write, mut output) = tokio::io::duplex(1);
    let connection = Connection::spawn(Transport::new(read, write));
    let session = connection.browser_session();
    let mut events = session.events();
    let mut request = Box::pin(session.send("Playwright.getInfo", json!({})));
    assert!(futures::poll!(&mut request).is_pending());
    let mut byte = [0];
    output.read_exact(&mut byte).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), connection.close())
      .await
      .unwrap()
      .unwrap();
    assert!(request.await.is_err());
    assert!(session.send("Playwright.getInfo", json!({})).await.is_err());
    assert!(events.recv().await.is_none());
    assert!(session.events().recv().await.is_none());
    assert!(connection.callbacks.lock().unwrap().is_empty());
    assert!(connection.routes.lock().unwrap().is_empty());
    connection.close().await.unwrap();
  }

  #[tokio::test]
  async fn malformed_frame_retires_io_with_a_retained_connection() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (read, mut input) = tokio::io::duplex(128);
    let (write, mut output) = tokio::io::duplex(128);
    let connection = Connection::spawn(Transport::new(read, write));
    let mut events = connection.browser_session().events();
    input.write_all(b"invalid json\0").await.unwrap();
    let mut byte = [0];
    assert_eq!(
      tokio::time::timeout(std::time::Duration::from_secs(1), output.read(&mut byte))
        .await
        .unwrap()
        .unwrap(),
      0
    );
    assert!(events.recv().await.is_none());
    assert!(connection.closed.load(Ordering::Acquire));
    connection.close().await.unwrap();
  }

  fn connection() -> (Arc<Connection>, mpsc::UnboundedReceiver<Vec<u8>>) {
    let (writer, queued) = WriterHandle::test_queue();
    (
      Arc::new(Connection {
        writer: Arc::new(writer),
        next_id: AtomicI64::new(1),
        callbacks: Mutex::new(FxHashMap::default()),
        routes: Mutex::new(FxHashMap::default()),
        closed: AtomicBool::new(false),
        tasks: crate::backend::transport_tasks::TransportTasks::new(tokio::sync::watch::channel(false).0, Vec::new()),
      }),
      queued,
    )
  }

  async fn ids(queued: &mut mpsc::UnboundedReceiver<Vec<u8>>) -> (i64, i64) {
    let mut frame = queued.recv().await.unwrap();
    assert_eq!(frame.pop(), Some(0));
    let envelope: Value = serde_json::from_slice(&frame).unwrap();
    let inner: Value = serde_json::from_str(envelope["params"]["message"].as_str().unwrap()).unwrap();
    (envelope["id"].as_i64().unwrap(), inner["id"].as_i64().unwrap())
  }

  #[tokio::test(start_paused = true)]
  async fn operation_budgets_allow_long_and_unlimited_nested_replies() {
    for timeout in [0, 90_000] {
      let (connection, mut queued) = connection();
      let session = connection.target_session("page", "target");
      let mut command = Box::pin(
        OperationBudget::new(timeout)
          .unwrap()
          .scope(session.send("Runtime.evaluate", json!({}))),
      );
      assert!(futures::poll!(&mut command).is_pending());
      let (wrapper, inner) = ids(&mut queued).await;
      tokio::time::advance(std::time::Duration::from_secs(31)).await;
      assert!(futures::poll!(&mut command).is_pending());
      connection.complete(wrapper, Ok(json!({})));
      assert!(futures::poll!(&mut command).is_pending());
      tokio::time::advance(std::time::Duration::from_secs(31)).await;
      assert!(futures::poll!(&mut command).is_pending());
      connection.complete(inner, Ok(json!({"value":42})));
      assert_eq!(command.await.unwrap()["value"], 42);
      assert!(connection.callbacks.lock().unwrap().is_empty());
    }
  }

  #[tokio::test(start_paused = true)]
  async fn operation_budget_is_shared_and_cancelled_nested_callbacks_are_removed() {
    let (connection, mut queued) = connection();
    let session = connection.target_session("page", "target");
    let mut command = Box::pin(
      OperationBudget::new(1000)
        .unwrap()
        .scope(session.send("Runtime.evaluate", json!({}))),
    );
    assert!(futures::poll!(&mut command).is_pending());
    let (wrapper, inner) = ids(&mut queued).await;
    tokio::time::advance(std::time::Duration::from_millis(700)).await;
    connection.complete(wrapper, Ok(json!({})));
    assert!(futures::poll!(&mut command).is_pending());
    tokio::time::advance(std::time::Duration::from_millis(300)).await;
    assert!(matches!(command.await, Err(ConnectionError::Timeout { ms: 1000, .. })));
    assert!(connection.callbacks.lock().unwrap().is_empty());
    connection.complete(inner, Ok(json!({})));
    for complete_wrapper in [false, true] {
      let mut command = Box::pin(session.send("Runtime.evaluate", json!({})));
      assert!(futures::poll!(&mut command).is_pending());
      let (wrapper, inner) = ids(&mut queued).await;
      if complete_wrapper {
        connection.complete(wrapper, Ok(json!({})));
        assert!(futures::poll!(&mut command).is_pending());
      }
      drop(command);
      assert!(connection.callbacks.lock().unwrap().is_empty());
      connection.complete(wrapper, Ok(json!({})));
      connection.complete(inner, Ok(json!({})));
      assert!(connection.callbacks.lock().unwrap().is_empty());
    }
  }
}
