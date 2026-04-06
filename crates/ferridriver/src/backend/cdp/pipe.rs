//! Pipe transport for CDP — NUL-delimited JSON over Unix socketpair.
//!
//! Chrome's `--remote-debugging-pipe` uses fd 3/4 for CDP communication.
//! We create a Unix socketpair, dup to fd 3/4, and communicate over the parent end.
//! All dispatch logic (responses, nav waiters, lifecycle, broadcast) is in `CdpDispatcher`.

use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::transport::CdpDispatcher;

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
  ) -> Result<(Self, tokio::process::Child), String> {
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
      .stderr(std::process::Stdio::piped());

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
    let dispatcher2 = dispatcher.clone();
    tokio::spawn(async move {
      let mut reader: BoxReader = reader;
      let mut rx = Vec::with_capacity(64 * 1024);
      #[allow(clippy::large_stack_arrays)]
      let mut tmp = [0u8; 32768];

      loop {
        let n = match reader.read(&mut tmp).await {
          Ok(0) | Err(_) => return,
          Ok(n) => n,
        };
        rx.extend_from_slice(&tmp[..n]);

        while let Some(nul_pos) = rx.iter().position(|&b| b == 0) {
          if nul_pos == 0 {
            rx.drain(..1);
            continue;
          }
          let raw = &rx[..nul_pos];
          dispatcher2.dispatch_message(raw);
          rx.drain(..=nul_pos);
        }
      }
    });

    let transport = Self { write_tx, dispatcher };
    Ok((transport, child))
  }
}

impl super::transport::CdpTransport for PipeTransport {
  async fn send_command(
    &self,
    session_id: Option<&str>,
    method: &str,
    params: serde_json::Value,
  ) -> Result<serde_json::Value, String> {
    let (data, rx) = self.dispatcher.build_command(session_id, method, &params)?;
    self.write_tx.send(data).await.map_err(|_| "Pipe writer closed".to_string())?;
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

// ── Platform-specific pipe spawning ──

#[cfg(unix)]
fn spawn_with_pipes(
  command: &mut tokio::process::Command,
  _chromium_path: &str,
) -> Result<(tokio::process::Child, BoxReader, BoxWriter), String> {
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
  Ok((child, Box::new(reader) as BoxReader, Box::new(writer) as BoxWriter))
}

#[cfg(windows)]
fn spawn_with_pipes(
  command: &mut tokio::process::Command,
  _chromium_path: &str,
) -> Result<(tokio::process::Child, BoxReader, BoxWriter), String> {
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
    std::ffi::OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
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
      1, 65536, 65536, 0, ptr::null_mut(),
    );
    if server_in == INVALID_HANDLE_VALUE {
      return Err("CreateNamedPipe failed for input pipe".into());
    }

    let server_out = CreateNamedPipeW(
      to_wide(&pipe_out_name).as_ptr(),
      PIPE_ACCESS_INBOUND | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
      PIPE_TYPE_BYTE | PIPE_WAIT,
      1, 65536, 65536, 0, ptr::null_mut(),
    );
    if server_out == INVALID_HANDLE_VALUE {
      CloseHandle(server_in);
      return Err("CreateNamedPipe failed for output pipe".into());
    }

    let client_in = CreateFileW(
      to_wide(&pipe_in_name).as_ptr(),
      GENERIC_READ, 0, &sa as *const SECURITY_ATTRIBUTES, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, 0 as HANDLE,
    );
    if client_in == INVALID_HANDLE_VALUE {
      CloseHandle(server_in);
      CloseHandle(server_out);
      return Err("CreateFile failed for Chrome input pipe".into());
    }

    let client_out = CreateFileW(
      to_wide(&pipe_out_name).as_ptr(),
      GENERIC_WRITE, 0, &sa as *const SECURITY_ATTRIBUTES, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, 0 as HANDLE,
    );
    if client_out == INVALID_HANDLE_VALUE {
      CloseHandle(server_in);
      CloseHandle(server_out);
      CloseHandle(client_in);
      return Err("CreateFile failed for Chrome output pipe".into());
    }

    command.arg(format!(
      "--remote-debugging-io-pipes={},{}",
      client_in as u32, client_out as u32,
    ));

    let child = command
      .spawn()
      .map_err(|e| format!("Failed to launch Chrome: {e}"))?;

    CloseHandle(client_in);
    CloseHandle(client_out);

    let reader = tokio::net::windows::named_pipe::NamedPipeServer::from_raw_handle(
      server_out as RawHandle,
    ).map_err(|e| format!("tokio NamedPipeServer (read): {e}"))?;

    let writer = tokio::net::windows::named_pipe::NamedPipeServer::from_raw_handle(
      server_in as RawHandle,
    ).map_err(|e| format!("tokio NamedPipeServer (write): {e}"))?;

    Ok((child, Box::new(reader) as BoxReader, Box::new(writer) as BoxWriter))
  }
}
