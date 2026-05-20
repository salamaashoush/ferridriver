//! IPC between parent and `WebKit` host subprocess.
//!
//! Wire-level binary frame protocol lives in
//! [`ferridriver_webkit_wire`]; this module is the parent-side client —
//! [`IpcClient`], reader thread, response dispatch, route-handler
//! plumbing.
//!
//! Child: single-threaded, nonblocking socket, run-loop-driven (`NSRunLoop`
//! on macOS, GTK main loop on Linux). NO background threads. NO mpsc. NO
//! socket cloning.
//!
//! Parent: std blocking sockets, background reader thread, oneshot
//! channels. `std::process::Command` spawn (NOT tokio).

use rustc_hash::FxHashMap;
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::oneshot;

pub use ferridriver_webkit_wire::{FRAME_HDR, Op, Rep, frame_write, str_decode, str_encode};

// ─── IPC Response ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum IpcResponse {
  Ok,
  Error(String),
  Value(serde_json::Value),
  ViewCreated(u64),
  ViewList(Vec<u64>),
  Binary(Vec<u8>),
}

// ─── Response decoding helpers ──────────────────────────────────────────────

fn decode_value_response(payload: &[u8]) -> IpcResponse {
  let mut o = 0;
  let raw = str_decode(payload, &mut o);
  let v = serde_json::from_str(&raw).unwrap_or(serde_json::Value::String(raw));
  IpcResponse::Value(v)
}

fn decode_view_created(payload: &[u8]) -> IpcResponse {
  let vid = if payload.len() >= 8 {
    u64::from_le_bytes([
      payload[0], payload[1], payload[2], payload[3], payload[4], payload[5], payload[6], payload[7],
    ])
  } else {
    0
  };
  IpcResponse::ViewCreated(vid)
}

fn decode_view_list(payload: &[u8]) -> IpcResponse {
  let cnt = if payload.len() >= 4 {
    u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize
  } else {
    0
  };
  let mut ids = Vec::with_capacity(cnt);
  for i in 0..cnt {
    let o = 4 + i * 8;
    if o + 8 <= payload.len() {
      ids.push(u64::from_le_bytes([
        payload[o],
        payload[o + 1],
        payload[o + 2],
        payload[o + 3],
        payload[o + 4],
        payload[o + 5],
        payload[o + 6],
        payload[o + 7],
      ]));
    }
  }
  IpcResponse::ViewList(ids)
}

fn decode_shm_screenshot(payload: &[u8]) -> IpcResponse {
  // REP_SHM_SCREENSHOT: payload = u32 nameLen + name + u32 pngLen
  // Open shared memory, read PNG bytes, unlink. Zero-copy from child.
  let mut o = 0;
  let shm_name = str_decode(payload, &mut o);
  let png_len = if o + 4 <= payload.len() {
    u32::from_le_bytes([payload[o], payload[o + 1], payload[o + 2], payload[o + 3]]) as usize
  } else {
    0
  };

  let bytes = (|| -> Result<Vec<u8>, String> {
    use std::ffi::CString;
    let c_name = CString::new(shm_name.as_bytes()).map_err(|e| format!("CString: {e}"))?;
    // SAFETY: All libc calls operate on POSIX shared memory:
    // - shm_open: c_name is a valid CString, fd is checked for < 0
    // - mmap: fd is valid from shm_open, png_len matches child allocation
    // - close: fd is valid
    // - from_raw_parts: map is valid readable memory of png_len bytes
    // - munmap + shm_unlink: cleaning up after data is copied to Vec
    #[allow(unsafe_code)]
    unsafe {
      let fd = libc::shm_open(c_name.as_ptr(), libc::O_RDONLY, 0);
      if fd < 0 {
        return Err("shm_open failed".into());
      }
      let map = libc::mmap(std::ptr::null_mut(), png_len, libc::PROT_READ, libc::MAP_SHARED, fd, 0);
      libc::close(fd);
      if map == libc::MAP_FAILED {
        libc::shm_unlink(c_name.as_ptr());
        return Err("mmap failed".into());
      }
      let data = std::slice::from_raw_parts(map as *const u8, png_len).to_vec();
      libc::munmap(map, png_len);
      libc::shm_unlink(c_name.as_ptr());
      Ok(data)
    }
  })();

  match bytes {
    Ok(data) => IpcResponse::Binary(data),
    Err(e) => IpcResponse::Error(e),
  }
}

/// Handle a route request from the host subprocess: decode the request,
/// run the route handler callback, and write the response back.
fn handle_route_request(
  rid: u64,
  payload: &[u8],
  route_handler: &Arc<StdMutex<Option<RouteCallback>>>,
  writer: &Arc<StdMutex<std::os::unix::net::UnixStream>>,
) {
  let mut o = 0;
  let url = str_decode(payload, &mut o);
  let method = str_decode(payload, &mut o);
  let headers_json = str_decode(payload, &mut o);
  let post_data = str_decode(payload, &mut o);

  let action_json = if let Ok(guard) = route_handler.lock() {
    if let Some(handler) = guard.as_ref() {
      handler(&url, &method, &headers_json, &post_data)
    } else {
      r#"{"action":"continue"}"#.to_string()
    }
  } else {
    r#"{"action":"continue"}"#.to_string()
  };

  // Write response back to child as OP_ROUTE_REQUEST (71).
  // The host's dispatch_frame will handle op=71 by resolving
  // the pending replyHandler.
  let action_bytes = action_json.as_bytes();
  #[allow(clippy::cast_possible_truncation)] // action JSON always < 4GB
  let action_len = action_bytes.len() as u32;
  let mut resp_payload = Vec::with_capacity(4 + action_len as usize);
  resp_payload.extend_from_slice(&action_len.to_le_bytes());
  resp_payload.extend_from_slice(action_bytes);
  {
    #[allow(clippy::cast_possible_truncation)] // rid fits in u32 (originated as u32)
    let rid_u32 = rid as u32;
    if let Ok(mut w) = writer.lock() {
      // Route-reply errors are non-fatal: if the writer is gone, the host has
      // either crashed or shut down — the pending fetch on the page side will
      // surface its own timeout. Mirrors the macOS reader-thread behaviour
      // (the prior `let _ = w.write_all(...)` pattern in the old inline impl).
      let _ = frame_write(&mut *w, rid_u32, 71 /* OP_ROUTE_REQUEST */, &resp_payload);
    }
  }
}

// ─── Parent-side client ─────────────────────────────────────────────────────

/// Route handler callback for `WebKit` network interception.
/// Takes (url, method, `headers_json`, `post_data`) and returns serialized `RouteAction` JSON.
pub type RouteCallback = Arc<dyn Fn(&str, &str, &str, &str) -> String + Send + Sync>;

/// Decode `REP_NET_RESPONSE_EVENT`'s single-string JSON payload into a
/// [`NetworkEvent::Response`]. Extracted from the reader-thread match
/// arm so the dispatch loop stays under the line-count budget.
fn decode_network_response_event(payload: &[u8]) -> Option<NetworkEvent> {
  let mut o = 0;
  let json = str_decode(payload, &mut o);
  let value: serde_json::Value = serde_json::from_str(&json).ok()?;
  let id = value.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
  let status = value.get("status").and_then(serde_json::Value::as_i64).unwrap_or(0);
  let status_text = value
    .get("statusText")
    .and_then(|v| v.as_str())
    .unwrap_or("")
    .to_string();
  let url = value.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
  let headers = value
    .get("headers")
    .and_then(|h| h.as_object())
    .map(|obj| {
      obj
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
        .collect::<FxHashMap<String, String>>()
    })
    .unwrap_or_default();
  Some(NetworkEvent::Response {
    id,
    status,
    status_text,
    url,
    headers,
  })
}

/// Network event payload pushed by the host's JS interceptor.
///
/// `Request` carries the four-string tuple from the original
/// `REP_NET_EVENT`. `Response` and `Failure` are added so
/// `Page.on('response')` / `request.failure()` work on the `WebKit`
/// backend (within the JS-fetch interceptor's reach — body bytes are
/// still typed `Unsupported` because `WKWebView` exposes no public
/// API for them).
#[derive(Debug, Clone)]
pub enum NetworkEvent {
  Request {
    id: String,
    method: String,
    url: String,
    resource_type: String,
  },
  Response {
    id: String,
    status: i64,
    status_text: String,
    url: String,
    headers: FxHashMap<String, String>,
  },
  Failure {
    id: String,
    error_text: String,
  },
}

/// Context for the background reader thread, bundling event logs and routing state.
struct ReaderCtx {
  console_log: Arc<StdMutex<Vec<(String, String, u64)>>>,
  dialog_log: Arc<StdMutex<Vec<(String, String, String)>>>,
  network_log: Arc<StdMutex<Vec<NetworkEvent>>>,
  route_handler: Arc<StdMutex<Option<RouteCallback>>>,
  writer: Arc<StdMutex<std::os::unix::net::UnixStream>>,
  event_notify: Arc<tokio::sync::Notify>,
}

pub struct IpcClient {
  writer: StdMutex<std::os::unix::net::UnixStream>,
  pending: Arc<StdMutex<FxHashMap<u64, oneshot::Sender<IpcResponse>>>>,
  next_id: AtomicU64,
  /// Console messages pushed by the host via `REP_CONSOLE_EVENT`.
  pub console_log: Arc<StdMutex<Vec<(String, String, u64)>>>,
  /// Dialog events pushed by the host via `REP_DIALOG_EVENT`.
  pub dialog_log: Arc<StdMutex<Vec<(String, String, String)>>>,
  /// Network events pushed by the host via `REP_NET_EVENT`.
  pub network_log: Arc<StdMutex<Vec<NetworkEvent>>>,
  /// Notified when any event arrives, so `attach_listeners` can drain immediately.
  pub event_notify: Arc<tokio::sync::Notify>,
  /// Route handler callback. Called from the reader thread for route requests.
  /// Set by `WebKitPage` when routes are registered.
  pub route_handler: Arc<StdMutex<Option<RouteCallback>>>,
}

/// File name of the cross-platform `WebKit` host binary, produced by
/// `crates/ferridriver-webkit-host` for both macOS (Obj-C `WKWebView`)
/// and Linux (webkit6 / GTK4).
const HOST_BINARY_NAME: &str = "ferridriver-webkit-host";

impl IpcClient {
  /// Resolve the path to the `WebKit` host binary.
  ///
  /// Priority:
  /// 1. `FERRIDRIVER_WEBKIT_HOST` env var (explicit override)
  /// 2. Sibling to the running executable (e.g. next to `ferridriver` CLI)
  /// 3. Cargo target directory walked up from `CARGO_MANIFEST_DIR`
  ///    (dev builds — `cargo build` puts the binary in
  ///    `target/{profile}/ferridriver-webkit-host`)
  /// 4. `~/Library/Caches/ferridriver/ferridriver-webkit-host` (macOS-native)
  /// 5. `~/.cache/ferridriver/ferridriver-webkit-host` (XDG)
  fn resolve_host_binary() -> crate::error::Result<std::path::PathBuf> {
    use crate::FerriError;
    // Priority 1: Environment variable override
    if let Ok(path) = std::env::var("FERRIDRIVER_WEBKIT_HOST") {
      let p = std::path::PathBuf::from(&path);
      if p.exists() {
        return Ok(p);
      }
      return Err(FerriError::invalid_argument(
        "FERRIDRIVER_WEBKIT_HOST",
        format!("path {path:?} does not exist"),
      ));
    }

    // Priority 2: Sibling to the running executable
    if let Ok(exe) = std::env::current_exe() {
      if let Some(dir) = exe.parent() {
        let sibling = dir.join(HOST_BINARY_NAME);
        if sibling.exists() {
          return Ok(sibling);
        }
      }
    }

    // Priority 3: Cache directories (macOS Library/Caches + XDG ~/.cache)
    if let Some(home) = std::env::var_os("HOME") {
      let home = std::path::Path::new(&home);
      // macOS-native cache location (used by npm postinstall)
      let mac_cached = home.join("Library/Caches/ferridriver").join(HOST_BINARY_NAME);
      if mac_cached.exists() {
        return Ok(mac_cached);
      }
      // XDG-style fallback
      let xdg_cached = home.join(".cache/ferridriver").join(HOST_BINARY_NAME);
      if xdg_cached.exists() {
        return Ok(xdg_cached);
      }
    }

    Err(FerriError::backend(format!(
      "WebKit host binary not found. Checked:\n  \
       1. $FERRIDRIVER_WEBKIT_HOST (not set)\n  \
       2. sibling to current executable\n  \
       3. ~/Library/Caches/ferridriver/{HOST_BINARY_NAME}\n  \
       4. ~/.cache/ferridriver/{HOST_BINARY_NAME}\n\
       Rebuild with `cargo build --workspace` (or `cargo build -p ferridriver-webkit-host`)."
    )))
  }

  /// Spawn the `WebKit` host subprocess and establish IPC communication.
  ///
  /// # Errors
  ///
  /// Returns an error if the Unix socketpair cannot be created, the host binary
  /// cannot be found or spawned, or the subprocess fails to become ready within
  /// the probe timeout.
  pub async fn spawn(headless: bool) -> crate::error::Result<(Self, std::process::Child)> {
    use std::os::unix::io::IntoRawFd;

    let (parent_sock, child_sock) = std::os::unix::net::UnixStream::pair()?;
    let child_fd = child_sock.into_raw_fd();
    let exe = Self::resolve_host_binary()?;

    let child = Self::spawn_host_process(&exe, child_fd, headless)?;

    let read_sock = parent_sock.try_clone()?;
    let write_sock = parent_sock;

    let pending: Arc<StdMutex<FxHashMap<u64, oneshot::Sender<IpcResponse>>>> =
      Arc::new(StdMutex::new(FxHashMap::default()));
    let console_log: Arc<StdMutex<Vec<(String, String, u64)>>> = Arc::new(StdMutex::new(Vec::new()));
    let dialog_log: Arc<StdMutex<Vec<(String, String, String)>>> = Arc::new(StdMutex::new(Vec::new()));
    let network_log: Arc<StdMutex<Vec<NetworkEvent>>> = Arc::new(StdMutex::new(Vec::new()));
    let route_handler: Arc<StdMutex<Option<RouteCallback>>> = Arc::new(StdMutex::new(None));
    let writer_for_reader = Arc::new(StdMutex::new(write_sock.try_clone()?));
    let event_notify = Arc::new(tokio::sync::Notify::new());

    // Reader thread: blocking reads of binary frames
    Self::spawn_reader_thread(
      read_sock,
      pending.clone(),
      ReaderCtx {
        console_log: console_log.clone(),
        dialog_log: dialog_log.clone(),
        network_log: network_log.clone(),
        route_handler: route_handler.clone(),
        writer: writer_for_reader,
        event_notify: event_notify.clone(),
      },
    );

    let client = Self {
      writer: StdMutex::new(write_sock),
      pending,
      next_id: AtomicU64::new(1),
      console_log,
      dialog_log,
      network_log,
      event_notify,
      route_handler,
    };

    // Probe subprocess readiness -- send ListViews, retry until it responds.
    for _ in 0..200 {
      match tokio::time::timeout(std::time::Duration::from_millis(50), client.send_empty(Op::ListViews)).await {
        Ok(Ok(_)) => return Ok((client, child)),
        _ => tokio::time::sleep(std::time::Duration::from_millis(5)).await,
      }
    }
    Ok((client, child))
  }

  /// Spawn the `WebKit` host subprocess with the IPC socket passed as fd 3.
  fn spawn_host_process(
    exe: &std::path::Path,
    child_fd: std::os::unix::io::RawFd,
    headless: bool,
  ) -> crate::error::Result<std::process::Child> {
    // SAFETY: pre_exec runs in the forked child before exec. We manipulate
    // file descriptors to pass the IPC socket as fd 3 to the host subprocess.
    // The child_fd is valid (from IntoRawFd above) and only used in the child.
    #[allow(unsafe_code)]
    let child = unsafe {
      use std::os::unix::process::CommandExt;
      let mut cmd = std::process::Command::new(exe);
      cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit());
      // Linux: when `headless` is requested, ask the host to re-exec
      // under `xvfb-run` so the GTK window never lands on the user's
      // desktop. macOS host ignores this env var (its `WKWebView`
      // host already runs offscreen by design).
      if headless {
        cmd.env("FERRIDRIVER_WEBKIT_HEADLESS", "1");
      }
      cmd.pre_exec(move || {
        // Put the WebKit host in its own session+process group so any
        // helper it forks is torn down together with it.
        libc::setsid();
        let flags = libc::fcntl(child_fd, libc::F_GETFD);
        if flags != -1 {
          libc::fcntl(child_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
        }
        if child_fd != 3 {
          if libc::dup2(child_fd, 3) == -1 {
            return Err(std::io::Error::last_os_error());
          }
          libc::close(child_fd);
        }
        Ok(())
      });
      cmd
        .spawn()
        .map_err(|e| crate::FerriError::backend(format!("spawn webkit host ({}): {e}", exe.display())))?
    };
    Ok(child)
  }

  /// Spawn the background reader thread that processes incoming IPC frames.
  #[allow(clippy::too_many_lines)] // dispatcher fan-out per Rep code; splitting hurts readability
  fn spawn_reader_thread(
    read_sock: std::os::unix::net::UnixStream,
    pending: Arc<StdMutex<FxHashMap<u64, oneshot::Sender<IpcResponse>>>>,
    ctx: ReaderCtx,
  ) {
    std::thread::spawn(move || {
      let mut s = read_sock;
      let mut h = [0u8; FRAME_HDR];
      loop {
        if s.read_exact(&mut h).is_err() {
          return;
        }
        let len = u32::from_le_bytes([h[0], h[1], h[2], h[3]]) as usize;
        let rid = u64::from(u32::from_le_bytes([h[4], h[5], h[6], h[7]]));
        let rep = h[8];
        let mut payload = vec![0u8; len];
        if len > 0 && s.read_exact(&mut payload).is_err() {
          return;
        }

        let response = match rep {
          1 => IpcResponse::Ok,
          2 => {
            let mut o = 0;
            IpcResponse::Error(str_decode(&payload, &mut o))
          },
          3 => decode_value_response(&payload),
          4 => decode_view_created(&payload),
          5 => decode_view_list(&payload),
          6 => IpcResponse::Binary(payload),
          7 => decode_shm_screenshot(&payload),
          8 => {
            let mut o = 0;
            let level = str_decode(&payload, &mut o);
            let text = str_decode(&payload, &mut o);
            // The host now appends the originating view id so per-page
            // listeners can filter to events that originated in their
            // own view (`drain_console_events` discards mismatches).
            let vid = if o + 8 <= payload.len() {
              let mut vid_bytes = [0u8; 8];
              vid_bytes.copy_from_slice(&payload[o..o + 8]);
              u64::from_le_bytes(vid_bytes)
            } else {
              0
            };
            if let Ok(mut log) = ctx.console_log.lock() {
              log.push((level, text, vid));
            }
            ctx.event_notify.notify_one();
            continue;
          },
          9 => {
            let mut o = 0;
            let dtype = str_decode(&payload, &mut o);
            let message = str_decode(&payload, &mut o);
            let action = str_decode(&payload, &mut o);
            if let Ok(mut log) = ctx.dialog_log.lock() {
              log.push((dtype, message, action));
            }
            ctx.event_notify.notify_one();
            continue;
          },
          10 => {
            let mut o = 0;
            let id = str_decode(&payload, &mut o);
            let method = str_decode(&payload, &mut o);
            let url = str_decode(&payload, &mut o);
            let res_type = str_decode(&payload, &mut o);
            if let Ok(mut log) = ctx.network_log.lock() {
              log.push(NetworkEvent::Request {
                id,
                method,
                url,
                resource_type: res_type,
              });
            }
            ctx.event_notify.notify_one();
            continue;
          },
          11 => {
            handle_route_request(rid, &payload, &ctx.route_handler, &ctx.writer);
            continue;
          },
          12 => {
            if let Some(event) = decode_network_response_event(&payload) {
              if let Ok(mut log) = ctx.network_log.lock() {
                log.push(event);
              }
              ctx.event_notify.notify_one();
            }
            continue;
          },
          13 => {
            let mut o = 0;
            let id = str_decode(&payload, &mut o);
            let error_text = str_decode(&payload, &mut o);
            if let Ok(mut log) = ctx.network_log.lock() {
              log.push(NetworkEvent::Failure { id, error_text });
            }
            ctx.event_notify.notify_one();
            continue;
          },
          _ => IpcResponse::Error(format!("unknown rep {rep}")),
        };

        if let Ok(mut pending_guard) = pending.lock() {
          if let Some(tx) = pending_guard.remove(&rid) {
            let _ = tx.send(response);
          }
        }
      }
    });
  }

  /// Send an IPC frame to the `WebKit` host subprocess and wait for a response.
  ///
  /// # Errors
  ///
  /// Returns an error if the response channel is dropped (subprocess crashed),
  /// the request times out after 30 seconds, or the mutex is poisoned.
  pub async fn send(&self, op: Op, payload: &[u8]) -> crate::error::Result<IpcResponse> {
    use crate::FerriError;
    #[allow(clippy::cast_possible_truncation)] // request IDs wrap around at u32::MAX, which is acceptable
    let rid = self.next_id.fetch_add(1, Ordering::Relaxed) as u32;
    let (tx, rx) = oneshot::channel();
    self
      .pending
      .lock()
      .map_err(|e| FerriError::backend(format!("pending lock poisoned: {e}")))?
      .insert(u64::from(rid), tx);
    {
      let mut w = self
        .writer
        .lock()
        .map_err(|e| FerriError::backend(format!("writer lock poisoned: {e}")))?;
      frame_write(&mut *w, rid, op as u8, payload)
        .map_err(|e| FerriError::backend(format!("webkit ipc write: {e}")))?;
    }
    match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
      Ok(Ok(r)) => Ok(r),
      Ok(Err(_)) => Err(FerriError::target_closed(Some(
        "webkit host response channel dropped".into(),
      ))),
      Err(_) => {
        if let Ok(mut guard) = self.pending.lock() {
          guard.remove(&u64::from(rid));
        }
        Err(FerriError::timeout(format!("webkit ipc op {op:?}"), 30_000))
      },
    }
  }

  /// Send an IPC frame with a single string payload.
  ///
  /// # Errors
  ///
  /// Returns an error if the underlying `send` call fails (timeout, channel dropped,
  /// or mutex poisoned).
  pub async fn send_str(&self, op: Op, s: &str) -> crate::error::Result<IpcResponse> {
    let mut p = Vec::new();
    str_encode(&mut p, s);
    self.send(op, &p).await
  }

  /// Send an IPC frame with a string payload and a view ID suffix.
  ///
  /// # Errors
  ///
  /// Returns an error if the underlying `send` call fails (timeout, channel dropped,
  /// or mutex poisoned).
  pub async fn send_str_vid(&self, op: Op, s: &str, vid: u64) -> crate::error::Result<IpcResponse> {
    let mut p = Vec::new();
    str_encode(&mut p, s);
    p.extend_from_slice(&vid.to_le_bytes());
    self.send(op, &p).await
  }

  /// Send an IPC frame with only a view ID as payload.
  ///
  /// # Errors
  ///
  /// Returns an error if the underlying `send` call fails (timeout, channel dropped,
  /// or mutex poisoned).
  pub async fn send_vid(&self, op: Op, vid: u64) -> crate::error::Result<IpcResponse> {
    self.send(op, &vid.to_le_bytes()).await
  }

  /// Send an IPC frame with an empty payload.
  ///
  /// # Errors
  ///
  /// Returns an error if the underlying `send` call fails (timeout, channel dropped,
  /// or mutex poisoned).
  pub async fn send_empty(&self, op: Op) -> crate::error::Result<IpcResponse> {
    self.send(op, &[]).await
  }
}
