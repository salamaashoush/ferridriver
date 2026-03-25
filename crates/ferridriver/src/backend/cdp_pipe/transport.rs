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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{broadcast, oneshot, Mutex};

/// CDP transport over pipes -- manages command IDs, response correlation, and event dispatch.
pub struct PipeTransport {
    /// Write half of the parent socket.
    writer: Mutex<tokio::io::WriteHalf<tokio::net::UnixStream>>,
    /// Next command ID.
    next_id: AtomicU64,
    /// Pending commands waiting for responses: id -> sender.
    /// The Result carries Ok(result_value) or Err(error_description).
    pending: Arc<Mutex<FxHashMap<u64, oneshot::Sender<Result<serde_json::Value, String>>>>>,
    /// Navigation waiters: sessionId -> sender that resolves when Page.loadEventFired arrives.
    /// Follows Bun's pattern: register before navigate, reader dispatches on event arrival.
    nav_waiters: Arc<Mutex<FxHashMap<String, oneshot::Sender<Result<(), String>>>>>,
    /// Broadcast channel for CDP events (console, network -- fire-and-forget listeners).
    event_tx: broadcast::Sender<serde_json::Value>,
}

impl PipeTransport {
    /// Spawn Chrome with `--remote-debugging-pipe` and set up the pipe transport.
    pub async fn spawn(
        chromium_path: &str,
        user_data_dir: &Path,
        extra_flags: &[String],
    ) -> Result<(Self, tokio::process::Child), String> {
        use std::os::unix::io::IntoRawFd;

        // Create a Unix socketpair instead of two pipes.
        // Chrome will read AND write on the child end (dup'd to fd 3 and fd 4).
        let (parent_sock, child_sock) =
            std::os::unix::net::UnixStream::pair().map_err(|e| format!("socketpair: {e}"))?;

        let child_fd = child_sock.into_raw_fd();

        let mut command = tokio::process::Command::new(chromium_path);

        // user-data-dir MUST come before --remote-debugging-pipe (Chrome quirk,
        // see Bun's ChromeProcess.zig comment about CommandLine::Init)
        command.arg(format!("--user-data-dir={}", user_data_dir.display()));
        command.arg("--remote-debugging-pipe");

        // Apply Chrome flags (from LaunchOptions)
        for flag in extra_flags {
            command.arg(flag);
        }

        // Pipe-specific flags
        command.arg("--no-startup-window"); // we create targets explicitly via CDP

        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());

        // In pre_exec, dup the child socket fd to fd 3 and fd 4 for Chrome.
        // Check return values of dup2 for safety.
        unsafe {
            command.pre_exec(move || {
                // Chrome reads commands from fd 3 and writes responses to fd 4.
                // Both point to the same socketpair end.
                //
                // Critical: child_fd comes from UnixStream::pair() which sets
                // SOCK_CLOEXEC. If child_fd happens to be 3 or 4, dup2 is a
                // no-op but CLOEXEC stays set, so exec closes the fd and Chrome
                // sees "pipe file descriptors are not open". Fix: always clear
                // CLOEXEC on child_fd first, then dup2 to both 3 and 4.
                // dup2 target fds never inherit CLOEXEC.

                // Clear CLOEXEC on the source fd
                let flags = libc::fcntl(child_fd, libc::F_GETFD);
                if flags != -1 {
                    libc::fcntl(child_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
                }

                if child_fd != 3 {
                    if libc::dup2(child_fd, 3) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                if child_fd != 4 {
                    if libc::dup2(child_fd, 4) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
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

        // Convert the parent socket to a tokio UnixStream
        parent_sock.set_nonblocking(true).map_err(|e| format!("set_nonblocking: {e}"))?;
        let parent_stream =
            tokio::net::UnixStream::from_std(parent_sock).map_err(|e| format!("tokio stream: {e}"))?;

        let (reader, writer) = tokio::io::split(parent_stream);

        let (event_tx, _) = broadcast::channel(256);

        let pending: Arc<Mutex<FxHashMap<u64, oneshot::Sender<Result<serde_json::Value, String>>>>> =
            Arc::new(Mutex::new(FxHashMap::default()));

        let nav_waiters: Arc<Mutex<FxHashMap<String, oneshot::Sender<Result<(), String>>>>> =
            Arc::new(Mutex::new(FxHashMap::default()));

        let transport = Self {
            writer: Mutex::new(writer),
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
        pending: Arc<Mutex<FxHashMap<u64, oneshot::Sender<Result<serde_json::Value, String>>>>>,
        nav_waiters: Arc<Mutex<FxHashMap<String, oneshot::Sender<Result<(), String>>>>>,
    ) {
        tokio::spawn(async move {
            let mut reader = reader;
            // Chunk-based reader matching Bun's onData pattern:
            // read() what's available into rx buffer, memchr for NUL delimiters,
            // dispatch each complete message. No byte-at-a-time reads.
            let mut rx = Vec::with_capacity(64 * 1024);
            let mut tmp = [0u8; 32768];

            loop {
                // Read a chunk from the socket
                let n = match reader.read(&mut tmp).await {
                    Ok(0) => return,  // EOF
                    Ok(n) => n,
                    Err(_) => return,
                };
                rx.extend_from_slice(&tmp[..n]);

                // Process all complete NUL-delimited messages
                loop {
                    let nul_pos = match rx.iter().position(|&b| b == 0) {
                        Some(p) => p,
                        None => break, // no complete message yet
                    };
                    if nul_pos == 0 { rx.drain(..1); continue; }

                    let raw = &rx[..nul_pos];
                    use super::json_scan;

                    // Fast path: use zero-alloc scanner for dispatch fields
                    let id = json_scan::json_id(raw);

                    if id > 0 {
                        // Response — check error field without full parse
                        let error_field = json_scan::json_field(raw, b"error");
                        let payload = if !error_field.is_empty() {
                            let msg_bytes = json_scan::error_message(error_field);
                            let msg_str = std::str::from_utf8(msg_bytes).unwrap_or("CDP error");
                            Err(msg_str.to_string())
                        } else {
                            // Only full-parse the result value (needed downstream)
                            let result_field = json_scan::json_field(raw, b"result");
                            if result_field.is_empty() {
                                Ok(serde_json::Value::Object(serde_json::Map::new()))
                            } else {
                                let val: serde_json::Value = serde_json::from_slice(result_field)
                                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                                Ok(val)
                            }
                        };
                        rx.drain(..nul_pos + 1);
                        if let Some(sender) = pending.lock().await.remove(&id) {
                            let _ = sender.send(payload);
                        }
                    } else {
                        // Event — scan method and sessionId without full parse
                        let method = json_scan::json_string(json_scan::json_field(raw, b"method"));
                        let session_id = json_scan::json_string(json_scan::json_field(raw, b"sessionId"));
                        let method_str = std::str::from_utf8(method).unwrap_or("");
                        let sid_str = std::str::from_utf8(session_id).unwrap_or("");

                        match method_str {
                            // Playwright approach: use Page.lifecycleEvent which fires
                            // with the specific lifecycle name. Resolve on DOMContentLoaded.
                            "Page.lifecycleEvent" => {
                                let params = json_scan::json_field(raw, b"params");
                                let name = json_scan::json_string(json_scan::json_field(params, b"name"));
                                let name_str = std::str::from_utf8(name).unwrap_or("");
                                if name_str == "DOMContentLoaded" || name_str == "load" {
                                    if let Some(sender) = nav_waiters.lock().await.remove(&sid_str.to_string()) {
                                        let _ = sender.send(Ok(()));
                                    }
                                }
                            }
                            // Fallback: also handle the simple events
                            "Page.domContentEventFired" | "Page.loadEventFired" => {
                                if let Some(sender) = nav_waiters.lock().await.remove(&sid_str.to_string()) {
                                    let _ = sender.send(Ok(()));
                                }
                            }
                            "Inspector.targetCrashed" => {
                                if let Some(sender) = nav_waiters.lock().await.remove(&sid_str.to_string()) {
                                    let _ = sender.send(Err("Target crashed".into()));
                                }
                            }
                            _ => {}
                        }

                        // Full parse only for broadcast (console/network listeners need it)
                        if let Ok(msg) = serde_json::from_slice::<serde_json::Value>(raw) {
                            let _ = event_tx.send(msg);
                        }
                        rx.drain(..nul_pos + 1);
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
    ) -> oneshot::Receiver<Result<(), String>> {
        let (tx, rx) = oneshot::channel();
        self.nav_waiters
            .lock()
            .await
            .insert(session_id.to_string(), tx);
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
            format!(r#"{{"id":{id},"method":"{method}","params":{params_str},"sessionId":"{sid}"}}"#)
                .into_bytes()
        } else {
            format!(r#"{{"id":{id},"method":"{method}","params":{params_str}}}"#)
                .into_bytes()
        };
        data.push(0); // NUL terminator

        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        {
            let mut writer = self.writer.lock().await;
            writer
                .write_all(&data)
                .await
                .map_err(|e| format!("Write to pipe: {e}"))?;
            writer
                .flush()
                .await
                .map_err(|e| format!("Flush pipe: {e}"))?;
        }

        // Wait for response with timeout
        match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
            Ok(Ok(result)) => result, // result is already Result<Value, String>
            Ok(Err(_)) => Err(format!("Response channel dropped for {method}")),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(format!("Timeout waiting for {method} response"))
            }
        }
    }

    /// Subscribe to all CDP events (for console/network fire-and-forget listeners).
    pub fn subscribe_events(&self) -> broadcast::Receiver<serde_json::Value> {
        self.event_tx.subscribe()
    }
}
