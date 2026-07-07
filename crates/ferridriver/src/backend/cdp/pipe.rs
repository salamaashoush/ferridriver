//! Pipe transport for CDP — NUL-delimited JSON over Unix socketpair.
//!
//! Chrome's `--remote-debugging-pipe` uses fd 3/4 for CDP communication.
//! We create a Unix socketpair, dup to fd 3/4, and communicate over the parent end.
//! All dispatch logic (responses, nav waiters, lifecycle, broadcast) is in `CdpDispatcher`.

use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::transport::CdpDispatcher;
use crate::error::{FerriError, Result};

type BoxReader = Box<dyn AsyncRead + Send + Unpin>;
type BoxWriter = Box<dyn AsyncWrite + Send + Unpin>;

pub struct PipeTransport {
  write_tx: tokio::sync::mpsc::Sender<Vec<u8>>,
  dispatcher: Arc<CdpDispatcher>,
}

impl PipeTransport {
  /// Spawn a Chrome process with `--remote-debugging-pipe` and wire up transport.
  ///
  /// # Errors
  ///
  /// Returns an error if the Chrome process fails to launch or pipe setup fails.
  pub fn spawn(
    chromium_path: &str,
    user_data_dir: &Path,
    extra_flags: &[String],
  ) -> Result<(Self, tokio::process::Child)> {
    let mut command = tokio::process::Command::new(chromium_path);
    command.arg(format!("--user-data-dir={}", user_data_dir.display()));
    command.arg("--remote-debugging-pipe");
    for flag in extra_flags {
      command.arg(flag);
    }
    command.arg("--no-startup-window");
    command
      .stdin(std::process::Stdio::null())
      .stdout(std::process::Stdio::null())
      .stderr(std::process::Stdio::piped())
      .kill_on_drop(true);

    let (child, reader, writer) = spawn_with_pipes(&mut command, chromium_path)?;

    let dispatcher = Arc::new(CdpDispatcher::new());

    // Writer task: batches queued messages into single write_all syscall.
    let (write_tx, mut write_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    tokio::spawn(async move {
      let mut writer: BoxWriter = writer;
      let mut buf = Vec::with_capacity(8192);
      while let Some(first) = write_rx.recv().await {
        buf.clear();
        buf.extend_from_slice(&first);
        while let Ok(more) = write_rx.try_recv() {
          buf.extend_from_slice(&more);
        }
        if writer.write_all(&buf).await.is_err() {
          break;
        }
      }
    });

    // Reader task: reads NUL-delimited messages, dispatches via CdpDispatcher.
    //
    // Uses `BytesMut` + `memchr::memchr` instead of `Vec<u8>` + linear
    // `iter().position()` + `drain`. Two wins:
    //   1. `memchr` uses NEON on aarch64-darwin — 5–10x faster NUL scan
    //      vs the byte-by-byte loop on long buffers (the prior code
    //      scanned the whole 64KB ring on every iteration).
    //   2. `BytesMut::split_to(nul_pos + 1)` advances the read cursor
    //      without memmove. Prior `Vec::drain(..=nul_pos)` shifted
    //      remaining bytes — O(N²) for back-to-back small frames.
    //
    // Reads land into a fixed `tmp` stack buffer first (kept for
    // `AsyncRead::read` ergonomics), then are appended to `rx`. We
    // only memchr-scan the bytes we just appended (`from_idx`) so
    // each message frame's NUL costs O(message-length) lookup, not
    // O(buffer-length) — keeps the per-message dispatch cost flat
    // across read sizes.
    let dispatcher2 = dispatcher.clone();
    tokio::spawn(async move {
      let mut reader: BoxReader = reader;
      let mut rx = bytes::BytesMut::with_capacity(64 * 1024);
      #[allow(clippy::large_stack_arrays)]
      let mut tmp = [0u8; 32768];

      loop {
        let n = match reader.read(&mut tmp).await {
          Ok(0) | Err(_) => {
            // Pipe EOF / error — chrome exited. Drain every pending
            // oneshot so in-flight `send_command` awaits return with
            // `target_closed` instead of stalling until the 30s
            // response timeout. Without this, any close path that
            // SIGKILLs chrome while requests are in flight makes
            // every queued caller wait the full timeout.
            dispatcher2.fail_all_pending("CDP transport closed (chrome exited)");
            return;
          },
          Ok(n) => n,
        };
        let scan_from = rx.len();
        rx.extend_from_slice(&tmp[..n]);

        // Drain every complete (NUL-terminated) message currently in `rx`.
        let mut search_from = scan_from;
        while let Some(rel) = memchr::memchr(0, &rx[search_from..]) {
          let nul_pos = search_from + rel;
          if nul_pos > 0 {
            // Drop the NUL terminator before dispatching.
            let frame = rx.split_to(nul_pos + 1);
            dispatcher2.dispatch_message(&frame[..nul_pos]);
          } else {
            // Leading NUL (empty frame) — discard the byte.
            let _ = rx.split_to(1);
          }
          search_from = 0; // After split_to, indices reset.
        }
      }
    });

    let transport = Self { write_tx, dispatcher };
    Ok((transport, child))
  }
}

impl super::transport::CdpTransport for PipeTransport {
  #[tracing::instrument(skip(self, session_id, params), fields(method))]
  async fn send_command(
    &self,
    session_id: Option<&str>,
    method: &str,
    params: &serde_json::Value,
  ) -> Result<serde_json::Value> {
    let (id, data, rx) = self.dispatcher.build_command(session_id, method, params)?;
    if self.write_tx.send(data).await.is_err() {
      self.dispatcher.forget_pending(id);
      return Err(FerriError::target_closed(Some("Pipe writer closed".into())));
    }
    match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
      Ok(Ok(result)) => result,
      Ok(Err(_)) => Err(FerriError::Backend(format!("Response channel dropped for {method}"))),
      Err(_) => {
        self.dispatcher.forget_pending(id);
        Err(FerriError::timeout(format!("waiting for {method} response"), 30_000))
      },
    }
  }

  fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<std::sync::Arc<serde_json::Value>> {
    self.dispatcher.subscribe_events()
  }

  fn subscribe_event_method(
    &self,
    method: &'static str,
  ) -> tokio::sync::broadcast::Receiver<std::sync::Arc<serde_json::Value>> {
    self.dispatcher.subscribe_event_method(method)
  }

  fn subscribe_event_domain(
    &self,
    domain: &'static str,
  ) -> tokio::sync::broadcast::Receiver<std::sync::Arc<serde_json::Value>> {
    self.dispatcher.subscribe_event_domain(domain)
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

// ── Platform-specific pipe spawning ──

#[cfg(unix)]
fn spawn_with_pipes(
  command: &mut tokio::process::Command,
  _chromium_path: &str,
) -> Result<(tokio::process::Child, BoxReader, BoxWriter)> {
  use std::os::unix::io::IntoRawFd;

  let (parent_sock, child_sock) =
    std::os::unix::net::UnixStream::pair().map_err(|e| FerriError::Backend(format!("socketpair: {e}")))?;
  let child_fd = child_sock.into_raw_fd();

  #[allow(unsafe_code)]
  unsafe {
    command.pre_exec(move || {
      // Put the Chrome parent in its own session + process group so
      // `kill_process_group` can take down every renderer/GPU/zygote
      // helper together on teardown. See `backend::process`.
      libc::setsid();
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
    .map_err(|e| FerriError::Backend(format!("Failed to launch Chrome with --remote-debugging-pipe: {e}")))?;

  parent_sock
    .set_nonblocking(true)
    .map_err(|e| FerriError::Backend(format!("set_nonblocking: {e}")))?;
  let stream =
    tokio::net::UnixStream::from_std(parent_sock).map_err(|e| FerriError::Backend(format!("tokio stream: {e}")))?;
  let (reader, writer) = tokio::io::split(stream);
  Ok((child, Box::new(reader) as BoxReader, Box::new(writer) as BoxWriter))
}

#[cfg(windows)]
fn spawn_with_pipes(
  command: &mut tokio::process::Command,
  _chromium_path: &str,
) -> Result<(tokio::process::Child, BoxReader, BoxWriter)> {
  use std::os::windows::io::RawHandle;
  use std::ptr;

  let id = std::process::id();
  let ts = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or_default()
    .as_nanos();
  let pipe_in_name = format!(r"\\.\pipe\ferridriver-in-{id}-{ts}");
  let pipe_out_name = format!(r"\\.\pipe\ferridriver-out-{id}-{ts}");

  fn to_wide(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(s)
      .encode_wide()
      .chain(std::iter::once(0))
      .collect()
  }

  unsafe {
    use windows_sys::Win32::Foundation::*;
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::Storage::FileSystem::*;
    use windows_sys::Win32::System::Pipes::*;

    let mut sa = SECURITY_ATTRIBUTES {
      nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
      lpSecurityDescriptor: ptr::null_mut(),
      bInheritHandle: TRUE,
    };

    let server_in = CreateNamedPipeW(
      to_wide(&pipe_in_name).as_ptr(),
      PIPE_ACCESS_OUTBOUND | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
      PIPE_TYPE_BYTE | PIPE_WAIT,
      1,
      65536,
      65536,
      0,
      ptr::null_mut(),
    );
    if server_in == INVALID_HANDLE_VALUE {
      return Err(FerriError::backend("CreateNamedPipe failed for input pipe"));
    }

    let server_out = CreateNamedPipeW(
      to_wide(&pipe_out_name).as_ptr(),
      PIPE_ACCESS_INBOUND | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
      PIPE_TYPE_BYTE | PIPE_WAIT,
      1,
      65536,
      65536,
      0,
      ptr::null_mut(),
    );
    if server_out == INVALID_HANDLE_VALUE {
      CloseHandle(server_in);
      return Err(FerriError::backend("CreateNamedPipe failed for output pipe"));
    }

    let client_in = CreateFileW(
      to_wide(&pipe_in_name).as_ptr(),
      GENERIC_READ,
      0,
      &sa as *const SECURITY_ATTRIBUTES,
      OPEN_EXISTING,
      FILE_ATTRIBUTE_NORMAL,
      0 as HANDLE,
    );
    if client_in == INVALID_HANDLE_VALUE {
      CloseHandle(server_in);
      CloseHandle(server_out);
      return Err(FerriError::backend("CreateFile failed for Chrome input pipe"));
    }

    let client_out = CreateFileW(
      to_wide(&pipe_out_name).as_ptr(),
      GENERIC_WRITE,
      0,
      &sa as *const SECURITY_ATTRIBUTES,
      OPEN_EXISTING,
      FILE_ATTRIBUTE_NORMAL,
      0 as HANDLE,
    );
    if client_out == INVALID_HANDLE_VALUE {
      CloseHandle(server_in);
      CloseHandle(server_out);
      CloseHandle(client_in);
      return Err(FerriError::backend("CreateFile failed for Chrome output pipe"));
    }

    command.arg(format!(
      "--remote-debugging-io-pipes={},{}",
      client_in as u32, client_out as u32,
    ));

    let child = command
      .spawn()
      .map_err(|e| FerriError::Backend(format!("Failed to launch Chrome: {e}")))?;

    CloseHandle(client_in);
    CloseHandle(client_out);

    let reader = tokio::net::windows::named_pipe::NamedPipeServer::from_raw_handle(server_out as RawHandle)
      .map_err(|e| FerriError::Backend(format!("tokio NamedPipeServer (read): {e}")))?;

    let writer = tokio::net::windows::named_pipe::NamedPipeServer::from_raw_handle(server_in as RawHandle)
      .map_err(|e| FerriError::Backend(format!("tokio NamedPipeServer (write): {e}")))?;

    Ok((child, Box::new(reader) as BoxReader, Box::new(writer) as BoxWriter))
  }
}
