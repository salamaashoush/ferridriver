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
  tasks: crate::backend::transport_tasks::TransportTasks,
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
    owns_user_data_dir: bool,
    env: &rustc_hash::FxHashMap<String, String>,
  ) -> Result<(Self, tokio::process::Child)> {
    let mut command = tokio::process::Command::new(chromium_path);
    // Per-instance environment, merged onto the inherited one: an
    // instance may need a different proxy, locale or vendor-specific
    // variable than its siblings in the same process.
    command.envs(env);
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

    let (mut child, reader, writer) = spawn_with_pipes(&mut command, chromium_path)?;
    // Track before the handshake: a process killed between spawn and
    // `ChildGroup` would otherwise leave a browser nothing knows about.
    crate::backend::process::track_spawned(child.id().unwrap_or(0), Some(user_data_dir), owns_user_data_dir);
    crate::backend::process::drain_child_stderr(&mut child);

    let dispatcher = Arc::new(CdpDispatcher::new());

    // Writer task: batches queued messages into single write_all syscall.
    let (write_tx, mut write_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    let (shutdown, mut stopping) = tokio::sync::watch::channel(false);
    let writer_task = tokio::spawn(async move {
      let mut writer: BoxWriter = writer;
      let mut buf = Vec::with_capacity(8192);
      tokio::select! {
        biased;
        _ = stopping.wait_for(|stopping| *stopping) => {},
        () = async {
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
        } => {},
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
    let reader_ended = shutdown.clone();
    let reader_task = tokio::spawn(async move {
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
            reader_ended.send_replace(true);
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

    let transport = Self {
      write_tx,
      dispatcher,
      tasks: crate::backend::transport_tasks::TransportTasks::new(shutdown, vec![reader_task, writer_task]),
    };
    Ok((transport, child))
  }
}

impl super::transport::CdpTransport for PipeTransport {
  async fn close(&self) -> Result<()> {
    self.dispatcher.fail_all_pending("CDP transport closed");
    self.tasks.close().await
  }

  fn is_disconnected(&self) -> bool {
    self.dispatcher.is_disconnected()
  }

  #[tracing::instrument(skip(self, session_id, params), fields(method))]
  async fn send_command(
    &self,
    session_id: Option<&str>,
    method: &str,
    params: &serde_json::Value,
  ) -> Result<serde_json::Value> {
    let budget = crate::operation_budget::OperationBudget::current(30_000)?;
    let (id, data, rx) = self.dispatcher.build_command(session_id, method, params)?;
    let mut pending = self.dispatcher.pending_command(id);
    budget.send(&self.write_tx, data).await?;
    let result = budget
      .wait(rx)
      .await
      .map_err(|error| error.error(format!("waiting for {method} response")))?
      .map_err(|_| FerriError::backend(format!("Response channel dropped for {method}")))?;
    pending.completed();
    result
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

  fn tap_event_methods(
    &self,
    methods: &'static [&'static str],
    session_id: Option<&str>,
  ) -> tokio::sync::mpsc::UnboundedReceiver<std::sync::Arc<serde_json::Value>> {
    self.dispatcher.tap_event_methods(methods, session_id)
  }

  fn tap_event_domains(
    &self,
    domains: &'static [&'static str],
    session_id: Option<&str>,
  ) -> tokio::sync::mpsc::UnboundedReceiver<std::sync::Arc<serde_json::Value>> {
    self.dispatcher.tap_event_domains(domains, session_id)
  }

  fn tap_all_events(
    &self,
    session_id: &str,
  ) -> tokio::sync::mpsc::UnboundedReceiver<std::sync::Arc<serde_json::Value>> {
    self.dispatcher.tap_all_events(session_id)
  }

  fn tap_iframe_targets(&self) -> super::transport::IframeTargetSubscription {
    self.dispatcher.tap_iframe_targets()
  }

  fn register_lifecycle_tracker(
    &self,
    session_id: &str,
    state: Arc<std::sync::Mutex<super::LifecycleState>>,
    notify: Arc<tokio::sync::Notify>,
    frame_observer: super::transport::FrameStateObserver,
  ) {
    self
      .dispatcher
      .register_lifecycle_tracker(session_id, state, notify, frame_observer);
  }

  fn unregister_session(&self, session_id: &str) {
    self.dispatcher.unregister_session(session_id);
  }
}

// ── Platform-specific pipe spawning ──

#[cfg(unix)]
fn spawn_with_pipes(
  command: &mut tokio::process::Command,
  _chromium_path: &str,
) -> Result<(tokio::process::Child, BoxReader, BoxWriter)> {
  use std::os::unix::io::AsRawFd;

  let (parent_sock, child_sock) =
    std::os::unix::net::UnixStream::pair().map_err(|e| FerriError::Backend(format!("socketpair: {e}")))?;
  // Borrow the raw fd for `pre_exec`; ownership stays with `child_sock`
  // so the parent's copy is CLOSED (via Drop) right after `spawn()`.
  // Holding it open meant the reader could never see EOF when Chrome
  // died — a live in-process fd to the child end kept the socketpair
  // open, `fail_all_pending` never fired, and every in-flight command
  // ate the full 30s timeout instead of failing fast.
  let child_fd = child_sock.as_raw_fd();

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
  drop(child_sock);

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

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test(start_paused = true)]
  async fn operation_budgets_cover_replies_queueing_and_cancellation() {
    let dispatcher = Arc::new(CdpDispatcher::new());
    let (write_tx, queued) = tokio::sync::mpsc::channel(1);
    let transport = PipeTransport {
      write_tx: write_tx.clone(),
      dispatcher: dispatcher.clone(),
      tasks: crate::backend::transport_tasks::TransportTasks::new(tokio::sync::watch::channel(false).0, Vec::new()),
    };
    super::super::transport::verify_operation_budgets(transport, dispatcher, write_tx, queued, vec![0]).await;
  }

  fn context_test_page(transport: Arc<PipeTransport>, session: &str) -> super::super::CdpPage<PipeTransport> {
    use super::super::{AttachTasks, CdpBrowser};
    let browser = CdpBrowser {
      transport,
      child: Arc::default(),
      attached_targets: Arc::default(),
      version: Arc::from("Chrome/fixture"),
      isolated_contexts: true,
      headful: false,
      popup_taps: Arc::default(),
      create_ledger: Arc::default(),
      user_data_dir: None,
      downloads_dir: Arc::new(tempfile::tempdir().unwrap()),
      attach_tasks: Arc::new(AttachTasks(Vec::new())),
      webdriver: None,
      android: None,
    };
    browser.adopted_page("frame".into(), Some(session.into()))
  }

  #[tokio::test(flavor = "current_thread")]
  async fn evaluation_uses_context_events_that_precede_its_barrier_response() {
    use super::super::transport::CdpTransport as _;
    use serde_json::json;
    for session in ["page-session", "renderer-session"] {
      let dispatcher = Arc::new(CdpDispatcher::new());
      let (write_tx, mut queued) = tokio::sync::mpsc::channel(4);
      let transport = Arc::new(PipeTransport {
        write_tx,
        dispatcher: dispatcher.clone(),
        tasks: crate::backend::transport_tasks::TransportTasks::new(tokio::sync::watch::channel(false).0, Vec::new()),
      });
      let page = context_test_page(transport.clone(), session);
      transport.register_lifecycle_tracker(
        session,
        page.lifecycle.clone(),
        page.lifecycle_notify.clone(),
        page.frame_observer(),
      );
      let created = |session: &str, id| {
        json!({"sessionId":session, "method":"Runtime.executionContextCreated",
        "params":{"context":{"id":id,"auxData":{"frameId":"frame","isDefault":true}}}})
      };
      dispatcher.dispatch_message(&serde_json::to_vec(&created(session, 11)).unwrap());
      tokio::task::yield_now().await;
      assert_eq!(page.resolve_frame_context("frame").await.unwrap(), 11);
      let barrier_params = json!({"expression":"0"});
      let mut barrier = Box::pin(transport.send_command(Some(session), "Runtime.evaluate", &barrier_params));
      assert!(futures::poll!(&mut barrier).is_pending());
      let message = queued.try_recv().unwrap();
      let request: serde_json::Value = serde_json::from_slice(message.strip_suffix(&[0]).unwrap()).unwrap();
      for event in [
        json!({"sessionId":session,"method":"Runtime.executionContextDestroyed","params":{"executionContextId":11}}),
        created(session, 12),
        created("another-session", 99),
        json!({"id":request["id"],"result":{}}),
      ] {
        dispatcher.dispatch_message(&serde_json::to_vec(&event).unwrap());
      }
      assert!(matches!(futures::poll!(&mut barrier), std::task::Poll::Ready(Ok(_))));
      let context = page.resolve_frame_context("frame").await.unwrap();
      let mut evaluation =
        Box::pin(page.call_utility_in_context("() => 42", &[], &[], Some(context), Some(true), true));
      assert!(futures::poll!(&mut evaluation).is_pending());
      let message = queued.try_recv().unwrap();
      let request: serde_json::Value = serde_json::from_slice(message.strip_suffix(&[0]).unwrap()).unwrap();
      page.dispose_local();
      assert_eq!(request["method"], "Runtime.callFunctionOn");
      assert_eq!(request["sessionId"], session);
      assert_eq!(
        request["params"]["executionContextId"], 12,
        "first dispatch used an obsolete context"
      );
    }
  }

  #[tokio::test(flavor = "current_thread")]
  async fn existing_contexts_are_available_when_a_page_observer_attaches_late() {
    use super::super::transport::CdpTransport as _;
    use serde_json::json;
    let dispatcher = Arc::new(CdpDispatcher::new());
    let (write_tx, _queued) = tokio::sync::mpsc::channel(4);
    let transport = Arc::new(PipeTransport {
      write_tx,
      dispatcher: dispatcher.clone(),
      tasks: crate::backend::transport_tasks::TransportTasks::new(tokio::sync::watch::channel(false).0, Vec::new()),
    });
    let created = |session: &str, frame: &str, id, default| {
      json!({"sessionId":session,"method":"Runtime.executionContextCreated",
      "params":{"context":{"id":id,"auxData":{"frameId":frame,"isDefault":default}}}})
    };
    for event in [
      created("existing", "child", 11, true),
      created("existing", "frame", 20, true),
      created("existing", "child", 99, false),
      created("another", "child", 55, true),
      json!({"sessionId":"existing","method":"Runtime.executionContextDestroyed","params":{"executionContextId":11}}),
      created("existing", "child", 12, true),
      created("existing", "child", 77, false),
    ] {
      dispatcher.dispatch_message(&serde_json::to_vec(&event).unwrap());
    }
    let page = context_test_page(transport.clone(), "existing");
    transport.register_lifecycle_tracker(
      "existing",
      page.lifecycle.clone(),
      page.lifecycle_notify.clone(),
      page.frame_observer(),
    );
    let child = page.peek_frame_context("child");
    let main = page.peek_frame_context("frame");
    page.dispose_local();
    assert_eq!(
      child,
      Some(12),
      "existing child context was lost before observer registration"
    );
    assert_eq!(main, Some(20));
  }

  #[test]
  fn binding_source_is_captured_before_context_id_reuse() {
    use super::super::CdpPage;
    use serde_json::json;
    let contexts = Arc::new(std::sync::RwLock::new(rustc_hash::FxHashMap::default()));
    let notify = Arc::new(tokio::sync::Notify::new());
    let (sender, mut queued) = tokio::sync::mpsc::unbounded_channel();
    let created = |frame| {
      json!({"method":"Runtime.executionContextCreated",
      "params":{"context":{"id":11,"auxData":{"frameId":frame,"isDefault":true}}}})
    };
    for event in [
      created("child"),
      json!({"method":"Runtime.bindingCalled","params":{"name":"__fd_binding__","executionContextId":11,
        "payload":json!({"name":"report","seq":1,"args":["sashoush"]}).to_string()}}),
      json!({"method":"Runtime.executionContextsCleared","params":{}}),
      created("replacement"),
    ] {
      CdpPage::<PipeTransport>::handle_tracker_event(&event, &contexts, &notify, &Arc::from("main"), &sender);
    }
    let call = queued.try_recv().unwrap();
    assert_eq!(call.source.frame, "child");
    assert_eq!(call.source.page, "main");
    assert_eq!(call.ctx_id, Some(11));
    assert_eq!(call.args, vec![json!("sashoush")]);
    assert_eq!(contexts.read().unwrap().get("replacement"), Some(&11));
    assert!(!contexts.read().unwrap().contains_key("child"));
  }

  #[tokio::test(flavor = "current_thread")]
  async fn fetch_listener_captures_requests_before_its_task_is_polled()
  -> std::result::Result<(), Box<dyn std::error::Error>> {
    let dispatcher = Arc::new(CdpDispatcher::new());
    let (write_tx, mut write_rx) = tokio::sync::mpsc::channel(1);
    let transport = Arc::new(PipeTransport {
      write_tx,
      dispatcher: dispatcher.clone(),
      tasks: crate::backend::transport_tasks::TransportTasks::new(tokio::sync::watch::channel(false).0, Vec::new()),
    });
    let listener = super::super::CdpPage::spawn_fetch_listener(
      transport,
      Some(Arc::from("session")),
      Arc::new(tokio::sync::RwLock::new(Vec::new())),
      Arc::new(tokio::sync::RwLock::new(None)),
    );
    dispatcher.dispatch_message(&serde_json::to_vec(&serde_json::json!({
      "sessionId": "session", "method": "Fetch.requestPaused",
      "params": { "requestId": "first-request", "request": { "url": "http://example.com/" } }
    }))?);
    let command = tokio::time::timeout(std::time::Duration::from_secs(1), write_rx.recv()).await;
    listener.abort();
    let command = command?.ok_or("paused request was lost before the listener task started")?;
    let command: serde_json::Value = serde_json::from_slice(command.strip_suffix(&[0]).unwrap_or(&command))?;
    assert_eq!(command["method"], "Fetch.continueRequest");
    assert_eq!(command["params"]["requestId"], "first-request");
    dispatcher.dispatch_message(&serde_json::to_vec(
      &serde_json::json!({ "id": command["id"], "result": {} }),
    )?);
    Ok(())
  }

  #[tokio::test(flavor = "current_thread")]
  async fn network_listener_captures_events_before_its_task_is_polled()
  -> std::result::Result<(), Box<dyn std::error::Error>> {
    let dispatcher = Arc::new(CdpDispatcher::new());
    let (write_tx, _write_rx) = tokio::sync::mpsc::channel(1);
    let transport = Arc::new(PipeTransport {
      write_tx,
      dispatcher: dispatcher.clone(),
      tasks: crate::backend::transport_tasks::TransportTasks::new(tokio::sync::watch::channel(false).0, Vec::new()),
    });
    let log = Arc::new(tokio::sync::RwLock::new(Vec::new()));
    let navigation = crate::network::NavRequestSlot::new();
    let listener = super::super::CdpPage::spawn_network_listener(
      transport,
      Some(Arc::from("session")),
      Arc::from("page"),
      log.clone(),
      crate::events::EventEmitter::new(),
      navigation.clone(),
    );
    for event in [
      serde_json::json!({
        "sessionId": "session", "method": "Network.requestWillBeSent",
        "params": {"requestId": "document", "loaderId": "document", "type": "Document",
          "request": {"url": "http://example.com/", "method": "GET", "headers": {}}}
      }),
      serde_json::json!({
        "sessionId": "session", "method": "Network.responseReceived",
        "params": {"requestId": "document", "response": {
          "url": "http://example.com/", "status": 200, "statusText": "OK", "headers": {}}}
      }),
    ] {
      dispatcher.dispatch_message(&serde_json::to_vec(&event)?);
    }
    let response = tokio::time::timeout(std::time::Duration::from_secs(1), async {
      let request = navigation.wait(std::time::Duration::from_secs(1)).await?;
      request.response().await.ok().flatten()
    })
    .await;
    listener.abort();
    let response = response?.ok_or("initial navigation was lost before the listener task started")?;
    assert_eq!(response.status(), 200);
    assert_eq!(log.read().await.len(), 1);
    Ok(())
  }
}
