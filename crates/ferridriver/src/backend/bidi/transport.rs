//! WebSocket transport for `WebDriver` `BiDi` protocol.
//!
//! Handles connection, command/response correlation, and event dispatch.
//! Uses `json_scan` for zero-allocation hot-path field extraction (same as CDP).

use dashmap::DashMap;
use futures::StreamExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, trace, warn};

use crate::backend::json_scan;
use crate::error::{FerriError, Result};

// ── Types ──────────────────────────────────────────────────────────────────

/// Error from a `BiDi` command.
#[derive(Debug, Clone)]
pub(crate) struct BidiError {
  pub error: String,
  pub message: String,
}

impl std::fmt::Display for BidiError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "BiDi error '{}': {}", self.error, self.message)
  }
}

type BidiResult = std::result::Result<serde_json::Value, BidiError>;

/// A `BiDi` event received from the browser.
#[derive(Debug, Clone)]
pub(crate) struct BidiEvent {
  /// Event method name, e.g. "browsingContext.load"
  pub method: String,
  /// Raw event params
  pub params: serde_json::Value,
}

enum TapMessage {
  Event(BidiEvent),
  Barrier(oneshot::Sender<()>),
}

#[derive(Clone)]
pub(crate) struct EventBarrier {
  sender: mpsc::WeakUnboundedSender<TapMessage>,
}

impl EventBarrier {
  pub async fn wait(&self) -> Result<()> {
    let (tx, rx) = oneshot::channel();
    self
      .sender
      .upgrade()
      .ok_or_else(|| FerriError::backend("BiDi event consumer closed"))?
      .send(TapMessage::Barrier(tx))
      .map_err(|_| FerriError::backend("BiDi event consumer closed"))?;
    rx.await.map_err(|_| FerriError::backend("BiDi event consumer closed"))
  }
}

pub(crate) struct EventTap {
  receiver: mpsc::UnboundedReceiver<TapMessage>,
  barrier: EventBarrier,
}

impl EventTap {
  fn new() -> (mpsc::UnboundedSender<TapMessage>, Self) {
    let (sender, receiver) = mpsc::unbounded_channel();
    let barrier = EventBarrier {
      sender: sender.downgrade(),
    };
    (sender, Self { receiver, barrier })
  }

  pub fn barrier(&self) -> EventBarrier {
    self.barrier.clone()
  }

  pub async fn recv(&mut self) -> Option<BidiEvent> {
    while let Some(message) = self.receiver.recv().await {
      match message {
        TapMessage::Event(event) => return Some(event),
        TapMessage::Barrier(done) => {
          let _ = done.send(());
        },
      }
    }
    None
  }
}

/// Pending command map: command ID -> oneshot sender for the response.
type PendingMap = DashMap<u64, oneshot::Sender<BidiResult>>;

struct PendingCommand<'a> {
  pending: &'a PendingMap,
  id: u64,
}

impl Drop for PendingCommand<'_> {
  fn drop(&mut self) {
    self.pending.remove(&self.id);
  }
}

fn fail_pending(pending: &PendingMap) {
  let ids: Vec<u64> = pending.iter().map(|entry| *entry.key()).collect();
  for id in ids {
    let Some((_, tx)) = pending.remove(&id) else {
      continue;
    };
    let _ = tx.send(Err(BidiError {
      error: "target closed".into(),
      message: "BiDi transport closed".into(),
    }));
  }
}

// ── Transport ──────────────────────────────────────────────────────────────

/// High-performance WebSocket transport for the `BiDi` protocol.
///
/// Design principles:
/// - Zero-alloc hot path: `json_scan` extracts `type`, `id`, `method` without full parse
/// - Direct string command building: skip `serde_json::Value` intermediary for envelope
/// - Single WebSocket for all contexts (`BiDi` multiplexes natively)
/// - Broadcast channel for events with method-based filtering at receive site
pub(crate) struct BidiTransport {
  next_id: AtomicU64,
  pending: Arc<PendingMap>,
  write_tx: mpsc::Sender<Message>,
  event_tx: Arc<arc_swap::ArcSwapOption<broadcast::Sender<BidiEvent>>>,
  /// Lossless taps fed by the reader in wire order before the broadcast
  /// fanout. State-mutating consumers (frame cache, network tracker,
  /// route interception) use these; a broadcast `Lagged` drop there
  /// corrupts tracker state, see the CDP dispatcher's tap rationale.
  event_taps: Arc<std::sync::Mutex<Vec<mpsc::UnboundedSender<TapMessage>>>>,
  /// Set by [`Self::start_close`] before the browser process is killed.
  /// Firefox dies without sending a WebSocket close frame, so the
  /// reader sees a TCP reset (`ResetWithoutClosingHandshake`) — during
  /// an intentional shutdown that's expected teardown, not an error
  /// worth a WARN.
  closing: Arc<AtomicBool>,
  tasks: crate::backend::transport_tasks::TransportTasks,
}

impl Drop for BidiTransport {
  fn drop(&mut self) {
    self.start_close();
  }
}

fn trace_event(event: &BidiEvent) {
  trace!(
    method = event.method,
    context = ?event.params.get("context"),
    request = ?event.params.get("request").and_then(|request| request.get("request")),
    status = ?event.params.get("response").and_then(|response| response.get("status")),
    "BiDi event"
  );
}

fn websocket_request(
  ws_url: &str,
  headers: Option<&rustc_hash::FxHashMap<String, String>>,
) -> Result<tokio_tungstenite::tungstenite::handshake::client::Request> {
  use tokio_tungstenite::tungstenite::client::IntoClientRequest;
  let mut request = ws_url
    .into_client_request()
    .map_err(|e| FerriError::backend(format!("BiDi WebSocket request failed: {e}")))?;
  if let Some(headers) = headers {
    for (name, value) in headers {
      let name = reqwest::header::HeaderName::try_from(name)
        .map_err(|e| FerriError::invalid_argument("headers", format!("invalid header name: {e}")))?;
      let value = reqwest::header::HeaderValue::try_from(value)
        .map_err(|e| FerriError::invalid_argument("headers", format!("invalid header value: {e}")))?;
      request.headers_mut().insert(name, value);
    }
  }
  Ok(request)
}

async fn connect_socket(
  ws_url: &str,
  headers: Option<&rustc_hash::FxHashMap<String, String>>,
) -> Result<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>> {
  let request = websocket_request(ws_url, headers)?;
  let budget = crate::operation_budget::OperationBudget::current(30_000)?;
  let (socket, _) = budget
    .wait(Box::pin(tokio_tungstenite::connect_async(request)))
    .await
    .map_err(|error| error.error("connecting BiDi WebSocket"))?
    .map_err(|error| FerriError::backend(format!("BiDi WebSocket connection failed: {error}")))?;
  Ok(socket)
}

impl BidiTransport {
  /// Connect to a `BiDi` WebSocket endpoint.
  pub async fn connect(ws_url: &str) -> Result<Self> {
    Self::connect_with_headers(ws_url, None).await
  }

  pub async fn connect_with_headers(
    ws_url: &str,
    headers: Option<&rustc_hash::FxHashMap<String, String>>,
  ) -> Result<Self> {
    debug!("BiDi connecting");
    let (write, read) = connect_socket(ws_url, headers).await?.split();
    let pending: Arc<PendingMap> = Arc::new(DashMap::default());

    // Writer task
    let (write_tx, write_rx) = mpsc::channel::<Message>(128);
    let (shutdown, stopping) = tokio::sync::watch::channel(false);
    let writer = tokio::spawn(crate::backend::transport_tasks::write_websocket(
      write, write_rx, stopping,
    ));

    // Event broadcast channel — sized to absorb a worst-case page
    // load fan-out so slow subscribers don't get `RecvError::Lagged`
    // (which would, e.g., make the frame-cache listener miss a
    // `browsingContext.contextDestroyed` and leak stale frames). See
    // `EVENT_BROADCAST_CAPACITY` in the CDP transport for the same
    // rationale.
    let (event_tx, _) = broadcast::channel::<BidiEvent>(4096);
    let event_tx = Arc::new(arc_swap::ArcSwapOption::from(Some(Arc::new(event_tx))));
    let event_tx2 = Arc::clone(&event_tx);
    let event_taps: Arc<std::sync::Mutex<Vec<mpsc::UnboundedSender<TapMessage>>>> =
      Arc::new(std::sync::Mutex::new(Vec::new()));
    let event_taps2 = Arc::clone(&event_taps);

    let closing = Arc::new(AtomicBool::new(false));
    let closing2 = Arc::clone(&closing);

    // Reader task -- hot path uses json_scan for zero-alloc field extraction
    let pending2 = pending.clone();
    let reader_ended = shutdown.clone();
    let reader = tokio::spawn(async move {
      let mut read = read;
      while let Some(result) = read.next().await {
        let msg = match result {
          Ok(m) => m,
          Err(e) => {
            if closing2.load(Ordering::Relaxed) {
              debug!("BiDi WebSocket ended during shutdown: {e:?}");
            } else {
              warn!("BiDi WebSocket error: {e:?}");
            }
            break;
          },
        };
        let text = match msg {
          Message::Text(t) => t,
          Message::Close(frame) => {
            debug!("BiDi WebSocket close frame: {frame:?}");
            break;
          },
          _ => continue,
        };
        if closing2.load(Ordering::Acquire) {
          continue;
        }
        let bytes = text.as_bytes();

        // Hot path: extract "type" field without full parse
        let type_field = json_scan::json_string(json_scan::json_field(bytes, b"type"));

        if type_field == b"success" || type_field == b"error" {
          handle_command_response(bytes, type_field, &pending2);
        } else if type_field == b"event" {
          // Event -- extract method and params, broadcast
          let method_bytes = json_scan::json_string(json_scan::json_field(bytes, b"method"));
          if method_bytes.is_empty() {
            continue;
          }
          let method = String::from_utf8_lossy(method_bytes).to_string();

          // Full-parse only for events (we need the params)
          match serde_json::from_slice::<serde_json::Value>(bytes) {
            Ok(parsed) => {
              let params = parsed.get("params").cloned().unwrap_or(serde_json::Value::Null);
              let event = BidiEvent { method, params };
              trace_event(&event);
              // Taps first: lossless state trackers must observe the
              // event before best-effort broadcast consumers.
              {
                let mut taps = event_taps2.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                taps.retain(|tap| tap.send(TapMessage::Event(event.clone())).is_ok());
              }
              if let Some(sender) = event_tx2.load().as_ref() {
                let _ = sender.send(event);
              }
            },
            Err(e) => {
              warn!("BiDi event parse error: {e}");
            },
          }
        }
        // else: ignore unknown message types
      }
      // WebSocket closed — drain pending oneshots so in-flight
      // `send_command` awaits return immediately with a `target_closed`
      // error instead of waiting the full 60s response timeout.
      // Mirrors the CDP pipe/ws reader fix.
      closing2.store(true, Ordering::Release);
      event_tx2.store(None);
      event_taps2
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
      fail_pending(&pending2);
      reader_ended.send_replace(true);
      debug!("BiDi reader task ended");
    });

    debug!("BiDi transport connected");
    Ok(Self {
      next_id: AtomicU64::new(0),
      pending,
      write_tx,
      event_tx,
      event_taps,
      closing,
      tasks: crate::backend::transport_tasks::TransportTasks::new(shutdown, vec![reader, writer]),
    })
  }

  /// Reject commands and request a close handshake independently of queue capacity.
  /// This also marks expected process termination for the reader's diagnostics.
  pub fn start_close(&self) {
    self.closing.store(true, Ordering::Release);
    self.event_tx.store(None);
    self
      .event_taps
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clear();
    fail_pending(&self.pending);
    self.tasks.start_close();
  }

  pub async fn close(&self) -> Result<()> {
    self.start_close();
    self.tasks.close().await
  }

  /// Send a `BiDi` command and await the response.
  pub async fn send_command(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
    let budget = crate::operation_budget::OperationBudget::current(60_000)?;
    let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
    let (tx, rx) = oneshot::channel();

    // Register pending before sending (avoid race)
    self.pending.insert(id, tx);
    let _pending = PendingCommand {
      pending: &self.pending,
      id,
    };
    if self.closing.load(Ordering::Acquire) {
      return Err(FerriError::target_closed(Some(
        "BiDi WebSocket connection closed".into(),
      )));
    }

    // Build command JSON directly as string (no Value intermediary for envelope)
    let params_str = serde_json::to_string(&params)?;
    let cmd = format!(r#"{{"id":{id},"method":"{method}","params":{params_str}}}"#);
    trace!("BiDi send id={id}: {method}");

    budget.send(&self.write_tx, Message::Text(cmd.into())).await?;
    let result = budget
      .wait(rx)
      .await
      .map_err(|error| error.error(format!("BiDi command '{method}'")))?
      .map_err(|_| FerriError::backend("BiDi command response channel dropped"))?;
    result.map_err(|e| {
      if matches!(e.error.as_str(), "no such frame" | "target closed") {
        FerriError::target_closed(Some(e.to_string()))
      } else if matches!(e.error.as_str(), "unknown command" | "unsupported operation") {
        FerriError::unsupported(format!("{method}: {e}"))
      } else {
        FerriError::protocol(method, e.to_string())
      }
    })
  }

  /// Subscribe to `BiDi` events. Returns a broadcast receiver.
  /// Receivers filter by event method at the receive site.
  pub fn subscribe_events(&self) -> broadcast::Receiver<BidiEvent> {
    self
      .event_tx
      .load()
      .as_ref()
      .map_or_else(|| broadcast::channel(1).1, |sender| sender.subscribe())
  }

  /// Lossless, wire-ordered event tap. Never drops; consumers filter by
  /// method at the receive site. State-mutating consumers MUST use this
  /// instead of [`Self::subscribe_events`].
  pub fn tap_events(&self) -> EventTap {
    let (tx, rx) = EventTap::new();
    let mut taps = self
      .event_taps
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !self.closing.load(Ordering::Acquire) {
      taps.push(tx);
    }
    rx
  }
}

/// Process a `BiDi` command response (success or error) by correlating with the pending map.
fn handle_command_response(bytes: &[u8], type_field: &[u8], pending: &PendingMap) {
  let id = json_scan::json_id(bytes);
  if id == 0 {
    warn!("BiDi response missing id");
    return;
  }

  let tx = pending.remove(&id).map(|(_, tx)| tx);

  let Some(tx) = tx else {
    trace!("BiDi response for unknown id={id}");
    return;
  };

  if type_field == b"error" {
    let error_str = json_scan::json_string(json_scan::json_field(bytes, b"error"));
    let message_str = json_scan::json_string(json_scan::json_field(bytes, b"message"));
    let error = String::from_utf8_lossy(error_str).to_string();
    let message = String::from_utf8_lossy(message_str).to_string();
    trace!("BiDi error id={id}: {error} - {message}");
    let _ = tx.send(Err(BidiError { error, message }));
  } else {
    match serde_json::from_slice::<serde_json::Value>(bytes) {
      Ok(parsed) => {
        let result = parsed.get("result").cloned().unwrap_or(serde_json::Value::Null);
        trace!("BiDi response id={id}");
        let _ = tx.send(Ok(result));
      },
      Err(e) => {
        warn!("BiDi parse error id={id}: {e}");
        let _ = tx.send(Err(BidiError {
          error: "parse_error".into(),
          message: e.to_string(),
        }));
      },
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn close_bypasses_a_full_command_queue() {
    let (write_tx, queued) = mpsc::channel(1);
    write_tx.send(Message::Text("already queued".into())).await.unwrap();
    let (shutdown, mut stopping) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(async move {
      stopping.wait_for(|stopping| *stopping).await.unwrap();
      assert_eq!(queued.len(), 1);
      drop(queued);
    });
    let transport = BidiTransport {
      next_id: AtomicU64::new(0),
      pending: Arc::new(DashMap::default()),
      write_tx,
      event_tx: Arc::new(arc_swap::ArcSwapOption::from(Some(Arc::new(broadcast::channel(1).0)))),
      event_taps: Arc::new(std::sync::Mutex::new(Vec::new())),
      closing: Arc::new(AtomicBool::new(false)),
      tasks: crate::backend::transport_tasks::TransportTasks::new(shutdown, vec![worker]),
    };
    let mut events = transport.subscribe_events();
    let mut tap = transport.tap_events();
    let mut pending = Box::pin(
      crate::operation_budget::OperationBudget::new(0)
        .unwrap()
        .scope(transport.send_command("session.status", serde_json::json!({}))),
    );
    assert!(futures::poll!(&mut pending).is_pending());
    transport.close().await.unwrap();
    assert!(matches!(pending.await, Err(FerriError::TargetClosed { .. })));
    assert!(transport.pending.is_empty());
    assert!(matches!(events.try_recv(), Err(broadcast::error::TryRecvError::Closed)));
    assert!(matches!(
      tap.receiver.try_recv(),
      Err(mpsc::error::TryRecvError::Disconnected)
    ));
    assert!(matches!(
      transport.subscribe_events().try_recv(),
      Err(broadcast::error::TryRecvError::Closed)
    ));
    assert!(matches!(
      transport.tap_events().receiver.try_recv(),
      Err(mpsc::error::TryRecvError::Disconnected)
    ));
  }

  #[tokio::test]
  async fn close_releases_the_socket_and_unbounded_pending_commands_with_handles_retained() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    let (received, receipt) = oneshot::channel();
    let server = tokio::spawn(async move {
      let (stream, _) = listener.accept().await.unwrap();
      let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
      let request = socket.next().await.unwrap().unwrap();
      let request: serde_json::Value = serde_json::from_str(request.to_text().unwrap()).unwrap();
      assert_eq!(request["method"], "session.status");
      received.send(()).unwrap();
      while let Some(Ok(message)) = socket.next().await {
        if message.is_close() {
          break;
        }
        assert!(!message.is_text(), "close sent another protocol command");
      }
    });
    let transport = BidiTransport::connect(&endpoint).await.unwrap();
    let mut pending = Box::pin(
      crate::operation_budget::OperationBudget::new(0)
        .unwrap()
        .scope(transport.send_command("session.status", serde_json::json!({}))),
    );
    assert!(futures::poll!(&mut pending).is_pending());
    receipt.await.unwrap();
    transport.close().await.unwrap();
    assert!(matches!(pending.await, Err(FerriError::TargetClosed { .. })));
    assert!(matches!(
      transport.send_command("session.status", serde_json::json!({})).await,
      Err(FerriError::TargetClosed { .. })
    ));
    assert!(transport.pending.is_empty());
    tokio::time::timeout(std::time::Duration::from_secs(1), server)
      .await
      .unwrap()
      .unwrap();
  }

  #[tokio::test(start_paused = true)]
  async fn operation_budgets_cover_long_replies_and_queued_commands() {
    use crate::operation_budget::OperationBudget;
    let (write_tx, mut queued) = mpsc::channel(1);
    let transport = BidiTransport {
      next_id: AtomicU64::new(0),
      pending: Arc::new(DashMap::default()),
      write_tx,
      event_tx: Arc::new(arc_swap::ArcSwapOption::from(Some(Arc::new(broadcast::channel(1).0)))),
      event_taps: Arc::new(std::sync::Mutex::new(Vec::new())),
      closing: Arc::new(AtomicBool::new(false)),
      tasks: crate::backend::transport_tasks::TransportTasks::new(tokio::sync::watch::channel(false).0, Vec::new()),
    };
    for timeout in [0, 90_000] {
      let mut command = Box::pin(
        OperationBudget::new(timeout)
          .unwrap()
          .scope(transport.send_command("script.callFunction", serde_json::json!({}))),
      );
      assert!(futures::poll!(&mut command).is_pending());
      let message: serde_json::Value = serde_json::from_str(queued.recv().await.unwrap().to_text().unwrap()).unwrap();
      tokio::time::advance(std::time::Duration::from_secs(61)).await;
      assert!(futures::poll!(&mut command).is_pending());
      handle_command_response(
        serde_json::json!({"type":"success","id":message["id"],"result":{"value":42}})
          .to_string()
          .as_bytes(),
        b"success",
        &transport.pending,
      );
      assert_eq!(command.await.unwrap()["value"], 42);
      assert!(transport.pending.is_empty());
    }
    transport.write_tx.send(Message::Text("queued".into())).await.unwrap();
    let mut command = Box::pin(
      OperationBudget::new(1000)
        .unwrap()
        .scope(transport.send_command("script.callFunction", serde_json::json!({}))),
    );
    assert!(futures::poll!(&mut command).is_pending());
    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    assert!(command.await.unwrap_err().is_timeout_error());
    assert!(transport.pending.is_empty());
    assert!(queued.recv().await.is_some());
    assert!(queued.try_recv().is_err());
  }

  #[tokio::test]
  async fn cancelled_command_releases_its_pending_response_slot() {
    let (write_tx, mut write_rx) = mpsc::channel(1);
    let transport = BidiTransport {
      next_id: AtomicU64::new(0),
      pending: Arc::new(DashMap::default()),
      write_tx,
      event_tx: Arc::new(arc_swap::ArcSwapOption::from(Some(Arc::new(broadcast::channel(1).0)))),
      event_taps: Arc::new(std::sync::Mutex::new(Vec::new())),
      closing: Arc::new(AtomicBool::new(false)),
      tasks: crate::backend::transport_tasks::TransportTasks::new(tokio::sync::watch::channel(false).0, Vec::new()),
    };
    let mut command = Box::pin(transport.send_command("session.status", serde_json::json!({})));
    assert!(futures::poll!(&mut command).is_pending());
    assert!(write_rx.recv().await.unwrap().is_text());
    assert_eq!(transport.pending.len(), 1);
    drop(command);
    assert!(
      transport.pending.is_empty(),
      "cancelled commands accumulate response slots"
    );
  }

  #[tokio::test]
  async fn event_barrier_waits_for_the_consumer_to_process_queued_responses() {
    let (sender, mut tap) = EventTap::new();
    for status in [401, 200] {
      assert!(
        sender
          .send(TapMessage::Event(BidiEvent {
            method: "network.responseStarted".into(),
            params: serde_json::json!({ "response": { "status": status } }),
          }))
          .is_ok()
      );
    }
    let Some(challenge) = tap.recv().await else {
      panic!("missing challenge")
    };
    assert_eq!(challenge.params["response"]["status"], 401);
    let barrier = tap.barrier();
    let waiting = barrier.wait();
    tokio::pin!(waiting);
    assert!(futures::poll!(&mut waiting).is_pending());
    let Some(authenticated) = tap.recv().await else {
      panic!("missing authenticated response")
    };
    assert_eq!(authenticated.params["response"]["status"], 200);
    assert!(futures::poll!(&mut waiting).is_pending());
    let next = tap.recv();
    tokio::pin!(next);
    assert!(futures::poll!(&mut next).is_pending());
    assert!(waiting.await.is_ok());
  }

  #[tokio::test]
  async fn event_barrier_reports_a_stopped_consumer() {
    let (_sender, tap) = EventTap::new();
    let barrier = tap.barrier();
    drop(tap);
    assert!(barrier.wait().await.is_err());
  }
}
