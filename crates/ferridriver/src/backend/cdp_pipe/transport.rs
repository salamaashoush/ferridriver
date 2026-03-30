//! Pipe transport for CDP -- NUL-delimited JSON over Unix socketpair.
//!
//! Chrome's `--remote-debugging-pipe` flag makes Chrome:
//! - Read CDP commands from fd 3 (NUL-terminated JSON)
//! - Write CDP responses/events to fd 4 (NUL-terminated JSON)
//!
//! We create a Unix socketpair, dup the child end to fd 3 and 4, and
//! communicate over the parent end. No port discovery, no WebSocket handshake.
//!
//! Architecture follows Bun's ChromeBackend.cpp: fully event-driven with
//! oneshot channels for command responses and navigation completion.
//! NO polling, NO broadcast for navigation -- direct dispatch from reader.

use rustc_hash::FxHashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{broadcast, oneshot};

/// Pending commands: std::sync::Mutex for zero-futex overhead (never held across await).
type PendingMap = Arc<std::sync::Mutex<FxHashMap<u64, oneshot::Sender<Result<serde_json::Value, String>>>>>;
struct NavWaiter {
  target: crate::backend::NavLifecycle,
  tx: oneshot::Sender<Result<(), String>>,
}
/// Nav waiters: std::sync::Mutex (never held across await).
type NavWaiterMap = Arc<std::sync::Mutex<FxHashMap<String, NavWaiter>>>;


/// CDP transport over pipes -- manages command IDs, response correlation, and event dispatch.
pub struct PipeTransport {
  /// Channel sender for the dedicated writer task.
  write_tx: tokio::sync::mpsc::Sender<Vec<u8>>,
  /// Next command ID.
  next_id: AtomicU64,
  /// Pending commands waiting for responses.
  pending: PendingMap,
  /// Navigation waiters.
  nav_waiters: NavWaiterMap,
  /// Broadcast channel for CDP events (console, network -- fire-and-forget listeners).
  event_tx: broadcast::Sender<serde_json::Value>,
}

impl PipeTransport {
  /// Spawn Chrome with `--remote-debugging-pipe` and set up the pipe transport.
  pub fn spawn(
    chromium_path: &str,
    user_data_dir: &Path,
    extra_flags: &[String],
  ) -> Result<(Self, tokio::process::Child), String> {
    let mut command = tokio::process::Command::new(chromium_path);

    // user-data-dir MUST come before --remote-debugging-pipe (Chrome quirk).
    command.arg(format!("--user-data-dir={}", user_data_dir.display()));
    command.arg("--remote-debugging-pipe");

    for flag in extra_flags {
      command.arg(flag);
    }
    command.arg("--no-startup-window");

    command
      .stdin(std::process::Stdio::null())
      .stdout(std::process::Stdio::null())
      .stderr(std::process::Stdio::piped());

    // Platform-specific pipe setup and process spawning.
    let (child, reader, writer) = spawn_with_pipes(&mut command, chromium_path)?;

    let (event_tx, _) = broadcast::channel(256);

    let pending: PendingMap = Arc::new(std::sync::Mutex::new(FxHashMap::default()));

    let nav_waiters: NavWaiterMap = Arc::new(std::sync::Mutex::new(FxHashMap::default()));

    // Spawn dedicated writer task: drains channel, batches messages, single write_all.
    let (write_tx, mut write_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    tokio::spawn(async move {
      let mut writer = writer;
      let mut buf = Vec::with_capacity(8192);
      while let Some(first) = write_rx.recv().await {
        buf.clear();
        buf.extend_from_slice(&first);
        // Drain all queued messages (non-blocking) — batches parallel sends.
        while let Ok(more) = write_rx.try_recv() {
          buf.extend_from_slice(&more);
        }
        if writer.write_all(&buf).await.is_err() {
          break; // pipe closed
        }
        // Flush not needed for Unix pipes (unbuffered by default).
      }
    });

    let transport = Self {
      write_tx,
      next_id: AtomicU64::new(1),
      pending: pending.clone(),
      nav_waiters: nav_waiters.clone(),
      event_tx,
    };

    // Spawn reader task to process incoming messages from Chrome.
    Self::spawn_reader(reader, transport.event_tx.clone(), pending, nav_waiters);

    // No waiting needed. Chrome's DevToolsPipeHandler starts its read loop
    // almost immediately. Commands sent before Chrome is ready sit in the
    // kernel's socket buffer (socketpair has ~200KB buffer by default).
    // This matches Bun's approach: spawn and return immediately.

    Ok((transport, child))
  }

  fn spawn_reader(
    reader: tokio::io::ReadHalf<tokio::net::UnixStream>,
    event_tx: broadcast::Sender<serde_json::Value>,
    pending: PendingMap,
    nav_waiters: NavWaiterMap,
  ) {
    tokio::spawn(async move {
      use super::json_scan;

      let mut reader = reader;
      // Chunk-based reader matching Bun's onData pattern:
      // read() what's available into rx buffer, memchr for NUL delimiters,
      // dispatch each complete message. No byte-at-a-time reads.
      let mut rx = Vec::with_capacity(64 * 1024);
      #[allow(clippy::large_stack_arrays)] // intentional 32KB read buffer for performance
      let mut tmp = [0u8; 32768];

      loop {
        // Read a chunk from the socket
        let n = match reader.read(&mut tmp).await {
          Ok(0) | Err(_) => return, // EOF or error
          Ok(n) => n,
        };
        rx.extend_from_slice(&tmp[..n]);

        // Process all complete NUL-delimited messages
        while let Some(nul_pos) = rx.iter().position(|&b| b == 0) {
          if nul_pos == 0 {
            rx.drain(..1);
            continue;
          }

          let raw = &rx[..nul_pos];

          // Fast path: use zero-alloc scanner for dispatch fields
          let id = json_scan::json_id(raw);

          if id > 0 {
            // Response — check error field without full parse
            let error_field = json_scan::json_field(raw, b"error");
            let payload = if error_field.is_empty() {
              // Only full-parse the result value (needed downstream)
              let result_field = json_scan::json_field(raw, b"result");
              if result_field.is_empty() {
                Ok(serde_json::Value::Object(serde_json::Map::new()))
              } else {
                let val: serde_json::Value =
                  serde_json::from_slice(result_field).unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                Ok(val)
              }
            } else {
              let msg_bytes = json_scan::error_message(error_field);
              let msg_str = std::str::from_utf8(msg_bytes).unwrap_or("CDP error");
              Err(msg_str.to_string())
            };
            rx.drain(..=nul_pos);
            if let Some(sender) = pending.lock().unwrap().remove(&id) {
              let _ = sender.send(payload);
            }
          } else {
            // Event — scan method and sessionId without full parse
            let method = json_scan::json_string(json_scan::json_field(raw, b"method"));
            let session_id = json_scan::json_string(json_scan::json_field(raw, b"sessionId"));
            let method_str = std::str::from_utf8(method).unwrap_or("");
            let sid_str = std::str::from_utf8(session_id).unwrap_or("");

            // Single lock for all navigation event dispatch.
            {
              use crate::backend::NavLifecycle;
              let key = sid_str.to_string();
              let mut waiters = nav_waiters.lock().unwrap();
              match method_str {
                "Page.frameNavigated" => {
                  if matches!(waiters.get(&key).map(|w| w.target), Some(NavLifecycle::Commit)) {
                    if let Some(w) = waiters.remove(&key) { let _ = w.tx.send(Ok(())); }
                  }
                },
                "Page.lifecycleEvent" => {
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
                },
                "Page.loadEventFired" => {
                  if matches!(waiters.get(&key).map(|w| w.target), Some(NavLifecycle::Load | NavLifecycle::DomContentLoaded)) {
                    if let Some(w) = waiters.remove(&key) { let _ = w.tx.send(Ok(())); }
                  }
                },
                "Page.domContentEventFired" => {
                  if matches!(waiters.get(&key).map(|w| w.target), Some(NavLifecycle::DomContentLoaded)) {
                    if let Some(w) = waiters.remove(&key) { let _ = w.tx.send(Ok(())); }
                  }
                },
                "Inspector.targetCrashed" => {
                  if let Some(w) = waiters.remove(&key) {
                    let _ = w.tx.send(Err("Target crashed".into()));
                  }
                },
                _ => {},
              }
            }

            // Full parse only for broadcast (console/network listeners need it)
            if let Ok(msg) = serde_json::from_slice::<serde_json::Value>(raw) {
              let _ = event_tx.send(msg);
            }
            rx.drain(..=nul_pos);
          }
        }
      }
    });
  }

  /// Register a navigation waiter for a session. Returns a receiver that resolves
  /// when Page.loadEventFired arrives for that session.
  ///
  /// Follows Bun's ChromeBackend.cpp pattern: caller registers BEFORE sending
  /// Page.navigate, then awaits the receiver after the navigate response returns.
  pub async fn register_nav_waiter(
    &self,
    session_id: &str,
    target: crate::backend::NavLifecycle,
  ) -> oneshot::Receiver<Result<(), String>> {
    let (tx, rx) = oneshot::channel();
    self.nav_waiters.lock().unwrap().insert(session_id.to_string(), NavWaiter { target, tx });
    rx
  }


  /// Send a CDP command and wait for the response.
  pub async fn send_command(
    &self,
    session_id: Option<&str>,
    method: &str,
    params: serde_json::Value,
  ) -> Result<serde_json::Value, String> {
    let id = self.next_id.fetch_add(1, Ordering::Relaxed);

    // Build JSON directly as bytes, skip intermediate serde_json::Value.
    // params is already a Value so we serialize it once.
    let params_str = serde_json::to_string(&params).map_err(|e| format!("Serialize: {e}"))?;
    let mut data = if let Some(sid) = session_id {
      format!(r#"{{"id":{id},"method":"{method}","params":{params_str},"sessionId":"{sid}"}}"#).into_bytes()
    } else {
      format!(r#"{{"id":{id},"method":"{method}","params":{params_str}}}"#).into_bytes()
    };
    data.push(0); // NUL terminator

    let (tx, rx) = oneshot::channel();
    self.pending.lock().unwrap().insert(id, tx);

    // Push to writer task channel (~20-50ns, non-blocking).
    // Writer task batches queued messages and does one write_all syscall.
    self.write_tx.send(data).await.map_err(|_| "Pipe writer closed".to_string())?;

    // Wait for response with timeout
    match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
      Ok(Ok(result)) => result, // result is already Result<Value, String>
      Ok(Err(_)) => Err(format!("Response channel dropped for {method}")),
      Err(_) => {
        self.pending.lock().unwrap().remove(&id);
        Err(format!("Timeout waiting for {method} response"))
      },
    }
  }

  /// Subscribe to all CDP events (for console/network fire-and-forget listeners).
  pub fn subscribe_events(&self) -> broadcast::Receiver<serde_json::Value> {
    self.event_tx.subscribe()
  }
}

// ── Platform-specific pipe spawning ──

/// Unix: socketpair + dup2 to fd 3/4.
#[cfg(unix)]
fn spawn_with_pipes(
  command: &mut tokio::process::Command,
  _chromium_path: &str,
) -> Result<(
  tokio::process::Child,
  tokio::io::ReadHalf<tokio::net::UnixStream>,
  tokio::io::WriteHalf<tokio::net::UnixStream>,
), String> {
  use std::os::unix::io::IntoRawFd;

  let (parent_sock, child_sock) =
    std::os::unix::net::UnixStream::pair().map_err(|e| format!("socketpair: {e}"))?;
  let child_fd = child_sock.into_raw_fd();

  #[allow(unsafe_code)]
  unsafe {
    command.pre_exec(move || {
      let flags = libc::fcntl(child_fd, libc::F_GETFD);
      if flags != -1 {
        libc::fcntl(child_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
      }
      if child_fd != 3 && libc::dup2(child_fd, 3) == -1 {
        return Err(std::io::Error::last_os_error());
      }
      if child_fd != 4 && libc::dup2(child_fd, 4) == -1 {
        return Err(std::io::Error::last_os_error());
      }
      if child_fd != 3 && child_fd != 4 {
        libc::close(child_fd);
      }
      Ok(())
    });
  }

  let child = command
    .spawn()
    .map_err(|e| format!("Failed to launch Chrome with --remote-debugging-pipe: {e}"))?;

  parent_sock
    .set_nonblocking(true)
    .map_err(|e| format!("set_nonblocking: {e}"))?;
  let stream =
    tokio::net::UnixStream::from_std(parent_sock).map_err(|e| format!("tokio stream: {e}"))?;
  let (reader, writer) = tokio::io::split(stream);
  Ok((child, reader, writer))
}

/// Windows: anonymous pipes via `os_pipe`, passed as inheritable handles.
/// Chrome reads from handle 3, writes to handle 4 (mapped via `--remote-debugging-io`
/// or inherited stdio extras).
#[cfg(windows)]
fn spawn_with_pipes(
  command: &mut tokio::process::Command,
  _chromium_path: &str,
) -> Result<(
  tokio::process::Child,
  tokio::io::ReadHalf<tokio::io::DuplexStream>,
  tokio::io::WriteHalf<tokio::io::DuplexStream>,
), String> {
  // On Windows, Chrome's --remote-debugging-pipe uses handle 3 and 4.
  // We create anonymous pipes and pass them via STARTUPINFO additional handles.
  // For now, fall back to cdp_raw (WebSocket) on Windows. Pipe transport
  // requires platform-specific handle inheritance that is complex to implement.
  Err("CDP pipe transport is not yet supported on Windows. Use --backend cdp-raw instead.".into())
}
