#![allow(clippy::expect_used, clippy::unwrap_used)]
//! E2E test for `ferridriver bdd --ui`: spawns the built binary in UI
//! mode on a scratch feature directory, connects over the websocket,
//! drives a run, and validates the served trace artifact.
//!
//! Requires a built `ferridriver` binary (`FERRIDRIVER_BIN` or
//! `target/{debug,release}/ferridriver`) plus Chrome, exactly like the
//! `backends` suite.

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

struct KillOnDrop(Child);

impl Drop for KillOnDrop {
  fn drop(&mut self) {
    let _ = self.0.kill();
    let _ = self.0.wait();
  }
}

/// Write the scratch BDD project: one feature, one passing JS step.
fn write_scratch_project(root: &std::path::Path) {
  std::fs::create_dir_all(root.join("features")).expect("mkdir features");
  std::fs::create_dir_all(root.join("steps")).expect("mkdir steps");
  std::fs::write(
    root.join("features/smoke.feature"),
    "Feature: UI smoke\n  Scenario: blank page\n    Given a blank ui page\n",
  )
  .expect("write feature");
  std::fs::write(
    root.join("steps/steps.js"),
    "Given(\"a blank ui page\", async (world) => { await world.page.goto(\"about:blank\"); });\n",
  )
  .expect("write steps");
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
  let deadline = Instant::now() + Duration::from_secs(120);
  while Instant::now() < deadline {
    let Ok(line) = rx.recv_timeout(Duration::from_secs(1)) else {
      continue;
    };
    if let Some(idx) = line.find("http://127.0.0.1:") {
      let url = line[idx..].trim().to_string();
      // Keep draining stdout in the background for the child's lifetime.
      std::thread::spawn(move || while rx.recv().is_ok() {});
      return url;
    }
  }
  panic!("ferridriver bdd --ui never printed its URL");
}

type WsStream = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Next JSON text frame from the websocket (2-minute cap per frame).
async fn next_json(ws: &mut WsStream) -> serde_json::Value {
  loop {
    let frame = tokio::time::timeout(Duration::from_secs(120), ws.next())
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

/// Read the initial snapshot frames (test list + watch status) and
/// return the single scenario's id.
async fn read_snapshot_test_id(ws: &mut WsStream) -> String {
  let mut test_id = None;
  let mut saw_watch_status = false;
  for _ in 0..4 {
    let msg = next_json(ws).await;
    match msg["type"].as_str() {
      Some("testList") => {
        let tests = msg["suites"][0]["tests"].as_array().expect("suite tests");
        assert_eq!(tests.len(), 1, "one scenario discovered: {msg}");
        let id = tests[0]["id"].as_str().expect("test id").to_string();
        assert!(id.contains("smoke.feature"), "id carries the feature file: {id}");
        assert_eq!(tests[0]["status"].as_str(), Some("idle"));
        test_id = Some(id);
      },
      Some("watchStatus") => saw_watch_status = true,
      _ => {},
    }
    if test_id.is_some() && saw_watch_status {
      break;
    }
  }
  assert!(saw_watch_status, "watchStatus snapshot arrived");
  test_id.expect("testList snapshot arrived")
}

/// Fetch the trace artifact and validate CORS + the v8 first line.
async fn fetch_and_validate_trace(host: &str, url_path: &str) {
  let (trace_headers, trace_body) = http_get(host, url_path).await;
  assert!(
    trace_headers.starts_with("HTTP/1.1 200"),
    "trace fetch status: {trace_headers}"
  );
  assert!(
    trace_headers
      .to_ascii_lowercase()
      .contains("access-control-allow-origin: *"),
    "CORS header for trace.playwright.dev: {trace_headers}"
  );

  let mut archive = zip::ZipArchive::new(std::io::Cursor::new(trace_body)).expect("trace zip");
  let mut trace_text = String::new();
  archive
    .by_name("trace.trace")
    .expect("trace.trace entry")
    .read_to_string(&mut trace_text)
    .expect("read trace.trace");
  archive.by_name("trace.network").expect("trace.network entry");
  let lines: Vec<serde_json::Value> = trace_text
    .lines()
    .map(|l| serde_json::from_str(l).expect("json trace line"))
    .collect();
  let first = &lines[0];
  assert_eq!(first["type"].as_str(), Some("context-options"), "first line: {first}");
  assert_eq!(first["version"].as_u64(), Some(8), "first line: {first}");

  // The trace is recorded live by the library recorder: the BDD step
  // boundary and the protocol-level goto must both appear as actions
  // with a coherent timeline.
  let actions: Vec<&serde_json::Value> = lines.iter().filter(|l| l["type"] == "action").collect();
  let step_action = actions
    .iter()
    .find(|a| a["title"].as_str() == Some("Given a blank ui page"))
    .unwrap_or_else(|| panic!("step action in trace: {actions:?}"));
  assert!(
    step_action["endTime"].as_f64().unwrap_or(0.0) >= step_action["startTime"].as_f64().unwrap_or(f64::MAX),
    "step span times ordered: {step_action}"
  );
  let goto = actions
    .iter()
    .find(|a| a["method"].as_str() == Some("goto"))
    .unwrap_or_else(|| panic!("protocol goto action in trace: {actions:?}"));

  // Worker traces request DOM snapshots: the goto must carry snapshot
  // names resolving to frame-snapshot events.
  let snapshots: Vec<&serde_json::Value> = lines.iter().filter(|l| l["type"] == "frame-snapshot").collect();
  assert!(!snapshots.is_empty(), "frame-snapshot events in per-test trace");
  for kind in ["beforeSnapshot", "afterSnapshot"] {
    let name = goto[kind].as_str().unwrap_or_else(|| panic!("goto {kind}: {goto}"));
    assert!(
      snapshots
        .iter()
        .any(|f| f["snapshot"]["snapshotName"].as_str() == Some(name)),
      "{kind} {name} must resolve to a frame-snapshot"
    );
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn ui_mode_end_to_end() {
  let dir = tempfile::tempdir().expect("tempdir");
  write_scratch_project(dir.path());

  let mut child = Command::new(bin())
    .current_dir(dir.path())
    .args([
      "bdd",
      "--ui",
      "--ui-port",
      "0",
      "--headless",
      "--steps",
      "steps/*.js",
      "features/**/*.feature",
    ])
    .stdout(Stdio::piped())
    .stderr(Stdio::inherit())
    .spawn()
    .expect("spawn ferridriver bdd --ui");
  let stdout = child.stdout.take().expect("child stdout");
  let _guard = KillOnDrop(child);

  let url = wait_for_url(stdout);
  let host = url.strip_prefix("http://").expect("http url").to_string();

  // The index page serves the app shell.
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

  // Snapshot messages arrive first: the test list and the watch status.
  let test_id = read_snapshot_test_id(&mut ws).await;

  // Nothing runs until requested; runAll drives the suite.
  ws.send(Message::Text(r#"{"cmd":"runAll"}"#.into()))
    .await
    .expect("send runAll");

  let mut outcome = None;
  let totals = loop {
    let msg = next_json(&mut ws).await;
    match msg["type"].as_str() {
      Some("testFinished") => {
        assert_eq!(msg["id"].as_str(), Some(test_id.as_str()));
        outcome = Some(msg["outcome"].clone());
      },
      Some("runFinished") => break msg["totals"].clone(),
      _ => {},
    }
  };
  assert_eq!(totals["total"].as_u64(), Some(1), "totals: {totals}");
  assert_eq!(totals["passed"].as_u64(), Some(1), "totals: {totals}");
  assert_eq!(totals["failed"].as_u64(), Some(0), "totals: {totals}");

  let outcome = outcome.expect("testFinished outcome");
  assert_eq!(outcome["status"].as_str(), Some("passed"), "outcome: {outcome}");
  assert!(outcome["durationMs"].as_u64().is_some(), "outcome: {outcome}");

  // UI mode forces traces on: the trace attachment must exist and be
  // served (with CORS for trace.playwright.dev) as a v8 trace zip.
  let attachments = outcome["attachments"].as_array().expect("attachments");
  let trace = attachments
    .iter()
    .find(|a| a["name"].as_str() == Some("trace"))
    .unwrap_or_else(|| panic!("trace attachment present: {attachments:?}"));
  assert_eq!(trace["contentType"].as_str(), Some("application/zip"));
  let url_path = trace["urlPath"].as_str().expect("trace urlPath");
  assert!(url_path.starts_with("/artifact/"), "urlPath: {url_path}");

  fetch_and_validate_trace(&host, url_path).await;

  // Path traversal is rejected.
  let (traversal_headers, _) = http_get(&host, "/artifact/../Cargo.toml").await;
  assert!(
    traversal_headers.starts_with("HTTP/1.1 404"),
    "traversal must 404: {traversal_headers}"
  );
}
