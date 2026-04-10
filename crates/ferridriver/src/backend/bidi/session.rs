//! BiDi session management: WebSocket connection, session creation, browser launch.
//!
//! The BiDi protocol connects directly via WebSocket -- no HTTP session endpoint.
//! Firefox exposes native BiDi at `ws://host:port/session`.
//! Chrome can also be used via chromedriver's BiDi endpoint.

use std::sync::Arc;

use serde_json::json;
use tracing::{debug, info};

use super::transport::BidiTransport;

/// A BiDi session -- holds the transport and session metadata.
#[derive(Clone)]
pub(crate) struct BidiSession {
  #[allow(dead_code)]
  pub session_id: String,
  pub transport: Arc<BidiTransport>,
  #[allow(dead_code)]
  pub browser_name: String,
  #[allow(dead_code)]
  pub browser_version: String,
}

/// Core BiDi events supported across browsers.
/// We subscribe to these individually (not all at once) to handle
/// browsers that don't support all event types.
const BIDI_CORE_EVENTS: &[&str] = &[
  "browsingContext.contextCreated",
  "browsingContext.contextDestroyed",
  "browsingContext.load",
  "browsingContext.domContentLoaded",
  "browsingContext.navigationStarted",
  "browsingContext.fragmentNavigated",
  "browsingContext.userPromptOpened",
  "browsingContext.userPromptClosed",
  "log.entryAdded",
  "network.beforeRequestSent",
  "network.responseStarted",
  "network.responseCompleted",
  "network.fetchError",
  "network.authRequired",
  "script.realmCreated",
  "script.realmDestroyed",
  "script.message",
];

/// Extended BiDi events (may not be supported in all browsers).
const BIDI_EXTENDED_EVENTS: &[&str] = &[
  "browsingContext.navigationCommitted",
  "browsingContext.navigationFailed",
  "browsingContext.navigationAborted",
  "browsingContext.downloadWillBegin",
  "browsingContext.downloadEnd",
  "browsingContext.historyUpdated",
  "input.fileDialogOpened",
];

impl BidiSession {
  /// Connect to a BiDi endpoint directly via WebSocket.
  ///
  /// This is the native BiDi approach:
  /// 1. Connect WebSocket to `ws://host:port/session`
  /// 2. Send `session.new` to create a session
  /// 3. Subscribe to all events
  pub async fn connect(ws_url: &str) -> Result<Self, String> {
    info!("Connecting BiDi session to {ws_url}");

    let transport = Arc::new(BidiTransport::connect(ws_url).await?);

    // Create a new session
    let result = transport
      .send_command(
        "session.new",
        json!({
          "capabilities": {}
        }),
      )
      .await?;

    let session_id = result
      .get("sessionId")
      .and_then(|v| v.as_str())
      .unwrap_or("unknown")
      .to_string();
    let capabilities = result.get("capabilities").cloned().unwrap_or(json!({}));
    let browser_name = capabilities
      .get("browserName")
      .and_then(|v| v.as_str())
      .unwrap_or("unknown")
      .to_string();
    let browser_version = capabilities
      .get("browserVersion")
      .and_then(|v| v.as_str())
      .unwrap_or("unknown")
      .to_string();

    debug!("BiDi session created: id={session_id}, browser={browser_name} {browser_version}");

    // Subscribe to core events (one round-trip)
    let core_events: Vec<&str> = BIDI_CORE_EVENTS.to_vec();
    transport
      .send_command("session.subscribe", json!({"events": core_events}))
      .await?;

    // Try subscribing to extended events (ignore failures for unsupported ones)
    let ext_events: Vec<&str> = BIDI_EXTENDED_EVENTS.to_vec();
    let _ = transport
      .send_command("session.subscribe", json!({"events": ext_events}))
      .await;

    info!("BiDi session ready: {browser_name} {browser_version}");
    Ok(Self {
      session_id,
      transport,
      browser_name,
      browser_version,
    })
  }

  /// Connect to a BiDi endpoint at the given port.
  /// Constructs `ws://127.0.0.1:{port}/session` and connects.
  #[allow(dead_code, reason = "public library API for external consumers")]
  pub async fn connect_to_port(port: u16) -> Result<Self, String> {
    Self::connect(&format!("ws://127.0.0.1:{port}/session")).await
  }

  /// Launch Firefox and create a BiDi session.
  ///
  /// Firefox natively supports BiDi: launch with `--remote-debugging-port`,
  /// read the BiDi WebSocket URL from stderr, connect directly.
  pub async fn launch_firefox(firefox_path: &str, headless: bool) -> Result<(Self, tokio::process::Child), String> {
    let profile_dir = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;

    let mut command = tokio::process::Command::new(firefox_path);
    command.arg("--remote-debugging-port").arg("0");
    command.arg("--profile").arg(profile_dir.path());
    command.arg("--no-remote");
    if headless {
      command.arg("--headless");
    }
    command.env("MOZ_CRASHREPORTER_DISABLE", "1");
    command
      .stdin(std::process::Stdio::null())
      .stdout(std::process::Stdio::null())
      .stderr(std::process::Stdio::piped());

    debug!("Launching Firefox for BiDi: {firefox_path}");
    let mut child = command.spawn().map_err(|e| format!("Firefox launch: {e}"))?;

    // Firefox prints "WebDriver BiDi listening on ws://127.0.0.1:PORT" to stderr
    let ws_url = discover_bidi_ws_url(&mut child).await?;
    debug!("Firefox BiDi endpoint: {ws_url}");

    let session = Self::connect(&ws_url).await?;

    // Keep the profile dir alive
    std::mem::forget(profile_dir);

    Ok((session, child))
  }

  /// Launch a browser and create a BiDi session.
  /// Currently supports Firefox (native BiDi). Chrome does not have built-in
  /// BiDi support -- use the CDP backend for Chrome instead.
  pub async fn launch(
    browser_path: &str,
    _flags: &[String],
    headless: bool,
  ) -> Result<(Self, tokio::process::Child), String> {
    let path_lower = browser_path.to_lowercase();
    if path_lower.contains("firefox") {
      Self::launch_firefox(browser_path, headless).await
    } else {
      Err(format!(
        "BiDi backend requires Firefox (found: {browser_path}). \
         Chrome does not have built-in BiDi support -- use the CDP backend for Chrome. \
         Set FIREFOX_PATH or install Firefox."
      ))
    }
  }

  /// End the BiDi session gracefully.
  #[allow(dead_code)]
  pub async fn end(&self) -> Result<(), String> {
    let _ = self.transport.send_command("session.end", json!({})).await;
    Ok(())
  }
}

/// Read Firefox stderr to find the BiDi WebSocket URL.
/// Firefox prints: "WebDriver BiDi listening on ws://127.0.0.1:PORT"
async fn discover_bidi_ws_url(child: &mut tokio::process::Child) -> Result<String, String> {
  use tokio::io::AsyncBufReadExt;

  let stderr = child.stderr.take().ok_or("Firefox: no stderr handle")?;
  let mut reader = tokio::io::BufReader::new(stderr);
  let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);

  let mut line = String::new();
  loop {
    if tokio::time::Instant::now() >= deadline {
      return Err("Timeout waiting for Firefox BiDi WebSocket URL in stderr".into());
    }

    line.clear();
    let read_result = tokio::time::timeout(std::time::Duration::from_secs(1), reader.read_line(&mut line)).await;

    match read_result {
      Ok(Ok(0)) => {
        // EOF
        if let Ok(Some(status)) = child.try_wait() {
          return Err(format!("Firefox exited during startup with status: {status}"));
        }
        continue;
      },
      Ok(Ok(_)) => {
        // Look for the BiDi WebSocket URL
        // Firefox prints: "WebDriver BiDi listening on ws://127.0.0.1:PORT"
        // The actual BiDi endpoint is at ws://host:port/session
        if let Some(pos) = line.find("ws://") {
          let mut ws_url = line[pos..].trim().to_string();
          // Ensure the URL ends with /session (Firefox's BiDi endpoint)
          if !ws_url.ends_with("/session") {
            if ws_url.ends_with('/') {
              ws_url.push_str("session");
            } else {
              ws_url.push_str("/session");
            }
          }
          return Ok(ws_url);
        }
      },
      Ok(Err(e)) => return Err(format!("Firefox stderr read error: {e}")),
      Err(_) => {
        // Timeout on this read, check if process died
        if let Ok(Some(status)) = child.try_wait() {
          return Err(format!("Firefox exited during startup with status: {status}"));
        }
      },
    }
  }
}
