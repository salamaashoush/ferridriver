//! WebSocket transport for CDP -- high-performance, fully parallel.
//!
//! Same architecture as `cdp_pipe/transport.rs` but over WebSocket:
//! - Oneshot channels for response correlation (no handler bottleneck)
//! - Broadcast channel for events (console, network, dialog)
//! - Navigation waiters (register before navigate, resolve on loadEventFired)
//! - Channel-based writer (decouples sender from I/O, enables batching)
//! - std::sync::Mutex for maps (never held across await, zero futex overhead)

use futures::{SinkExt, StreamExt};
use rustc_hash::FxHashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{broadcast, oneshot};
use tokio_tungstenite::tungstenite::Message;

/// std::sync::Mutex — never held across await, zero futex overhead on uncontended path.
type PendingMap = Arc<std::sync::Mutex<FxHashMap<u64, oneshot::Sender<Result<serde_json::Value, String>>>>>;
struct NavWaiter {
  target: crate::backend::NavLifecycle,
  tx: oneshot::Sender<Result<(), String>>,
}
type NavWaiterMap = Arc<std::sync::Mutex<FxHashMap<String, NavWaiter>>>;

pub struct WsTransport {
  /// Channel sender for dedicated writer task (same pattern as cdp_pipe).
  write_tx: tokio::sync::mpsc::Sender<Message>,
  next_id: AtomicU64,
  pending: PendingMap,
  nav_waiters: NavWaiterMap,
  event_tx: broadcast::Sender<serde_json::Value>,
}

impl WsTransport {
  /// Connect to an existing Chrome `DevTools` WebSocket endpoint.
  pub async fn connect(ws_url: &str) -> Result<Self, String> {
    let (ws_stream, _) = tokio_tungstenite::connect_async(ws_url)
      .await
      .map_err(|e| format!("WebSocket connect to {ws_url}: {e}"))?;

    let (write, read) = ws_stream.split();
    let (event_tx, _) = broadcast::channel(256);

    let pending: PendingMap = Arc::new(std::sync::Mutex::new(FxHashMap::default()));
    let nav_waiters: NavWaiterMap = Arc::new(std::sync::Mutex::new(FxHashMap::default()));

    // Spawn dedicated writer task.
    let (write_tx, mut write_rx) = tokio::sync::mpsc::channel::<Message>(64);
    tokio::spawn(async move {
      let mut writer = write;
      while let Some(msg) = write_rx.recv().await {
        if writer.send(msg).await.is_err() {
          break;
        }
      }
    });

    let pending2 = pending.clone();
    let nav_waiters2 = nav_waiters.clone();
    let event_tx2 = event_tx.clone();

    // Reader task: uses json_scan for zero-alloc dispatch (same as cdp_pipe).
    // Full serde parse only for broadcast to event subscribers.
    tokio::spawn(async move {
      use crate::backend::json_scan;

      let mut read = read;
      while let Some(Ok(msg)) = read.next().await {
        let Message::Text(text) = msg else { continue };
        let raw = text.as_bytes();

        let id = json_scan::json_id(raw);

        if id > 0 {
          // Response — parse only the result or error field, not the whole message.
          let error_field = json_scan::json_field(raw, b"error");
          let payload = if error_field.is_empty() {
            let result_field = json_scan::json_field(raw, b"result");
            if result_field.is_empty() {
              Ok(serde_json::Value::Object(serde_json::Map::new()))
            } else {
              serde_json::from_slice::<serde_json::Value>(result_field)
                .map_err(|e| format!("parse result: {e}"))
            }
          } else {
            let msg_bytes = json_scan::error_message(error_field);
            let msg_str = std::str::from_utf8(msg_bytes).unwrap_or("CDP error");
            Err(msg_str.to_string())
          };

          if let Some(tx) = pending2.lock().unwrap().remove(&id) {
            let _ = tx.send(payload);
          }
        } else {
          // Event — scan method and sessionId without full parse.
          let method = json_scan::json_string(json_scan::json_field(raw, b"method"));
          let session_id = json_scan::json_string(json_scan::json_field(raw, b"sessionId"));
          let method_str = std::str::from_utf8(method).unwrap_or("");
          let sid_str = std::str::from_utf8(session_id).unwrap_or("");

          // Single lock for all navigation event dispatch.
          {
            use crate::backend::NavLifecycle;
            let key = sid_str.to_string();
            let mut waiters = nav_waiters2.lock().unwrap();
            if method_str == "Page.frameNavigated" {
              if matches!(waiters.get(&key).map(|w| w.target), Some(NavLifecycle::Commit)) {
                if let Some(w) = waiters.remove(&key) { let _ = w.tx.send(Ok(())); }
              }
            } else if method_str == "Page.lifecycleEvent" {
              let params = json_scan::json_field(raw, b"params");
              let name = json_scan::json_string(json_scan::json_field(params, b"name"));
              let name_str = std::str::from_utf8(name).unwrap_or("");
              let resolve = matches!(
                (name_str, waiters.get(&key).map(|w| w.target)),
                ("DOMContentLoaded", Some(NavLifecycle::DomContentLoaded))
                | ("load", Some(NavLifecycle::Load | NavLifecycle::DomContentLoaded))
              );
              if resolve {
                if let Some(w) = waiters.remove(&key) { let _ = w.tx.send(Ok(())); }
              }
            } else if method_str == "Page.loadEventFired" {
              if matches!(waiters.get(&key).map(|w| w.target), Some(NavLifecycle::Load | NavLifecycle::DomContentLoaded)) {
                if let Some(w) = waiters.remove(&key) { let _ = w.tx.send(Ok(())); }
              }
            } else if method_str == "Page.domContentEventFired" {
              if matches!(waiters.get(&key).map(|w| w.target), Some(NavLifecycle::DomContentLoaded)) {
                if let Some(w) = waiters.remove(&key) { let _ = w.tx.send(Ok(())); }
              }
            } else if method_str == "Inspector.targetCrashed" {
              if let Some(w) = waiters.remove(&key) {
                let _ = w.tx.send(Err("Target crashed".into()));
              }
            }
          }

          // Full parse only for broadcast to event subscribers.
          if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
            let _ = event_tx2.send(json);
          }
        }
      }
    });

    Ok(Self {
      write_tx,
      next_id: AtomicU64::new(1),
      pending,
      nav_waiters,
      event_tx,
    })
  }

  /// Launch Chrome with --remote-debugging-port=0 and connect to its WebSocket.
  pub async fn spawn(
    chromium_path: &str,
    user_data_dir: &Path,
    extra_flags: &[String],
  ) -> Result<(Self, tokio::process::Child), String> {
    let mut command = tokio::process::Command::new(chromium_path);
    command.arg(format!("--user-data-dir={}", user_data_dir.display()));
    command.arg("--remote-debugging-port=0");

    for flag in extra_flags {
      command.arg(flag);
    }

    command.arg("--no-startup-window");
    command
      .stdin(std::process::Stdio::null())
      .stdout(std::process::Stdio::null())
      .stderr(std::process::Stdio::piped());

    let mut child = command.spawn().map_err(|e| format!("Chrome launch: {e}"))?;

    let port_file = user_data_dir.join("DevToolsActivePort");
    let ws_url = discover_ws_url(&port_file, &mut child).await?;

    let transport = Self::connect(&ws_url).await?;
    Ok((transport, child))
  }

  /// Send a CDP command and await the response.
  pub async fn send_command(
    &self,
    session_id: Option<&str>,
    method: &str,
    params: serde_json::Value,
  ) -> Result<serde_json::Value, String> {
    let id = self.next_id.fetch_add(1, Ordering::Relaxed);

    let params_str = serde_json::to_string(&params).map_err(|e| format!("Serialize: {e}"))?;
    let text = if let Some(sid) = session_id {
      format!(r#"{{"id":{id},"method":"{method}","params":{params_str},"sessionId":"{sid}"}}"#)
    } else {
      format!(r#"{{"id":{id},"method":"{method}","params":{params_str}}}"#)
    };

    let (tx, rx) = oneshot::channel();
    self.pending.lock().unwrap().insert(id, tx);

    // Push to writer task (~20-50ns, non-blocking).
    self
      .write_tx
      .send(Message::Text(text))
      .await
      .map_err(|_| "WS writer closed".to_string())?;

    match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
      Ok(Ok(result)) => result,
      Ok(Err(_)) => Err("Channel closed".into()),
      Err(_) => {
        self.pending.lock().unwrap().remove(&id);
        Err("Timeout (30s)".into())
      }
    }
  }

  /// Register a navigation waiter for a session with a lifecycle target.
  pub fn register_nav_waiter(
    &self,
    session_id: &str,
    target: crate::backend::NavLifecycle,
  ) -> oneshot::Receiver<Result<(), String>> {
    let (tx, rx) = oneshot::channel();
    self.nav_waiters.lock().unwrap().insert(session_id.to_string(), NavWaiter { target, tx });
    rx
  }

  /// Subscribe to CDP events (console, network, dialog, etc.).
  pub fn subscribe_events(&self) -> broadcast::Receiver<serde_json::Value> {
    self.event_tx.subscribe()
  }
}

/// Discover Chrome's `DevTools` WebSocket URL by reading the `DevToolsActivePort` file.
async fn discover_ws_url(port_file: &Path, child: &mut tokio::process::Child) -> Result<String, String> {
  // Poll for DevToolsActivePort file (Chrome writes it after binding the port)
  for _ in 0..200 {
    if let Ok(contents) = tokio::fs::read_to_string(port_file).await {
      let lines: Vec<&str> = contents.lines().collect();
      if lines.len() >= 2 {
        let port = lines[0].trim();
        let path = lines[1].trim();
        return Ok(format!("ws://127.0.0.1:{port}{path}"));
      }
    }
    // Check if Chrome crashed
    if let Some(status) = child.try_wait().map_err(|e| format!("wait: {e}"))? {
      return Err(format!("Chrome exited with status {status} before DevToolsActivePort was written"));
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
  }
  Err("Timed out waiting for DevToolsActivePort".into())
}
