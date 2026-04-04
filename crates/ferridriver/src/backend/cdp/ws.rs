//! WebSocket transport for CDP — same dispatch logic as pipe, different I/O.
//!
//! All message dispatch (responses, nav waiters, lifecycle tracking, broadcast)
//! is handled by the shared CdpDispatcher. This file only implements WebSocket I/O.

use futures::{SinkExt, StreamExt};
use std::path::Path;
use std::sync::Arc;
use tokio_tungstenite::tungstenite::Message;

use super::transport::CdpDispatcher;

pub struct WsTransport {
  write_tx: tokio::sync::mpsc::Sender<Message>,
  dispatcher: Arc<CdpDispatcher>,
}

impl WsTransport {
  pub async fn connect(ws_url: &str) -> Result<Self, String> {
    let (ws_stream, _) = tokio_tungstenite::connect_async(ws_url)
      .await
      .map_err(|e| format!("WebSocket connect to {ws_url}: {e}"))?;

    let (write, read) = ws_stream.split();
    let dispatcher = Arc::new(CdpDispatcher::new());

    let (write_tx, mut write_rx) = tokio::sync::mpsc::channel::<Message>(64);
    tokio::spawn(async move {
      let mut writer = write;
      while let Some(msg) = write_rx.recv().await {
        if writer.send(msg).await.is_err() {
          break;
        }
      }
    });

    let dispatcher2 = dispatcher.clone();
    tokio::spawn(async move {
      let mut read = read;
      while let Some(Ok(msg)) = read.next().await {
        let Message::Text(text) = msg else { continue };
        dispatcher2.dispatch_message(text.as_bytes());
      }
    });

    Ok(Self { write_tx, dispatcher })
  }

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
}

impl super::transport::CdpTransport for WsTransport {
  async fn send_command(
    &self,
    session_id: Option<&str>,
    method: &str,
    params: serde_json::Value,
  ) -> Result<serde_json::Value, String> {
    let (mut data, rx) = self.dispatcher.build_command(session_id, method, &params)?;
    // Remove NUL terminator — WebSocket doesn't need it
    if data.last() == Some(&0) { data.pop(); }
    let text = String::from_utf8(data).map_err(|e| format!("UTF-8: {e}"))?;
    self.write_tx.send(Message::Text(text)).await.map_err(|_| "WS writer closed".to_string())?;
    match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
      Ok(Ok(result)) => result,
      Ok(Err(_)) => Err(format!("Response channel dropped for {method}")),
      Err(_) => Err(format!("Timeout waiting for {method} response")),
    }
  }

  fn register_nav_waiter(
    &self,
    session_id: &str,
    target: crate::backend::NavLifecycle,
  ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
    self.dispatcher.register_nav_waiter(session_id, target)
  }

  fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<serde_json::Value> {
    self.dispatcher.subscribe_events()
  }

  fn register_lifecycle_tracker(
    &self,
    session_id: &str,
    state: Arc<std::sync::Mutex<super::LifecycleState>>,
    notify: Arc<tokio::sync::Notify>,
  ) {
    self.dispatcher.register_lifecycle_tracker(session_id, state, notify);
  }
}

async fn discover_ws_url(port_file: &Path, child: &mut tokio::process::Child) -> Result<String, String> {
  let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
  loop {
    if tokio::time::Instant::now() >= deadline {
      return Err("Timeout waiting for DevToolsActivePort".into());
    }
    if let Ok(contents) = tokio::fs::read_to_string(port_file).await {
      let lines: Vec<&str> = contents.lines().collect();
      if lines.len() >= 2 {
        let port = lines[0].trim();
        let path = lines[1].trim();
        return Ok(format!("ws://127.0.0.1:{port}{path}"));
      }
    }
    if let Ok(Some(status)) = child.try_wait() {
      return Err(format!("Chrome exited with status {status} before providing DevToolsActivePort"));
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
  }
}
