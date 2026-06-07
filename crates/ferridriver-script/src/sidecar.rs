//! Generic sidecar transport: spawn a long-lived child process and drive it
//! over fd 3 / fd 4 with NUL-delimited JSON — id-correlated request/response
//! plus server-pushed events. This is the runtime behind the `sidecars` JS
//! binding; it is transport-only and knows nothing about any particular
//! child. It mirrors how the WebKit/CDP backends talk
//! to their child processes (fd 3/4, `\0`-delimited JSON), exposed as a
//! reusable primitive for extension authors.
//!
//! Wire contract (the child's side of the deal):
//! - The child reads requests on **fd 3** and writes responses on **fd 4**
//!   (Chrome `--remote-debugging-pipe` convention). No flags are passed —
//!   the fd numbers ARE the contract.
//! - Frames are UTF-8 JSON terminated by a single `0x00` byte.
//! - Request:  `{"id": <u64>, "method": <string>, "params": <json>}`
//! - Response: `{"id": <u64>, "result": <json>}` or
//!   `{"id": <u64>, "error": {"code": <i64>, "message": <string>}}`
//! - A frame WITHOUT an `id` is an event: `{"method": <string>, "params": <json>}`.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, broadcast, oneshot};

/// Max bytes a single inbound frame may reach before the connection is
/// failed — guards against a runaway child writing an unterminated frame.
const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum SidecarError {
  #[error("sidecar spawn failed: {0}")]
  Spawn(String),
  #[error("sidecar closed before responding")]
  Closed,
  #[error("sidecar request timed out after {0}ms")]
  Timeout(u64),
  #[error("sidecar protocol error: {0}")]
  Protocol(String),
  /// A well-formed `{"error": {code, message}}` response from the child.
  #[error("{message}")]
  Remote { code: i64, message: String },
}

/// How to launch a sidecar. The command's `argv[0]` is the program; the rest
/// are its arguments. fd 3/4 are wired by the transport, not via argv.
#[derive(Clone, Debug)]
pub struct SidecarSpec {
  pub name: String,
  pub command: Vec<String>,
  pub env: Vec<(String, String)>,
  pub cwd: Option<String>,
  pub startup_timeout_ms: u64,
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, SidecarError>>>>>;

/// A live sidecar: owns the child process, the request/response sockets, and
/// the background reader task. Cheap to share behind an `Arc`.
pub struct Sidecar {
  name: String,
  child: Mutex<Child>,
  writer: Mutex<UnixStream>,
  next_id: AtomicU64,
  pending: Pending,
  events: broadcast::Sender<(String, Value)>,
  reader: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Sidecar {
  pub fn name(&self) -> &str {
    &self.name
  }

  /// Subscribe to events the child pushes (frames with no `id`). Each
  /// subscriber gets every event published after it subscribes.
  pub fn subscribe(&self) -> broadcast::Receiver<(String, Value)> {
    self.events.subscribe()
  }

  /// Spawn the child and complete a `ping`/handshake-free startup: the
  /// transport is ready as soon as the sockets are wired. The first
  /// [`Self::send`] proves the child is actually speaking.
  pub async fn connect(spec: &SidecarSpec) -> Result<Arc<Self>, SidecarError> {
    if spec.command.is_empty() {
      return Err(SidecarError::Spawn("empty command".into()));
    }
    // sp_in: parent writes requests -> child reads them as fd 3.
    // sp_out: child writes responses as fd 4 -> parent reads them.
    let (parent_in, child_in) = UnixStream::pair().map_err(|e| SidecarError::Spawn(e.to_string()))?;
    let (parent_out, child_out) = UnixStream::pair().map_err(|e| SidecarError::Spawn(e.to_string()))?;

    let child = spawn_child(spec, &child_in, &child_out)?;
    // Parent does not need the child's ends; closing them lets the child see
    // EOF when it exits and vice-versa.
    drop(child_in);
    drop(child_out);

    // `parent_in` carries requests to the child (fd 3); `parent_out` carries
    // responses back (fd 4). Each is half-duplex in use, so own the whole
    // stream rather than `io::split` (whose half-drops can shut the socket).
    let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
    let (events, _) = broadcast::channel(1024);

    let sidecar = Arc::new(Self {
      name: spec.name.clone(),
      child: Mutex::new(child),
      writer: Mutex::new(parent_in),
      next_id: AtomicU64::new(1),
      pending: pending.clone(),
      events: events.clone(),
      reader: Mutex::new(None),
    });

    let task = tokio::spawn(read_loop(parent_out, pending, events));
    *sidecar.reader.lock().await = Some(task);
    Ok(sidecar)
  }

  /// Send a request and await the matching response. `params` defaults to
  /// `{}` when `None`. Fails with [`SidecarError::Timeout`] after
  /// `timeout_ms` (0 = wait indefinitely), [`SidecarError::Closed`] if the
  /// child dies first, or [`SidecarError::Remote`] on an `{error}` reply.
  pub async fn send(&self, method: &str, params: Option<Value>, timeout_ms: u64) -> Result<Value, SidecarError> {
    let id = self.next_id.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = oneshot::channel();
    self.pending.lock().await.insert(id, tx);

    let frame = json!({ "id": id, "method": method, "params": params.unwrap_or_else(|| json!({})) });
    let mut bytes = serde_json::to_vec(&frame).map_err(|e| SidecarError::Protocol(e.to_string()))?;
    bytes.push(0);
    {
      let mut w = self.writer.lock().await;
      if let Err(e) = w.write_all(&bytes).await {
        self.pending.lock().await.remove(&id);
        return Err(SidecarError::Protocol(format!("write: {e}")));
      }
      let _ = w.flush().await;
    }

    let recv = async {
      match rx.await {
        Ok(result) => result,
        Err(_) => Err(SidecarError::Closed),
      }
    };
    if timeout_ms == 0 {
      recv.await
    } else if let Ok(r) = tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), recv).await {
      r
    } else {
      self.pending.lock().await.remove(&id);
      Err(SidecarError::Timeout(timeout_ms))
    }
  }

  /// Send N requests as one batch: all frames are written in a single
  /// `write_all` (one syscall instead of N), all ids registered under one
  /// `pending` lock, then every response is awaited together. Responses are
  /// still id-correlated by `read_loop`, so the child may answer in any
  /// order. Results are returned positionally (`results[i]` is the reply to
  /// `calls[i]`); a per-call `{error}` reply becomes `Err` in that slot
  /// without failing the batch. `timeout_ms` (0 = wait indefinitely) bounds
  /// the whole batch; on expiry every still-unanswered slot is `Timeout`.
  ///
  /// This is the throughput path for issuing many calls from the single JS
  /// interpreter thread: it collapses the per-call promise/future/Promise.all
  /// machinery (and the per-call write syscall) that otherwise dominates the
  /// concurrent ceiling into one call.
  pub async fn send_many(
    &self,
    calls: Vec<(String, Option<Value>)>,
    timeout_ms: u64,
  ) -> Vec<Result<Value, SidecarError>> {
    if calls.is_empty() {
      return Vec::new();
    }
    let n = calls.len();
    // One slot per input, in order: a registered receiver, or an immediate
    // error (encode failure). Keeps `results[i]` aligned to `calls[i]`.
    let mut slots: Vec<Result<oneshot::Receiver<Result<Value, SidecarError>>, SidecarError>> = Vec::with_capacity(n);
    let mut ids = Vec::with_capacity(n);
    let mut buf: Vec<u8> = Vec::with_capacity(n * 64);
    {
      let mut pending = self.pending.lock().await;
      for (method, params) in calls {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let frame = json!({ "id": id, "method": method, "params": params.unwrap_or_else(|| json!({})) });
        let checkpoint = buf.len();
        match serde_json::to_writer(&mut buf, &frame) {
          Ok(()) => {
            buf.push(0);
            let (tx, rx) = oneshot::channel();
            pending.insert(id, tx);
            ids.push(id);
            slots.push(Ok(rx));
          },
          Err(e) => {
            buf.truncate(checkpoint);
            slots.push(Err(SidecarError::Protocol(e.to_string())));
          },
        }
      }
    }

    {
      let mut w = self.writer.lock().await;
      if let Err(e) = w.write_all(&buf).await {
        let mut pending = self.pending.lock().await;
        for id in &ids {
          pending.remove(id);
        }
        return (0..n)
          .map(|_| Err(SidecarError::Protocol(format!("write: {e}"))))
          .collect();
      }
      let _ = w.flush().await;
    }

    let gather = futures::future::join_all(slots.into_iter().map(|slot| async move {
      match slot {
        Ok(rx) => match rx.await {
          Ok(result) => result,
          Err(_) => Err(SidecarError::Closed),
        },
        Err(e) => Err(e),
      }
    }));
    if timeout_ms == 0 {
      gather.await
    } else if let Ok(results) = tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), gather).await {
      results
    } else {
      let mut pending = self.pending.lock().await;
      for id in &ids {
        pending.remove(id);
      }
      (0..n).map(|_| Err(SidecarError::Timeout(timeout_ms))).collect()
    }
  }

  /// Close the request socket and reap the child. Idempotent.
  pub async fn close(&self) -> Result<(), SidecarError> {
    // Dropping the writer's stream closes fd 3 for the child -> it sees EOF
    // and should exit. Then reap.
    {
      let mut w = self.writer.lock().await;
      let _ = w.shutdown().await;
    }
    if let Some(task) = self.reader.lock().await.take() {
      task.abort();
    }
    let mut child = self.child.lock().await;
    let _ = child.start_kill();
    let _ = child.wait().await;
    // Fail any stragglers.
    let mut pending = self.pending.lock().await;
    for (_, tx) in pending.drain() {
      let _ = tx.send(Err(SidecarError::Closed));
    }
    Ok(())
  }
}

#[cfg(unix)]
fn spawn_child(spec: &SidecarSpec, child_in: &UnixStream, child_out: &UnixStream) -> Result<Child, SidecarError> {
  use std::os::fd::AsRawFd;

  let mut cmd = Command::new(&spec.command[0]);
  cmd.args(&spec.command[1..]);
  for (k, v) in &spec.env {
    cmd.env(k, v);
  }
  if let Some(cwd) = &spec.cwd {
    cmd.current_dir(cwd);
  }
  cmd.kill_on_drop(true);

  let read_fd = child_in.as_raw_fd();
  let write_fd = child_out.as_raw_fd();
  // SAFETY: post-fork, single-threaded child; dup the child's ends onto the
  // conventional fd 3 (read) / fd 4 (write) and clear CLOEXEC so they
  // survive exec. Mirrors the WebKit launcher's `pre_exec_setup_fds`.
  #[allow(unsafe_code)]
  unsafe {
    cmd.pre_exec(move || {
      if libc::dup2(read_fd, 3) == -1 {
        return Err(std::io::Error::last_os_error());
      }
      if libc::dup2(write_fd, 4) == -1 {
        return Err(std::io::Error::last_os_error());
      }
      for fd in [3i32, 4] {
        // Survive exec.
        let descriptor_flags = libc::fcntl(fd, libc::F_GETFD);
        if descriptor_flags != -1 {
          libc::fcntl(fd, libc::F_SETFD, descriptor_flags & !libc::FD_CLOEXEC);
        }
        // Hand the child BLOCKING descriptors. The socket inherits the
        // parent's tokio O_NONBLOCK (shared status flags); a child reading
        // fd 3 synchronously would otherwise get spurious EWOULDBLOCK. An
        // async child re-enables non-blocking itself when it adopts the fd.
        let status_flags = libc::fcntl(fd, libc::F_GETFL);
        if status_flags != -1 {
          libc::fcntl(fd, libc::F_SETFL, status_flags & !libc::O_NONBLOCK);
        }
      }
      Ok(())
    });
  }
  cmd.spawn().map_err(|e| SidecarError::Spawn(e.to_string()))
}

#[cfg(not(unix))]
fn spawn_child(_spec: &SidecarSpec, _child_in: &UnixStream, _child_out: &UnixStream) -> Result<Child, SidecarError> {
  Err(SidecarError::Spawn(
    "sidecars require a unix fd-3/4 transport (not supported on this platform)".into(),
  ))
}

/// Read NUL-delimited JSON frames from the child, routing `{id,...}` to the
/// pending request and id-less frames to the event channel. Exits on EOF
/// (child gone) or a fatal protocol error, failing all pending requests.
async fn read_loop(mut reader: UnixStream, pending: Pending, events: broadcast::Sender<(String, Value)>) {
  let mut buf: Vec<u8> = Vec::with_capacity(8192);
  let mut chunk = [0u8; 8192];
  loop {
    let n = match reader.read(&mut chunk).await {
      Ok(0) | Err(_) => break, // EOF (child closed fd 4) or read error
      Ok(n) => n,
    };
    buf.extend_from_slice(&chunk[..n]);
    if buf.len() > MAX_FRAME_BYTES {
      break;
    }
    while let Some(pos) = buf.iter().position(|&b| b == 0) {
      let frame: Vec<u8> = buf.drain(..=pos).collect();
      let frame = &frame[..frame.len() - 1]; // strip the trailing NUL
      if frame.is_empty() {
        continue;
      }
      // Skip a malformed frame; never kill the loop over one bad message.
      if let Ok(v) = serde_json::from_slice::<Value>(frame) {
        route_frame(v, &pending, &events).await;
      }
    }
  }
  // Connection gone: fail every outstanding request.
  let mut p = pending.lock().await;
  for (_, tx) in p.drain() {
    let _ = tx.send(Err(SidecarError::Closed));
  }
}

async fn route_frame(mut v: Value, pending: &Pending, events: &broadcast::Sender<(String, Value)>) {
  if let Some(id) = v.get("id").and_then(Value::as_u64) {
    let Some(tx) = pending.lock().await.remove(&id) else {
      return; // late/unknown id
    };
    if let Some(err) = v.get("error") {
      let code = err.get("code").and_then(Value::as_i64).unwrap_or(-1);
      let message = err
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("sidecar error")
        .to_string();
      let _ = tx.send(Err(SidecarError::Remote { code, message }));
    } else {
      let result = v.get_mut("result").map_or(Value::Null, Value::take);
      let _ = tx.send(Ok(result));
    }
  } else if let Some(method) = v.get("method").and_then(Value::as_str).map(str::to_string) {
    let params = v.get_mut("params").map_or(Value::Null, Value::take);
    let _ = events.send((method, params));
  }
}
