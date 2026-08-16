#![allow(clippy::expect_used, clippy::unwrap_used)]
//! `ferridriver bdd --ui` end to end, through the test-server protocol.
//!
//! Same app as `ferridriver test --ui` (Playwright's UI mode, embedded),
//! so this is that app's side of the conversation for a BDD suite:
//! scenarios are the tests, a run is driven over the websocket, and the
//! recorded trace is checked for the things the viewer actually reads —
//! the step span, the protocol call nested inside it, the DOM snapshots
//! around it, and the embedded sources its Source tab opens.
//!
//! Requires a built `ferridriver` binary (`FERRIDRIVER_BIN` or
//! `target/{debug,release}/ferridriver`) plus Chrome.

use std::io::{BufRead, BufReader, Read as _};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
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
    root.join("features/slow.feature"),
    "Feature: UI slow\n  Scenario: slow page\n    Given a slow ui step\n",
  )
  .expect("write slow feature");
  std::fs::write(
    root.join("steps/steps.js"),
    concat!(
      "Given(\"a blank ui page\", async (world) => { await world.page.goto(\"about:blank\"); });\n",
      // Long enough that a Stop reliably lands while the step is running.
      "Given(\"a slow ui step\", async (world) => { await world.page.waitForTimeout(6000); });\n",
    ),
  )
  .expect("write steps");
}

/// Wait for the child to print its URL, and keep draining stdout after —
/// a server blocked writing to a full pipe answers nothing.
fn wait_for_url(stdout: std::process::ChildStdout) -> String {
  let (tx, rx) = std::sync::mpsc::channel::<String>();
  std::thread::spawn(move || {
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
      let _ = tx.send(line);
    }
  });
  let deadline = Instant::now() + Duration::from_mins(2);
  while Instant::now() < deadline {
    let Ok(line) = rx.recv_timeout(Duration::from_secs(1)) else {
      continue;
    };
    if let Some(index) = line.find("http://127.0.0.1:") {
      let url = line[index..].trim().to_string();
      std::thread::spawn(move || while rx.recv().is_ok() {});
      return url;
    }
  }
  panic!("ferridriver bdd --ui never printed its URL");
}

type WsStream = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// The UI's side of the protocol: calls with replies, events collected.
struct Ui {
  ws: WsStream,
  next_id: u64,
  events: Vec<Value>,
}

impl Ui {
  async fn connect(url: &str) -> Self {
    let (base, query) = url.split_once("/trace/").expect("app url");
    let guid = query
      .split("ws=")
      .nth(1)
      .and_then(|rest| rest.split('&').next())
      .expect("ws parameter");
    let ws_url = format!("{}/{guid}", base.replace("http://", "ws://"));
    let (ws, _) = tokio_tungstenite::connect_async(&ws_url).await.expect("connect");
    Self {
      ws,
      next_id: 0,
      events: Vec::new(),
    }
  }

  async fn call(&mut self, method: &str, params: Value) -> Value {
    self.next_id += 1;
    let id = self.next_id;
    self
      .ws
      .send(Message::Text(
        json!({ "id": id, "method": method, "params": params })
          .to_string()
          .into(),
      ))
      .await
      .expect("send");
    loop {
      let value = self.next_message().await;
      if value.get("id").and_then(Value::as_u64) == Some(id) {
        assert!(value.get("error").is_none(), "{method} failed: {value}");
        return value.get("result").cloned().unwrap_or(Value::Null);
      }
    }
  }

  /// Send a call without waiting for its reply — a run, so events can be
  /// watched (and a Stop sent) while it is in flight.
  async fn send(&mut self, method: &str, params: Value) -> u64 {
    self.next_id += 1;
    let id = self.next_id;
    self
      .ws
      .send(Message::Text(
        json!({ "id": id, "method": method, "params": params })
          .to_string()
          .into(),
      ))
      .await
      .expect("send");
    id
  }

  async fn next_message(&mut self) -> Value {
    loop {
      let frame = tokio::time::timeout(Duration::from_mins(3), self.ws.next())
        .await
        .expect("frame timeout")
        .expect("socket closed")
        .expect("socket error");
      if let Message::Text(text) = frame {
        let value: Value = serde_json::from_str(&text).expect("json");
        if value.get("method").is_some() {
          self.events.push(value.clone());
        }
        return value;
      }
    }
  }

  /// Wait for a teleReporter event matching `method`, returning it.
  async fn wait_report(&mut self, method: &str) -> Value {
    if let Some(found) = self.reports_of(method).first() {
      return (*found).clone();
    }
    loop {
      let value = self.next_message().await;
      if value["method"] == "report" && value["params"]["method"] == method {
        return value["params"].clone();
      }
    }
  }

  fn reports_of(&self, method: &str) -> Vec<&Value> {
    self
      .events
      .iter()
      .filter(|event| event["method"] == "report" && event["params"]["method"] == method)
      .map(|event| &event["params"])
      .collect()
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn bdd_ui_lists_runs_and_traces_a_scenario() {
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
  assert!(url.contains("/trace/uiMode.html?ws="), "UI app url: {url}");
  let host = url
    .strip_prefix("http://")
    .and_then(|rest| rest.split('/').next())
    .expect("host")
    .to_string();

  // The app and its service worker are served offline by this same
  // process — no CDN, no npx.
  let (index_headers, index_body) = http_get(&host, "/trace/uiMode.html").await;
  assert!(index_headers.starts_with("HTTP/1.1 200"), "app: {index_headers}");
  assert!(
    String::from_utf8_lossy(&index_body).contains("Playwright"),
    "the UI-mode shell is served"
  );
  let (sw_headers, _) = http_get(&host, "/trace/sw.bundle.js").await;
  assert!(
    sw_headers.to_ascii_lowercase().contains("javascript"),
    "a service worker that does not arrive as JavaScript is refused: {sw_headers}"
  );

  let mut ui = Ui::connect(&url).await;
  ui.call("initialize", json!({ "watchTestDirs": true })).await;

  // Listing: each feature is a file suite, each scenario a test.
  let listed = ui.call("listTests", json!({})).await;
  let project = listed["report"]
    .as_array()
    .expect("report")
    .iter()
    .find(|event| event["method"] == "onProject")
    .expect("onProject")
    .clone();
  let suites = project["params"]["project"]["suites"].as_array().expect("suites");
  let scenarios: Vec<(String, String)> = suites
    .iter()
    .flat_map(|suite| suite["entries"].as_array().cloned().unwrap_or_default())
    .flat_map(|entry| {
      // A feature's scenarios sit under the feature suite, either
      // directly or under its `Feature:` name.
      entry["entries"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| vec![entry.clone()])
    })
    .filter_map(|entry| {
      Some((
        entry["title"].as_str()?.to_string(),
        entry["testId"].as_str()?.to_string(),
      ))
    })
    .collect();
  let (_, blank_id) = scenarios
    .iter()
    .find(|(title, _)| title.contains("blank page"))
    .unwrap_or_else(|| panic!("blank page scenario in {scenarios:?}"))
    .clone();

  // Run it.
  let run = ui
    .call("runTests", json!({ "testIds": [blank_id], "trace": "on" }))
    .await;
  assert_eq!(run["status"], "passed", "{run}");

  let ended = ui.wait_report("onTestEnd").await;
  assert_eq!(ended["params"]["result"]["status"], "passed");

  // The trace the viewer opens is an attachment on the result.
  let attached = ui.wait_report("onAttach").await;
  let trace = attached["params"]["attachments"]
    .as_array()
    .expect("attachments")
    .iter()
    .find(|attachment| attachment["name"] == "trace")
    .expect("trace attachment")
    .clone();
  assert_eq!(trace["contentType"], "application/zip");
  let trace_path = trace["path"].as_str().expect("trace path").to_string();

  // …and it is fetched through the viewer's own file route.
  let (headers, body) = http_get(&host, &format!("/trace/file?path={}", encode(&trace_path))).await;
  assert!(headers.starts_with("HTTP/1.1 200"), "trace fetch: {headers}");
  validate_trace(body);

  // A path outside the served roots is refused.
  let (forbidden, _) = http_get(&host, "/trace/file?path=%2Fetc%2Fpasswd").await;
  assert!(
    forbidden.starts_with("HTTP/1.1 403"),
    "the file route must stay inside the run's directories: {forbidden}"
  );

  stop_is_graceful(&mut ui, &scenarios).await;
}

/// Stop cancels cooperatively: the in-flight scenario finishes rather
/// than being detached, the run reports back, and the server takes
/// another run afterwards.
async fn stop_is_graceful(ui: &mut Ui, scenarios: &[(String, String)]) {
  let (_, slow_id) = scenarios
    .iter()
    .find(|(title, _)| title.contains("slow page"))
    .expect("slow scenario")
    .clone();
  let (_, blank_id) = scenarios
    .iter()
    .find(|(title, _)| title.contains("blank page"))
    .expect("blank scenario")
    .clone();

  let run_id = ui
    .send("runTests", json!({ "testIds": [slow_id], "trace": "on" }))
    .await;

  // Wait until it is actually executing, then stop.
  loop {
    let value = ui.next_message().await;
    if value["method"] == "report" && value["params"]["method"] == "onTestBegin" {
      break;
    }
  }
  ui.send("stopTests", json!({})).await;

  // The run answers, and the scenario it was running reported an end.
  let mut ended = false;
  loop {
    let value = ui.next_message().await;
    if value["method"] == "report" && value["params"]["method"] == "onTestEnd" {
      ended = true;
    }
    if value.get("id").and_then(Value::as_u64) == Some(run_id) {
      break;
    }
  }
  assert!(ended, "a stopped run still reports the test it was running");

  // The runner is reusable: another run completes normally.
  let again = ui
    .call("runTests", json!({ "testIds": [blank_id], "trace": "on" }))
    .await;
  assert_eq!(again["status"], "passed", "post-stop run: {again}");
}

/// The recorded trace, checked for what the viewer reads: a v8 stream,
/// the BDD step span, the protocol call nested inside it, DOM snapshots
/// around that call, and the sources behind both.
fn validate_trace(body: Vec<u8>) {
  let mut archive = zip::ZipArchive::new(std::io::Cursor::new(body)).expect("trace zip");
  let mut trace_text = String::new();
  archive
    .by_name("trace.trace")
    .expect("trace.trace entry")
    .read_to_string(&mut trace_text)
    .expect("read trace.trace");
  archive.by_name("trace.network").expect("trace.network entry");

  let lines: Vec<Value> = trace_text
    .lines()
    .map(|line| serde_json::from_str(line).expect("json trace line"))
    .collect();
  let first = &lines[0];
  assert_eq!(first["type"], "context-options", "first line: {first}");
  assert_eq!(first["version"], 8, "first line: {first}");

  let befores: Vec<&Value> = lines.iter().filter(|line| line["type"] == "before").collect();
  let step = befores
    .iter()
    .find(|action| action["title"] == "Given a blank ui page")
    .unwrap_or_else(|| panic!("step before event: {befores:?}"));
  let step_call_id = step["callId"].as_str().expect("step callId");
  assert_eq!(
    step["stepId"].as_str(),
    step["stepId"].as_str().filter(|id| !id.is_empty()),
    "v8 actions carry a stepId: {step}"
  );
  let step_after = lines
    .iter()
    .find(|line| line["type"] == "after" && line["callId"] == step_call_id)
    .expect("step after event");
  assert!(
    step_after["endTime"].as_f64().unwrap_or(0.0) >= step["startTime"].as_f64().unwrap_or(f64::MAX),
    "step span times ordered: {step} {step_after}"
  );

  let goto = befores
    .iter()
    .find(|action| action["method"] == "goto")
    .unwrap_or_else(|| panic!("protocol goto: {befores:?}"));
  assert_eq!(
    goto["parentId"].as_str(),
    Some(step_call_id),
    "a protocol call nests under the step that made it: {goto}"
  );

  let snapshots: Vec<&Value> = lines.iter().filter(|line| line["type"] == "frame-snapshot").collect();
  assert!(!snapshots.is_empty(), "DOM snapshots recorded");
  let goto_call_id = goto["callId"].as_str().expect("goto callId");
  let goto_after = lines
    .iter()
    .find(|line| line["type"] == "after" && line["callId"] == goto_call_id)
    .expect("goto after event");
  for (event, kind) in [(*goto, "beforeSnapshot"), (goto_after, "afterSnapshot")] {
    let name = event[kind].as_str().unwrap_or_else(|| panic!("goto {kind}: {event}"));
    assert!(
      snapshots
        .iter()
        .any(|snapshot| snapshot["snapshot"]["snapshotName"] == name),
      "{kind} {name} must resolve to a frame-snapshot"
    );
  }

  validate_source_stacks(&mut archive, step, goto);
}

/// Every action carries where it was written, and `sources: true` embeds
/// those files into the zip.
///
/// The two spans point at different files on purpose: the BDD step span
/// is located at its `.feature` line, while the protocol `goto` inside it
/// is located in the step body that issued the call. Both are what the
/// viewer's Source tab reads.
fn validate_source_stacks(archive: &mut zip::ZipArchive<std::io::Cursor<Vec<u8>>>, step: &Value, goto: &Value) {
  let goto_top = goto["stack"]
    .as_array()
    .and_then(|stack| stack.first())
    .unwrap_or_else(|| panic!("goto stack frame: {goto}"));
  assert!(
    goto_top["file"].as_str().is_some_and(|file| file.ends_with("steps.js")),
    "the goto's stack must name the step body that wrote it: {goto_top}"
  );

  let top = step["stack"]
    .as_array()
    .and_then(|stack| stack.first())
    .unwrap_or_else(|| panic!("step stack frame: {step}"));
  let file = top["file"].as_str().expect("stack frame file");
  assert!(file.ends_with("smoke.feature"), "stack file: {top}");
  assert_eq!(top["line"].as_u64(), Some(3), "the Given's feature line: {top}");

  // `resources/src@<sha1-of-path>.txt` — exactly the name the viewer
  // fetches (`sourceTab.tsx`).
  let sha1_hex = {
    use sha1::{Digest as _, Sha1};
    Sha1::digest(file.as_bytes())
      .iter()
      .fold(String::new(), |mut acc, byte| {
        use std::fmt::Write as _;
        let _ = write!(acc, "{byte:02x}");
        acc
      })
  };
  let mut source_text = String::new();
  archive
    .by_name(&format!("resources/src@{sha1_hex}.txt"))
    .expect("embedded feature source in trace zip")
    .read_to_string(&mut source_text)
    .expect("read embedded source");
  assert!(
    source_text.contains("Given a blank ui page"),
    "embedded source must be the feature file: {source_text}"
  );
}

fn encode(value: &str) -> String {
  use std::fmt::Write as _;

  let mut out = String::new();
  for byte in value.bytes() {
    match byte {
      b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(byte as char),
      _ => {
        let _ = write!(out, "%{byte:02X}");
      },
    }
  }
  out
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
    .position(|window| window == b"\r\n\r\n")
    .expect("header/body separator");
  let headers = String::from_utf8_lossy(&response[..split]).to_string();
  (headers, response[split + 4..].to_vec())
}
