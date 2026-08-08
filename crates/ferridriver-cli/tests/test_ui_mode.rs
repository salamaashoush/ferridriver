#![allow(clippy::expect_used, clippy::unwrap_used)]
//! E2E test for `ferridriver test --ui`: spawns the built binary in UI
//! mode against the workspace's `rust-e2e-example` ferritest harness,
//! connects over the websocket, waits for the list cycle to populate
//! the sidebar, drives a single test, and validates the streamed
//! lifecycle plus the served trace artifact.
//!
//! Requires a built `ferridriver` binary (`FERRIDRIVER_BIN` or
//! `target/{debug,release}/ferridriver`), Chrome, and cargo — each
//! cycle spawns `cargo test -p rust-e2e-example` inside the workspace.

use std::io::{BufRead, BufReader, Read as _};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::Message;

fn bin() -> String {
  std::env::var("FERRIDRIVER_BIN").unwrap_or_else(|_| {
    let base = format!("{}/../../target", env!("CARGO_MANIFEST_DIR"));
    let debug = format!("{base}/debug/ferridriver");
    if std::path::Path::new(&debug).exists() {
      debug
    } else {
      format!("{base}/release/ferridriver")
    }
  })
}

fn workspace_root() -> std::path::PathBuf {
  std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("../..")
    .canonicalize()
    .expect("workspace root")
}

struct KillOnDrop(Child);

impl Drop for KillOnDrop {
  fn drop(&mut self) {
    // SIGTERM first: the CLI's graceful path kills any in-flight cycle's
    // process group and removes its unix socket file. SIGKILL after a
    // grace period as backstop.
    let pid = i32::try_from(self.0.id()).unwrap_or(0);
    if pid > 0 {
      // SAFETY: kill(2) with a positive pid signals only the CLI child.
      #[allow(unsafe_code)]
      unsafe {
        libc::kill(pid, libc::SIGTERM);
      }
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
      match self.0.try_wait() {
        Ok(Some(_)) => return,
        Ok(None) => std::thread::sleep(Duration::from_millis(50)),
        Err(_) => break,
      }
    }
    let _ = self.0.kill();
    let _ = self.0.wait();
  }
}

/// Wait for the child to print its `http://127.0.0.1:<port>` URL. The
/// reader thread keeps draining stdout afterwards so the child never
/// blocks on a full pipe.
fn wait_for_url(stdout: std::process::ChildStdout) -> String {
  let (tx, rx) = std::sync::mpsc::channel::<String>();
  std::thread::spawn(move || {
    let reader = BufReader::new(stdout);
    for line in reader.lines() {
      let Ok(line) = line else { break };
      let _ = tx.send(line);
    }
  });
  let deadline = Instant::now() + Duration::from_mins(2);
  while Instant::now() < deadline {
    let Ok(line) = rx.recv_timeout(Duration::from_secs(1)) else {
      continue;
    };
    if let Some(idx) = line.find("http://127.0.0.1:") {
      let url = line[idx..].trim().to_string();
      std::thread::spawn(move || while rx.recv().is_ok() {});
      return url;
    }
  }
  panic!("ferridriver test --ui never printed its URL");
}

type WsStream = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Next JSON text frame (generous cap — list cycles compile cargo
/// targets before any hello arrives).
async fn next_json(ws: &mut WsStream) -> serde_json::Value {
  loop {
    let frame = tokio::time::timeout(Duration::from_mins(10), ws.next())
      .await
      .expect("websocket frame timeout")
      .expect("websocket closed")
      .expect("websocket error");
    if let Message::Text(text) = frame {
      return serde_json::from_str(&text).expect("valid JSON frame");
    }
  }
}

/// Minimal HTTP/1.1 GET over a raw socket; returns (headers, body).
async fn http_get(host: &str, path: &str) -> (String, Vec<u8>) {
  let mut stream = tokio::net::TcpStream::connect(host).await.expect("connect");
  let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
  stream.write_all(request.as_bytes()).await.expect("send request");
  let mut response = Vec::new();
  stream.read_to_end(&mut response).await.expect("read response");
  let split = response
    .windows(4)
    .position(|w| w == b"\r\n\r\n")
    .expect("header/body separator");
  let headers = String::from_utf8_lossy(&response[..split]).to_string();
  (headers, response[split + 4..].to_vec())
}

/// Drain frames until the harness sidebar arrives (list cycle finished
/// compiling + helloing) AND the bridge is idle again. Returns the id
/// of the target test.
async fn wait_for_sidebar(ws: &mut WsStream, needle: &str) -> String {
  let mut test_id = None;
  let mut idle = false;
  let deadline = Instant::now() + Duration::from_mins(10);
  while Instant::now() < deadline {
    let msg = next_json(ws).await;
    match msg["type"].as_str() {
      Some("testList") => {
        for suite in msg["suites"].as_array().cloned().unwrap_or_default() {
          for test in suite["tests"].as_array().cloned().unwrap_or_default() {
            let id = test["id"].as_str().unwrap_or_default();
            if id.contains(needle) {
              test_id = Some(id.to_string());
            }
          }
        }
      },
      Some("watchStatus") => idle = msg["status"].as_str() == Some("idle"),
      _ => {},
    }
    if idle && let Some(id) = test_id {
      return id;
    }
  }
  panic!("sidebar never listed a test containing {needle:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn test_ui_mode_end_to_end() {
  let root = workspace_root();
  let mut child = Command::new(bin())
    .current_dir(&root)
    .args(["rust-test", "--ui", "--headless", "-p", "rust-e2e-example"])
    .stdout(Stdio::piped())
    .stderr(Stdio::inherit())
    .spawn()
    .expect("spawn ferridriver test --ui");
  let stdout = child.stdout.take().expect("child stdout");
  let _guard = KillOnDrop(child);

  let url = wait_for_url(stdout);
  let host = url.strip_prefix("http://").expect("http url").to_string();

  let (index_headers, index_body) = http_get(&host, "/").await;
  assert!(
    index_headers.starts_with("HTTP/1.1 200"),
    "index status: {index_headers}"
  );
  assert!(
    String::from_utf8_lossy(&index_body).contains("ferridriver UI"),
    "index page must be the UI shell"
  );

  let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{host}/ws"))
    .await
    .expect("websocket connect");

  // The initial list cycle compiles the example harness and hellos its
  // full plan; the sidebar must list the target test with idle status.
  let test_id = wait_for_sidebar(&mut ws, "lists_seeded_users").await;

  // Drive exactly one test.
  let run_test = serde_json::json!({ "cmd": "runTest", "id": test_id });
  ws.send(Message::Text(run_test.to_string().into()))
    .await
    .expect("send runTest");

  let mut saw_run_started = false;
  let mut live_trace_url = None;
  let mut outcome = None;
  let totals = loop {
    let msg = next_json(&mut ws).await;
    match msg["type"].as_str() {
      Some("runStarted") => {
        saw_run_started = true;
        assert_eq!(msg["totalTests"].as_u64(), Some(1), "runStarted: {msg}");
      },
      Some("testStarted") if msg["id"].as_str() == Some(test_id.as_str()) => {
        live_trace_url = msg["liveTraceUrl"].as_str().map(str::to_string);
      },
      Some("testFinished") if msg["id"].as_str() == Some(test_id.as_str()) => {
        outcome = Some(msg["outcome"].clone());
      },
      Some("runFinished") => break msg["totals"].clone(),
      _ => {},
    }
  };
  assert!(saw_run_started, "bridge must announce the run boundary");

  // Cross-process live traces: the harness serves its own /live-trace
  // endpoint, so the announced URL must be absolute (the CLI's server
  // cannot export another process's recorder spool).
  let live_trace_url = live_trace_url.expect("testStarted must announce a liveTraceUrl");
  assert!(
    live_trace_url.starts_with("http://127.0.0.1:"),
    "harness-hosted live trace URL: {live_trace_url}"
  );
  assert!(
    live_trace_url.contains("/live-trace?key="),
    "live trace URL shape: {live_trace_url}"
  );

  assert_eq!(totals["total"].as_u64(), Some(1), "totals: {totals}");
  assert_eq!(totals["passed"].as_u64(), Some(1), "totals: {totals}");
  assert_eq!(totals["failed"].as_u64(), Some(0), "totals: {totals}");

  let outcome = outcome.expect("testFinished outcome");
  assert_eq!(outcome["status"].as_str(), Some("passed"), "outcome: {outcome}");

  // UI mode forces traces on in the harness; the attachment must be
  // served by the CLI's artifact route even though a separate process
  // wrote it (FERRITEST_UI_ARTIFACTS redirects harness output).
  let attachments = outcome["attachments"].as_array().expect("attachments");
  let trace = attachments
    .iter()
    .find(|a| a["name"].as_str() == Some("trace"))
    .unwrap_or_else(|| panic!("trace attachment present: {attachments:?}"));
  let url_path = trace["urlPath"].as_str().expect("trace urlPath");
  assert!(url_path.starts_with("/artifact/"), "urlPath: {url_path}");

  validate_trace_zip(&host, url_path).await;
  stop_mid_run(&mut ws, &host).await;
}

/// Fetch the trace attachment through the CLI's artifact route and
/// validate it is a v8 trace with action events.
async fn validate_trace_zip(host: &str, url_path: &str) {
  let (trace_headers, trace_body) = http_get(host, url_path).await;
  assert!(
    trace_headers.starts_with("HTTP/1.1 200"),
    "trace fetch status: {trace_headers}"
  );
  let mut archive = zip::ZipArchive::new(std::io::Cursor::new(trace_body)).expect("trace zip");
  let mut trace_text = String::new();
  archive
    .by_name("trace.trace")
    .expect("trace.trace entry")
    .read_to_string(&mut trace_text)
    .expect("read trace.trace");
  let first: serde_json::Value = serde_json::from_str(trace_text.lines().next().expect("first line")).expect("json");
  assert_eq!(first["type"].as_str(), Some("context-options"), "first line: {first}");
  assert_eq!(first["version"].as_u64(), Some(8), "first line: {first}");
  assert!(
    trace_text.lines().any(|line| line.contains("\"type\":\"before\"")),
    "trace carries action events"
  );
}

/// Stop mid-run: kick off the full suite, wait until a test is actually
/// executing, then Stop. The bridge must kill the cycle's process
/// group, broadcast runCancelled (no runFinished follows), and return
/// to idle with the server still serving.
async fn stop_mid_run(ws: &mut WsStream, host: &str) {
  ws.send(Message::Text(r#"{"cmd":"runAll"}"#.into()))
    .await
    .expect("send runAll");
  loop {
    let msg = next_json(ws).await;
    if msg["type"].as_str() == Some("testStarted") {
      break;
    }
  }
  ws.send(Message::Text(r#"{"cmd":"stop"}"#.into()))
    .await
    .expect("send stop");
  let mut cancelled = false;
  loop {
    let msg = next_json(ws).await;
    match msg["type"].as_str() {
      Some("runCancelled") => cancelled = true,
      Some("runFinished") => panic!("no runFinished may follow a Stop"),
      Some("watchStatus") if msg["status"].as_str() == Some("idle") => break,
      _ => {},
    }
  }
  assert!(cancelled, "Stop must broadcast runCancelled before going idle");
  let (headers, _) = http_get(host, "/").await;
  assert!(
    headers.starts_with("HTTP/1.1 200"),
    "server must survive the group kill: {headers}"
  );
}
