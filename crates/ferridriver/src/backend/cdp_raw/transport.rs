//! WebSocket transport for CDP -- high-performance, fully parallel.
//!
//! Same architecture as cdp_pipe/transport.rs but over WebSocket:
//! - Oneshot channels for response correlation (no handler bottleneck)
//! - Broadcast channel for events (console, network, dialog)
//! - Navigation waiters (register before navigate, resolve on loadEventFired)
//! - Multiple send_command calls can be in-flight simultaneously

use futures::{SinkExt, StreamExt};
use rustc_hash::FxHashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, oneshot, Mutex};
use tokio_tungstenite::tungstenite::Message;

type WsSink = futures::stream::SplitSink<
  tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
  Message,
>;

pub struct WsTransport {
  writer: Mutex<WsSink>,
  next_id: AtomicU64,
  pending: Arc<Mutex<FxHashMap<u64, oneshot::Sender<Result<serde_json::Value, String>>>>>,
  nav_waiters: Arc<Mutex<FxHashMap<String, oneshot::Sender<Result<(), String>>>>>,
  event_tx: broadcast::Sender<serde_json::Value>,
}

impl WsTransport {
  /// Connect to an existing Chrome DevTools WebSocket endpoint.
  pub async fn connect(ws_url: &str) -> Result<Self, String> {
    let (ws_stream, _) = tokio_tungstenite::connect_async(ws_url)
      .await
      .map_err(|e| format!("WebSocket connect to {ws_url}: {e}"))?;

    let (write, read) = ws_stream.split();
    let (event_tx, _) = broadcast::channel(256);

    let pending: Arc<Mutex<FxHashMap<u64, oneshot::Sender<Result<serde_json::Value, String>>>>> =
      Arc::new(Mutex::new(FxHashMap::default()));
    let nav_waiters: Arc<Mutex<FxHashMap<String, oneshot::Sender<Result<(), String>>>>> =
      Arc::new(Mutex::new(FxHashMap::default()));

    let pending2 = pending.clone();
    let nav_waiters2 = nav_waiters.clone();
    let event_tx2 = event_tx.clone();

    // Reader task: process WebSocket messages
    tokio::spawn(async move {
      let mut read = read;
      while let Some(Ok(msg)) = read.next().await {
        let text = match msg {
          Message::Text(t) => t,
          _ => continue,
        };

        let json: serde_json::Value = match serde_json::from_str(&text) {
          Ok(v) => v,
          Err(_) => continue,
        };

        let id = json.get("id").and_then(|v| v.as_u64()).unwrap_or(0);

        if id > 0 {
          // Response to a command
          let result = if let Some(err) = json.get("error") {
            let msg = err
              .get("message")
              .and_then(|m| m.as_str())
              .unwrap_or("CDP error");
            Err(msg.to_string())
          } else {
            Ok(json.get("result").cloned().unwrap_or(serde_json::json!({})))
          };

          let mut pending = pending2.lock().await;
          if let Some(tx) = pending.remove(&id) {
            let _ = tx.send(result);
          }
        } else {
          // Event (no id)
          let method = json
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("");
          let session_id = json
            .get("sessionId")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();

          // Playwright approach: resolve on Page.lifecycleEvent DOMContentLoaded/load
          if method == "Page.lifecycleEvent" {
            if let Some(name) = json.get("params").and_then(|p| p.get("name")).and_then(|n| n.as_str()) {
              if name == "DOMContentLoaded" || name == "load" {
                let mut waiters = nav_waiters2.lock().await;
                if let Some(tx) = waiters.remove(&session_id) {
                  let _ = tx.send(Ok(()));
                }
              }
            }
          } else if method == "Page.domContentEventFired" || method == "Page.loadEventFired" {
            let mut waiters = nav_waiters2.lock().await;
            if let Some(tx) = waiters.remove(&session_id) {
              let _ = tx.send(Ok(()));
            }
          } else if method == "Inspector.targetCrashed" {
            let mut waiters = nav_waiters2.lock().await;
            if let Some(tx) = waiters.remove(&session_id) {
              let _ = tx.send(Err("Target crashed".into()));
            }
          }

          // Broadcast to all event subscribers
          let _ = event_tx2.send(json);
        }
      }
    });

    Ok(Self {
      writer: Mutex::new(write),
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
  ) -> Result<(Self, tokio::process::Child), String> {
    let mut command = tokio::process::Command::new(chromium_path);
    command.arg(format!("--user-data-dir={}", user_data_dir.display()));
    command.arg("--remote-debugging-port=0"); // auto-assign port

    for flag in crate::state::CHROME_FLAGS {
      command.arg(flag);
    }

    command.arg("--no-startup-window");
    command
      .stdin(std::process::Stdio::null())
      .stdout(std::process::Stdio::null())
      .stderr(std::process::Stdio::piped());

    let mut child = command
      .spawn()
      .map_err(|e| format!("Chrome launch: {e}"))?;

    // Discover WebSocket URL from DevToolsActivePort file
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

    // Build JSON directly, skip intermediate Value allocation.
    let params_str = serde_json::to_string(&params).map_err(|e| format!("Serialize: {e}"))?;
    let text = if let Some(sid) = session_id {
      format!(r#"{{"id":{id},"method":"{method}","params":{params_str},"sessionId":"{sid}"}}"#)
    } else {
      format!(r#"{{"id":{id},"method":"{method}","params":{params_str}}}"#)
    };

    let (tx, rx) = oneshot::channel();
    {
      let mut pending = self.pending.lock().await;
      pending.insert(id, tx);
    }

    {
      let mut writer = self.writer.lock().await;
      writer
        .send(Message::Text(text))
        .await
        .map_err(|e| format!("WS send: {e}"))?;
    }

    // Await response with timeout
    match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
      Ok(Ok(result)) => result,
      Ok(Err(_)) => Err("Channel closed".into()),
      Err(_) => {
        // Clean up pending on timeout
        self.pending.lock().await.remove(&id);
        Err("Timeout (30s)".into())
      }
    }
  }

  /// Register a navigation waiter for a session. Returns a receiver.
  pub async fn register_nav_waiter(
    &self,
    session_id: &str,
  ) -> oneshot::Receiver<Result<(), String>> {
    let (tx, rx) = oneshot::channel();
    self.nav_waiters.lock().await.insert(session_id.to_string(), tx);
    rx
  }

  /// Subscribe to CDP events (console, network, dialog, etc.).
  pub fn subscribe_events(&self) -> broadcast::Receiver<serde_json::Value> {
    self.event_tx.subscribe()
  }
}

/// Discover Chrome's DevTools WebSocket URL by reading the DevToolsActivePort file.
async fn discover_ws_url(
  port_file: &Path,
  child: &mut tokio::process::Child,
) -> Result<String, String> {
  // Poll for DevToolsActivePort file (Chrome writes it after binding the port)
  for _ in 0..200 {
    if let Ok(content) = tokio::fs::read_to_string(port_file).await {
      let lines: Vec<&str> = content.trim().lines().collect();
      if lines.len() >= 2 {
        let port = lines[0].trim();
        let path = lines[1].trim();
        return Ok(format!("ws://127.0.0.1:{port}{path}"));
      }
    }

    // Check if Chrome died
    if let Some(status) = child.try_wait().map_err(|e| format!("{e}"))? {
      return Err(format!("Chrome exited early with {status}"));
    }

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
  }

  Err("Timeout waiting for Chrome DevToolsActivePort (10s)".into())
}
