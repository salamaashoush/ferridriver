//! WebSocket transport for CDP — same dispatch logic as pipe, different I/O.
//!
//! All message dispatch (responses, nav waiters, lifecycle tracking, broadcast)
//! is handled by the shared `CdpDispatcher`. This file only implements WebSocket I/O.

use futures::StreamExt;
use std::path::Path;
use std::sync::Arc;
use tokio_tungstenite::tungstenite::Message;

use super::transport::CdpDispatcher;
use crate::backend::process::ChildGroup;
use crate::error::{FerriError, Result};

pub struct WsTransport {
  write_tx: tokio::sync::mpsc::Sender<Message>,
  dispatcher: Arc<CdpDispatcher>,
  tasks: crate::backend::transport_tasks::TransportTasks,
  /// The browser's own `DevTools` endpoint this transport dialled.
  endpoint: Arc<str>,
}

impl WsTransport {
  /// Connect to a running Chrome instance via WebSocket.
  ///
  /// # Errors
  ///
  /// Returns an error if the WebSocket connection fails.
  pub async fn connect(ws_url: &str) -> Result<Self> {
    Box::pin(Self::connect_with_headers(ws_url, &rustc_hash::FxHashMap::default())).await
  }

  /// Connect with custom HTTP headers (Playwright's `connectOptions.headers`).
  ///
  /// # Errors
  ///
  /// Returns an error if `ws_url` is not a valid WebSocket request, a header name or value is invalid, or the
  /// connection fails.
  pub async fn connect_with_headers(ws_url: &str, headers: &rustc_hash::FxHashMap<String, String>) -> Result<Self> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::http;
    let mut request = ws_url
      .into_client_request()
      .map_err(|e| FerriError::Backend(format!("WebSocket request build: {e}")))?;
    for (key, value) in headers {
      let header_name = http::header::HeaderName::from_bytes(key.as_bytes())
        .map_err(|e| FerriError::Backend(format!("invalid header name '{key}': {e}")))?;
      let header_value = http::header::HeaderValue::from_str(value)
        .map_err(|e| FerriError::Backend(format!("invalid header value for '{key}': {e}")))?;
      request.headers_mut().insert(header_name, header_value);
    }
    let budget = crate::operation_budget::OperationBudget::current(30_000)?;
    let (ws_stream, _) = budget
      .wait(Box::pin(tokio_tungstenite::connect_async(request)))
      .await
      .map_err(|error| error.error("connecting CDP WebSocket"))?
      .map_err(|error| FerriError::backend(format!("WebSocket connect failed: {error}")))?;

    let (write, read) = ws_stream.split();
    let dispatcher = Arc::new(CdpDispatcher::new());

    let (write_tx, write_rx) = tokio::sync::mpsc::channel::<Message>(64);
    let (shutdown, stopping) = tokio::sync::watch::channel(false);
    let writer = tokio::spawn(crate::backend::transport_tasks::write_websocket(
      write, write_rx, stopping,
    ));

    let dispatcher2 = Arc::clone(&dispatcher);
    let reader_ended = shutdown.clone();
    let reader = tokio::spawn(async move {
      let mut read = read;
      while let Some(Ok(msg)) = read.next().await {
        let Message::Text(text) = msg else { continue };
        dispatcher2.dispatch_message(text.as_bytes());
      }
      // WebSocket closed — drain pending oneshots so in-flight
      // `send_command` awaits don't stall to the 30s response
      // timeout (see pipe.rs reader for the same bug fix).
      dispatcher2.fail_all_pending("CDP transport closed (websocket ended)");
      reader_ended.send_replace(true);
    });

    Ok(Self {
      write_tx,
      dispatcher,
      tasks: crate::backend::transport_tasks::TransportTasks::new(shutdown, vec![reader, writer]),
      endpoint: Arc::from(ws_url),
    })
  }

  /// The browser's own `DevTools` WebSocket endpoint.
  #[must_use]
  pub fn endpoint(&self) -> &str {
    &self.endpoint
  }

  /// Spawn Chrome with `--remote-debugging-port` and connect via WebSocket.
  ///
  /// The browser is owned by a [`ChildGroup`] from the moment it is
  /// spawned, so a launch that fails before the handshake still takes down
  /// everything the launched binary started, not only the leader.
  ///
  /// # Errors
  ///
  /// Returns an error if Chrome fails to launch or the WebSocket connection fails.
  pub async fn spawn(
    chromium_path: &str,
    user_data_dir: &Path,
    extra_flags: &[String],
    owns_user_data_dir: bool,
    env: &rustc_hash::FxHashMap<String, String>,
  ) -> Result<(Self, ChildGroup)> {
    let mut command = tokio::process::Command::new(chromium_path);
    command.envs(env);
    command.arg(format!("--user-data-dir={}", user_data_dir.display()));
    command.arg("--remote-debugging-port=0");
    for flag in extra_flags {
      command.arg(flag);
    }
    command.arg("--no-startup-window");
    command
      .stdin(std::process::Stdio::null())
      .stdout(std::process::Stdio::null())
      .stderr(std::process::Stdio::piped())
      .kill_on_drop(true);

    // Put Chrome into its own session+process group so helper subprocesses
    // (renderer/GPU/zygote) die together with the parent on teardown. See
    // `backend::process`.
    // SAFETY: `setsid` is async-signal-safe; the closure performs no
    // allocation and captures nothing. `pre_exec` is unsafe on tokio's
    // `Command` because arbitrary code runs post-fork; our closure is
    // trivially sound.
    #[cfg(unix)]
    #[expect(unsafe_code, reason = "pre_exec has no safe form")]
    unsafe {
      command.pre_exec(super::super::process::setsid_pre_exec());
    }

    // Chrome writes `DevToolsActivePort` on startup and removes it on a CLEAN
    // exit — so a browser that was killed (SIGKILL, a crash, `kill_on_drop`)
    // leaves the file behind pointing at a port nothing listens on any more.
    // Reading it below would then dial a dead endpoint and keep dialling it,
    // because the stale file never changes. Remove it before the launch so the
    // only file that can appear is the one this Chrome writes.
    let port_file = user_data_dir.join("DevToolsActivePort");
    let _ = std::fs::remove_file(&port_file);

    let mut child = crate::backend::process::spawn_exclusive(|| command.spawn())
      .map_err(|e| FerriError::Backend(format!("Chrome launch: {e}")))?;
    let stderr_tail = crate::backend::process::drain_child_stderr(&mut child);
    let mut group = ChildGroup::recorded(child, Some(user_data_dir), owns_user_data_dir);

    let ws_url = discover_ws_url(&port_file, &mut group, chromium_path, &stderr_tail).await?;

    let transport = Box::pin(Self::connect(&ws_url)).await?;
    Ok((transport, group))
  }
}

impl super::transport::CdpTransport for WsTransport {
  async fn close(&self) -> Result<()> {
    self.dispatcher.fail_all_pending("CDP transport closed");
    self.tasks.close().await
  }

  fn is_disconnected(&self) -> bool {
    self.dispatcher.is_disconnected()
  }

  fn closed(&self) -> futures::future::BoxFuture<'static, ()> {
    self.tasks.closed()
  }

  #[tracing::instrument(skip(self, session_id, params), fields(method))]
  async fn send_command(
    &self,
    session_id: Option<&str>,
    method: &str,
    params: &serde_json::Value,
  ) -> Result<serde_json::Value> {
    let budget = crate::operation_budget::OperationBudget::current(30_000)?;
    let (id, mut data, rx) = self.dispatcher.build_command(session_id, method, params)?;
    let mut pending = self.dispatcher.pending_command(id);
    if data.last() == Some(&0) {
      data.pop();
    }
    let text = String::from_utf8(data).map_err(|error| FerriError::backend(format!("UTF-8: {error}")))?;
    budget.send(&self.write_tx, Message::Text(text.into())).await?;
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

  fn take_session_dirty(&self, session_id: &str) -> super::transport::SessionDirty {
    self.dispatcher.take_session_dirty(session_id)
  }

  fn reset_session_dirty(&self, session_id: &str) {
    self.dispatcher.reset_session_dirty(session_id);
  }

  fn mark_pointer_dirty(&self, session_id: &str) {
    self.dispatcher.mark_pointer_dirty(session_id);
  }

  fn take_permissions_dirty(&self, browser_context_id: &str) -> bool {
    self.dispatcher.take_permissions_dirty(browser_context_id)
  }

  fn take_cookies_dirty(&self, browser_context_id: &str) -> bool {
    self.dispatcher.take_cookies_dirty(browser_context_id)
  }
}

async fn discover_ws_url(
  port_file: &Path,
  group: &mut ChildGroup,
  chromium_path: &str,
  stderr_tail: &crate::backend::process::StderrTail,
) -> Result<String> {
  let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
  loop {
    if tokio::time::Instant::now() >= deadline {
      return Err(FerriError::Backend(launch_failure_detail(
        &format!(
          "timed out after 10000ms waiting for {} to write {}",
          chromium_path,
          port_file.display()
        ),
        stderr_tail,
      )));
    }
    if let Ok(contents) = tokio::fs::read_to_string(port_file).await {
      let lines: Vec<&str> = contents.lines().collect();
      if lines.len() >= 2 {
        let port = lines[0].trim();
        let path = lines[1].trim();
        return Ok(format!("ws://127.0.0.1:{port}{path}"));
      }
    }
    // Observed without reaping: a launcher that forked the real browser
    // and exited leaves that browser in its process group, and the group
    // id stays ours to signal only while the leader is unreaped.
    if !group.is_running() {
      let status = group.wait().await?;
      return Err(FerriError::Backend(launch_failure_detail(
        &format!(
          "{chromium_path} exited with status {status} before writing {}",
          port_file.display()
        ),
        stderr_tail,
      )));
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
  }
}

/// A launch failure with everything needed to act on it: what we were
/// waiting for, whatever the browser said, and — when the message is the
/// signature of an enterprise policy — the way out.
fn launch_failure_detail(what: &str, stderr_tail: &crate::backend::process::StderrTail) -> String {
  let mut msg = format!("Chrome launch: {what}");
  let lines = stderr_tail.lines();
  if let Some(context) = stderr_tail.as_error_context() {
    msg.push('\n');
    msg.push_str(&context);
  }
  if lines.iter().any(|l| l.contains("remote debugging is disallowed")) {
    msg.push_str(
      "\nThis browser is enrolled in cloud management and its policy forbids remote debugging, \
       so it will never open the port. Point at an unenrolled build instead: run \
       `ferridriver install chromium`, or set `executablePath` (per instance under \
       `[mcp.browser.instances.<name>]`) or the CHROMIUM_PATH environment variable.",
    );
  }
  msg
}

#[cfg(test)]
mod tests {
  use super::launch_failure_detail;
  use crate::backend::process::StderrTail;

  /// The launched binary is only the group's leader. When it exits before
  /// Chrome is reachable, the launch fails, and what it started must go
  /// with it rather than outlive the run with its profile deleted.
  #[cfg(unix)]
  #[tokio::test]
  async fn a_failed_launch_kills_what_the_exited_launcher_forked() {
    use crate::backend::process::test_support::{test_forking_launcher, test_process_ends, test_read_pid};
    let dir = tempfile::tempdir().expect("fixture dir");
    let (launcher, forked) = test_forking_launcher(dir.path());
    let profile = dir.path().join("profile");
    std::fs::create_dir(&profile).expect("profile dir");
    let outcome = super::WsTransport::spawn(
      &launcher.to_string_lossy(),
      &profile,
      &[],
      false,
      &rustc_hash::FxHashMap::default(),
    )
    .await;
    assert!(outcome.is_err(), "a launcher that exits must fail the launch");
    let forked = test_read_pid(&forked);
    assert!(
      test_process_ends(forked),
      "pid {forked}, forked by the launcher, outlived the failed launch"
    );
  }

  #[tokio::test(start_paused = true)]
  async fn operation_budgets_cover_replies_queueing_and_cancellation() {
    use super::{Arc, CdpDispatcher, Message, WsTransport};
    let dispatcher = Arc::new(CdpDispatcher::new());
    let (write_tx, queued) = tokio::sync::mpsc::channel(1);
    let transport = WsTransport {
      write_tx: write_tx.clone(),
      dispatcher: Arc::clone(&dispatcher),
      tasks: crate::backend::transport_tasks::TransportTasks::new(tokio::sync::watch::channel(false).0, Vec::new()),
      endpoint: Arc::from("ws://127.0.0.1:9/devtools/browser/test"),
    };
    super::super::transport::verify_operation_budgets(
      transport,
      dispatcher,
      write_tx,
      queued,
      Message::Text("queued".into()),
    )
    .await;
  }

  /// The whole point of capturing stderr: an enrolled Chrome refuses
  /// remote debugging with one line and otherwise starts normally, so the
  /// timeout alone tells the operator nothing about what to do next.
  #[test]
  fn a_policy_refusal_is_quoted_and_answered() {
    let tail = StderrTail::default();
    tail.record("DevTools remote debugging is disallowed by the system admin.".to_string());

    let msg = launch_failure_detail("timed out after 10000ms", &tail);
    assert!(
      msg.contains("disallowed by the system admin"),
      "must quote the browser: {msg}"
    );
    assert!(
      msg.contains("ferridriver install chromium"),
      "must name the way out: {msg}"
    );
    assert!(msg.contains("executablePath"), "must name the config key: {msg}");
  }

  /// An ordinary failure must not be dressed up as a policy problem.
  #[test]
  fn an_unexplained_failure_gets_no_policy_advice() {
    let msg = launch_failure_detail("exited with status 1", &StderrTail::default());
    assert!(msg.contains("exited with status 1"));
    assert!(
      !msg.contains("install chromium"),
      "no policy hint without the signature: {msg}"
    );
    assert!(!msg.contains("browser stderr:"), "no empty stderr section: {msg}");
  }
}
