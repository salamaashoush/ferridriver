//! WebSocket transport for `WebDriver` `BiDi` protocol.
//!
//! Handles connection, command/response correlation, and event dispatch.
//! Uses `json_scan` for zero-allocation hot-path field extraction (same as CDP).

use futures::{SinkExt, StreamExt};
use rustc_hash::FxHashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, trace, warn};

use crate::backend::json_scan;

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

type BidiResult = Result<serde_json::Value, BidiError>;

/// A `BiDi` event received from the browser.
#[derive(Debug, Clone)]
pub(crate) struct BidiEvent {
  /// Event method name, e.g. "browsingContext.load"
  pub method: String,
  /// Raw event params
  pub params: serde_json::Value,
}

/// Pending command map: command ID -> oneshot sender for the response.
type PendingMap = FxHashMap<u64, oneshot::Sender<BidiResult>>;

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
  pending: Arc<std::sync::Mutex<PendingMap>>,
  write_tx: mpsc::Sender<Message>,
  event_tx: broadcast::Sender<BidiEvent>,
}

impl BidiTransport {
  /// Connect to a `BiDi` WebSocket endpoint.
  pub async fn connect(ws_url: &str) -> Result<Self, String> {
    debug!("BiDi connecting to {ws_url}");

    let (ws_stream, _) = tokio_tungstenite::connect_async(ws_url)
      .await
      .map_err(|e| format!("BiDi WebSocket connect to {ws_url}: {e}"))?;

    let (write, read) = ws_stream.split();
    let pending: Arc<std::sync::Mutex<PendingMap>> = Arc::new(std::sync::Mutex::new(FxHashMap::default()));

    // Writer task
    let (write_tx, mut write_rx) = mpsc::channel::<Message>(128);
    tokio::spawn(async move {
      let mut writer = write;
      while let Some(msg) = write_rx.recv().await {
        if writer.send(msg).await.is_err() {
          break;
        }
      }
    });

    // Event broadcast channel (256 buffer -- events are filtered by receivers)
    let (event_tx, _) = broadcast::channel::<BidiEvent>(256);
    let event_tx2 = event_tx.clone();

    // Reader task -- hot path uses json_scan for zero-alloc field extraction
    let pending2 = pending.clone();
    tokio::spawn(async move {
      let mut read = read;
      while let Some(result) = read.next().await {
        let msg = match result {
          Ok(m) => m,
          Err(e) => {
            warn!("BiDi WebSocket error: {e:?}");
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
              trace!("BiDi event: {method}");
              let _ = event_tx2.send(BidiEvent { method, params });
            },
            Err(e) => {
              warn!("BiDi event parse error: {e}");
            },
          }
        }
        // else: ignore unknown message types
      }
      debug!("BiDi reader task ended");
    });

    debug!("BiDi transport connected");
    Ok(Self {
      next_id: AtomicU64::new(0),
      pending,
      write_tx,
      event_tx,
    })
  }

  /// Send a `BiDi` command and await the response.
  pub async fn send_command(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
    let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
    let (tx, rx) = oneshot::channel();

    // Register pending before sending (avoid race)
    {
      let mut map = self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      map.insert(id, tx);
    }

    // Build command JSON directly as string (no Value intermediary for envelope)
    let params_str = serde_json::to_string(&params).unwrap_or_else(|_| "{}".to_string());
    let cmd = format!(r#"{{"id":{id},"method":"{method}","params":{params_str}}}"#);
    trace!("BiDi send id={id}: {method}");

    if self.write_tx.send(Message::Text(cmd.into())).await.is_err() {
      let mut map = self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      map.remove(&id);
      return Err("BiDi WebSocket connection closed".into());
    }

    // Await response with timeout
    match tokio::time::timeout(std::time::Duration::from_secs(60), rx).await {
      Ok(Ok(result)) => result.map_err(|e| e.to_string()),
      Ok(Err(_)) => Err("BiDi command response channel dropped".into()),
      Err(_) => {
        let mut map = self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        map.remove(&id);
        Err(format!("BiDi command '{method}' timed out after 60s"))
      },
    }
  }

  /// Send multiple commands in parallel and await all responses.
  #[allow(dead_code)]
  /// Commands are written to the channel sequentially (they batch naturally),
  /// then all responses are awaited concurrently.
  pub async fn send_batch(&self, commands: &[(&str, serde_json::Value)]) -> Vec<Result<serde_json::Value, String>> {
    let mut receivers = Vec::with_capacity(commands.len());

    for (method, params) in commands {
      let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
      let (tx, rx) = oneshot::channel();

      {
        let mut map = self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        map.insert(id, tx);
      }

      let params_str = serde_json::to_string(params).unwrap_or_else(|_| "{}".to_string());
      let cmd = format!(r#"{{"id":{id},"method":"{method}","params":{params_str}}}"#);
      trace!("BiDi batch send id={id}: {method}");

      if self.write_tx.send(Message::Text(cmd.into())).await.is_err() {
        receivers.push(Err("BiDi WebSocket connection closed".into()));
        continue;
      }

      receivers.push(Ok(rx));
    }

    let mut results = Vec::with_capacity(receivers.len());
    for recv in receivers {
      match recv {
        Ok(rx) => match tokio::time::timeout(std::time::Duration::from_secs(60), rx).await {
          Ok(Ok(result)) => results.push(result.map_err(|e| e.to_string())),
          Ok(Err(_)) => results.push(Err("BiDi batch response channel dropped".into())),
          Err(_) => results.push(Err("BiDi batch command timed out after 60s".into())),
        },
        Err(e) => results.push(Err(e)),
      }
    }

    results
  }

  /// Subscribe to `BiDi` events. Returns a broadcast receiver.
  /// Receivers filter by event method at the receive site.
  pub fn subscribe_events(&self) -> broadcast::Receiver<BidiEvent> {
    self.event_tx.subscribe()
  }
}

/// Process a `BiDi` command response (success or error) by correlating with the pending map.
fn handle_command_response(bytes: &[u8], type_field: &[u8], pending: &Arc<std::sync::Mutex<PendingMap>>) {
  let id = json_scan::json_id(bytes);
  if id == 0 {
    warn!("BiDi response missing id");
    return;
  }

  let tx = {
    let mut map = pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    map.remove(&id)
  };

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
