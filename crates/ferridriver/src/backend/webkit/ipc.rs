//! IPC between parent and `WebKit` host subprocess.
//!
//! Binary frame protocol over Unix socketpair (ported from Bun's `ipc_protocol.h)`:
//!   Frame = { u32 len, u32 `req_id`, u8 op } (9 bytes LE) + payload\[len\]
//!   Strings = u32 len (LE) + UTF-8 bytes
//!
//! Child: single-threaded, nonblocking socket, `NSRunLoop` for `AppKit` callbacks.
//!        NO background threads. NO mpsc. NO socket cloning.
//!        Matches Bun's `host_main.cpp`: `read()` to EAGAIN, parse frames, dispatch.
//!
//! Parent: std blocking sockets, background reader thread, oneshot channels.
//!         `std::process::Command` spawn (NOT tokio).

use rustc_hash::FxHashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::oneshot;

// ─── Binary frame protocol ──────────────────────────────────────────────────

const FRAME_HDR: usize = 9;

#[repr(u8)]
#[derive(Debug, Clone, Copy)]
pub enum Op {
  CreateView = 1,
  Navigate = 2,
  Evaluate = 3,
  Screenshot = 4,
  Close = 5,
  GoBack = 7,
  GoForward = 8,
  Reload = 9,
  Click = 10,
  Type = 11,
  PressKey = 12,
  GetUrl = 20,
  GetTitle = 21,
  ListViews = 22,
  SetUserAgent = 30,
  WaitNav = 40,
  SetFileInput = 50,
  SetViewport = 51,
  GetCookies = 60,
  SetCookie = 61,
  DeleteCookie = 62,
  ClearCookies = 63,
  LoadHtml = 64,
  AddInitScript = 65,
  MouseEvent = 66,
  SetLocale = 67,
  SetTimezone = 68,
  EmulateMedia = 69,
  AccessibilityTree = 70,
  /// Route request: sent FROM the host subprocess TO the parent when a JS
  /// fetch/XHR matches a route. Payload: str url + str method + str `headers_json` + str body.
  /// Parent responds with `REP_VALUE` containing the serialized `RouteAction` JSON.
  RouteRequest = 71,
  Shutdown = 255,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy)]
pub enum Rep {
  Ok = 1,
  Error = 2,
  Value = 3,
  ViewCreated = 4,
  ViewList = 5,
  Binary = 6,
}

fn frame_write(w: &mut impl Write, req_id: u32, op: u8, payload: &[u8]) {
  #[allow(clippy::cast_possible_truncation)] // payload length is always < 4GB for IPC frames
  let len = payload.len() as u32;
  let mut h = [0u8; FRAME_HDR];
  h[0..4].copy_from_slice(&len.to_le_bytes());
  h[4..8].copy_from_slice(&req_id.to_le_bytes());
  h[8] = op;
  let _ = w.write_all(&h);
  if !payload.is_empty() {
    let _ = w.write_all(payload);
  }
  let _ = w.flush();
}

pub fn str_encode(buf: &mut Vec<u8>, s: &str) {
  #[allow(clippy::cast_possible_truncation)] // string length is always < 4GB for IPC payloads
  let str_len = s.len() as u32;
  buf.extend_from_slice(&str_len.to_le_bytes());
  buf.extend_from_slice(s.as_bytes());
}

fn str_decode(data: &[u8], off: &mut usize) -> String {
  if *off + 4 > data.len() {
    return String::new();
  }
  let n = u32::from_le_bytes([data[*off], data[*off + 1], data[*off + 2], data[*off + 3]]) as usize;
  *off += 4;
  if *off + n > data.len() {
    *off = data.len();
    return String::new();
  }
  let s = String::from_utf8_lossy(&data[*off..*off + n]).to_string();
  *off += n;
  s
}

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
      frame_write(&mut *w, rid_u32, 71 /* OP_ROUTE_REQUEST */, &resp_payload);
    }
  }
}

// ─── Parent-side client ─────────────────────────────────────────────────────

/// Route handler callback for `WebKit` network interception.
/// Takes (url, method, `headers_json`, `post_data`) and returns serialized `RouteAction` JSON.
pub type RouteCallback = Arc<dyn Fn(&str, &str, &str, &str) -> String + Send + Sync>;

/// Network event tuple: (id, method, url, `resource_type`).
pub type NetworkEvent = (String, String, String, String);

/// Context for the background reader thread, bundling event logs and routing state.
struct ReaderCtx {
  console_log: Arc<StdMutex<Vec<(String, String)>>>,
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
  pub console_log: Arc<StdMutex<Vec<(String, String)>>>,
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

/// Path to the host binary baked in at compile time by build.rs.
#[cfg(target_os = "macos")]
static HOST_BINARY_PATH: &str = concat!(env!("OUT_DIR"), "/fd_webkit_host");

/// File name of the `WebKit` host binary.
#[cfg(target_os = "macos")]
const HOST_BINARY_NAME: &str = "fd_webkit_host";

impl IpcClient {
  /// Resolve the path to the `WebKit` host binary.
  ///
  /// Priority:
  /// 1. `FERRIDRIVER_WEBKIT_HOST` env var (explicit override)
  /// 2. Sibling to the running executable (e.g. next to `ferridriver` CLI)
  /// 3. `~/.cache/ferridriver/fd_webkit_host` (survives cargo clean)
  /// 4. Compile-time baked path from build.rs (dev builds)
  #[cfg(target_os = "macos")]
  fn resolve_host_binary() -> Result<std::path::PathBuf, String> {
    // Priority 1: Environment variable override
    if let Ok(path) = std::env::var("FERRIDRIVER_WEBKIT_HOST") {
      let p = std::path::PathBuf::from(&path);
      if p.exists() {
        return Ok(p);
      }
      return Err(format!("FERRIDRIVER_WEBKIT_HOST={path} does not exist"));
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
      // XDG-style fallback (used by build.rs)
      let xdg_cached = home.join(".cache/ferridriver").join(HOST_BINARY_NAME);
      if xdg_cached.exists() {
        return Ok(xdg_cached);
      }
    }

    // Priority 4: Compile-time baked path (dev builds)
    let baked = std::path::PathBuf::from(HOST_BINARY_PATH);
    if baked.exists() {
      return Ok(baked);
    }

    Err(format!(
      "WebKit host binary not found. Checked:\n  \
       1. $FERRIDRIVER_WEBKIT_HOST (not set)\n  \
       2. sibling to current executable\n  \
       3. ~/Library/Caches/ferridriver/{HOST_BINARY_NAME}\n  \
       4. ~/.cache/ferridriver/{HOST_BINARY_NAME}\n  \
       5. {HOST_BINARY_PATH}\n\
       Set FERRIDRIVER_WEBKIT_HOST or rebuild with `cargo build`."
    ))
  }

  /// Spawn the `WebKit` host subprocess and establish IPC communication.
  ///
  /// # Errors
  ///
  /// Returns an error if the Unix socketpair cannot be created, the host binary
  /// cannot be found or spawned, or the subprocess fails to become ready within
  /// the probe timeout.
  pub async fn spawn(headless: bool) -> Result<(Self, std::process::Child), String> {
    use std::os::unix::io::IntoRawFd;

    let (parent_sock, child_sock) = std::os::unix::net::UnixStream::pair().map_err(|e| format!("socketpair: {e}"))?;
    let child_fd = child_sock.into_raw_fd();
    let exe = Self::resolve_host_binary()?;

    let child = Self::spawn_host_process(&exe, child_fd, headless)?;

    let read_sock = parent_sock.try_clone().map_err(|e| format!("clone: {e}"))?;
    let write_sock = parent_sock;

    let pending: Arc<StdMutex<FxHashMap<u64, oneshot::Sender<IpcResponse>>>> =
      Arc::new(StdMutex::new(FxHashMap::default()));
    let console_log: Arc<StdMutex<Vec<(String, String)>>> = Arc::new(StdMutex::new(Vec::new()));
    let dialog_log: Arc<StdMutex<Vec<(String, String, String)>>> = Arc::new(StdMutex::new(Vec::new()));
    let network_log: Arc<StdMutex<Vec<NetworkEvent>>> = Arc::new(StdMutex::new(Vec::new()));
    let route_handler: Arc<StdMutex<Option<RouteCallback>>> = Arc::new(StdMutex::new(None));
    let writer_for_reader = Arc::new(StdMutex::new(
      write_sock.try_clone().map_err(|e| format!("clone writer: {e}"))?,
    ));
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
    _headless: bool,
  ) -> Result<std::process::Child, String> {
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
      cmd.pre_exec(move || {
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
        .map_err(|e| format!("spawn webkit host ({}): {e}", exe.display()))?
    };
    Ok(child)
  }

  /// Spawn the background reader thread that processes incoming IPC frames.
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
            if let Ok(mut log) = ctx.console_log.lock() {
              log.push((level, text));
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
              log.push((id, method, url, res_type));
            }
            ctx.event_notify.notify_one();
            continue;
          },
          11 => {
            handle_route_request(rid, &payload, &ctx.route_handler, &ctx.writer);
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
  pub async fn send(&self, op: Op, payload: &[u8]) -> Result<IpcResponse, String> {
    #[allow(clippy::cast_possible_truncation)] // request IDs wrap around at u32::MAX, which is acceptable
    let rid = self.next_id.fetch_add(1, Ordering::Relaxed) as u32;
    let (tx, rx) = oneshot::channel();
    self
      .pending
      .lock()
      .map_err(|e| format!("pending lock poisoned: {e}"))?
      .insert(u64::from(rid), tx);
    {
      let mut w = self.writer.lock().map_err(|e| format!("writer lock poisoned: {e}"))?;
      frame_write(&mut *w, rid, op as u8, payload);
    }
    match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
      Ok(Ok(r)) => Ok(r),
      Ok(Err(_)) => Err("dropped".into()),
      Err(_) => {
        if let Ok(mut guard) = self.pending.lock() {
          guard.remove(&u64::from(rid));
        }
        Err("timeout".into())
      },
    }
  }

  /// Send an IPC frame with a single string payload.
  ///
  /// # Errors
  ///
  /// Returns an error if the underlying `send` call fails (timeout, channel dropped,
  /// or mutex poisoned).
  pub async fn send_str(&self, op: Op, s: &str) -> Result<IpcResponse, String> {
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
  pub async fn send_str_vid(&self, op: Op, s: &str, vid: u64) -> Result<IpcResponse, String> {
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
  pub async fn send_vid(&self, op: Op, vid: u64) -> Result<IpcResponse, String> {
    self.send(op, &vid.to_le_bytes()).await
  }

  /// Send an IPC frame with an empty payload.
  ///
  /// # Errors
  ///
  /// Returns an error if the underlying `send` call fails (timeout, channel dropped,
  /// or mutex poisoned).
  pub async fn send_empty(&self, op: Op) -> Result<IpcResponse, String> {
    self.send(op, &[]).await
  }
}
