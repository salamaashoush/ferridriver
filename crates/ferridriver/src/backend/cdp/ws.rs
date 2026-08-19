//! WebSocket transport for CDP — same dispatch logic as pipe, different I/O.
//!
//! All message dispatch (responses, nav waiters, lifecycle tracking, broadcast)
//! is handled by the shared `CdpDispatcher`. This file only implements WebSocket I/O.

use futures::{SinkExt, StreamExt};
use std::path::Path;
use std::sync::Arc;
use tokio_tungstenite::tungstenite::Message;

use super::transport::CdpDispatcher;
use crate::error::{FerriError, Result};

pub struct WsTransport {
  write_tx: tokio::sync::mpsc::Sender<Message>,
  dispatcher: Arc<CdpDispatcher>,
}

impl WsTransport {
  /// Connect to a running Chrome instance via WebSocket.
  ///
  /// # Errors
  ///
  /// Returns an error if the WebSocket connection fails.
  pub async fn connect(ws_url: &str) -> Result<Self> {
    Box::pin(Self::connect_with_headers(ws_url, &std::collections::HashMap::new())).await
  }

  /// Connect with custom HTTP headers (Playwright's `connectOptions.headers`).
  pub async fn connect_with_headers(ws_url: &str, headers: &std::collections::HashMap<String, String>) -> Result<Self> {
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
    let (ws_stream, _) = Box::pin(tokio_tungstenite::connect_async(request))
      .await
      .map_err(|e| FerriError::Backend(format!("WebSocket connect to {ws_url}: {e}")))?;

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
      // WebSocket closed — drain pending oneshots so in-flight
      // `send_command` awaits don't stall to the 30s response
      // timeout (see pipe.rs reader for the same bug fix).
      dispatcher2.fail_all_pending("CDP transport closed (websocket ended)");
    });

    Ok(Self { write_tx, dispatcher })
  }

  /// Spawn Chrome with `--remote-debugging-port` and connect via WebSocket.
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
  ) -> Result<(Self, tokio::process::Child)> {
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
    #[allow(unsafe_code)]
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

    let mut child = command
      .spawn()
      .map_err(|e| FerriError::Backend(format!("Chrome launch: {e}")))?;
    // Track before the port-file wait + websocket connect below.
    crate::backend::process::track_spawned(child.id().unwrap_or(0), Some(user_data_dir), owns_user_data_dir);
    let stderr_tail = crate::backend::process::drain_child_stderr(&mut child);

    let ws_url = discover_ws_url(&port_file, &mut child, chromium_path, &stderr_tail).await?;

    let transport = Box::pin(Self::connect(&ws_url)).await?;
    Ok((transport, child))
  }
}

impl super::transport::CdpTransport for WsTransport {
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
    let (id, mut data, rx) = self.dispatcher.build_command(session_id, method, params)?;
    // Remove NUL terminator — WebSocket doesn't need it
    if data.last() == Some(&0) {
      data.pop();
    }
    let text = match String::from_utf8(data) {
      Ok(t) => t,
      Err(e) => {
        self.dispatcher.forget_pending(id);
        return Err(FerriError::Backend(format!("UTF-8: {e}")));
      },
    };
    if self.write_tx.send(Message::Text(text.into())).await.is_err() {
      self.dispatcher.forget_pending(id);
      return Err(FerriError::backend("WS writer closed"));
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

  fn register_lifecycle_tracker(
    &self,
    session_id: &str,
    state: Arc<std::sync::Mutex<super::LifecycleState>>,
    notify: Arc<tokio::sync::Notify>,
  ) {
    self.dispatcher.register_lifecycle_tracker(session_id, state, notify);
  }

  fn unregister_session(&self, session_id: &str) {
    self.dispatcher.unregister_session(session_id);
  }
}

async fn discover_ws_url(
  port_file: &Path,
  child: &mut tokio::process::Child,
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
    if let Ok(Some(status)) = child.try_wait() {
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
