//! Web UI server for `ferridriver bdd --ui` — a localhost app that lists
//! scenarios, streams live run events over a websocket, and serves run
//! artifacts (trace zips, screenshots) over HTTP.
//!
//! The server is transport only: it owns no run state machine. The run
//! loop ([`crate::runner::TestRunner::run_ui`]) publishes the test list
//! and per-run reporter events through [`UiState`], and receives
//! [`UiCommand`]s parsed from websocket clients. Events fan out through a
//! `tokio::sync::broadcast` channel so any number of browser tabs stay in
//! sync; a lagged tab is resynchronized from the state snapshot instead
//! of silently missing updates.

use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Path as UrlPath, State, WebSocketUpgrade};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use tokio::sync::{broadcast, mpsc};

use crate::model::{AttachmentBody, TestOutcome, TestPlan};
use crate::reporter::{ReporterEvent, Subscription};

const INDEX_HTML: &str = include_str!("ui_assets/index.html");

/// Vendored Playwright trace-viewer static app (playwright-core 1.61.1,
/// Apache-2.0 — LICENSE ships inside the archive). Embedded so the
/// trace viewer works fully offline; unpacked into memory on first use.
const TRACE_VIEWER_ZIP: &[u8] = include_bytes!("ui_assets/trace_viewer.zip");

static TRACE_VIEWER_ASSETS: std::sync::LazyLock<rustc_hash::FxHashMap<String, Vec<u8>>> =
  std::sync::LazyLock::new(|| {
    let mut assets = rustc_hash::FxHashMap::default();
    let Ok(mut archive) = zip::ZipArchive::new(std::io::Cursor::new(TRACE_VIEWER_ZIP)) else {
      return assets;
    };
    for index in 0..archive.len() {
      let Ok(mut entry) = archive.by_index(index) else {
        continue;
      };
      if entry.is_dir() {
        continue;
      }
      let name = entry.name().to_string();
      let mut bytes = Vec::new();
      if std::io::Read::read_to_end(&mut entry, &mut bytes).is_ok() {
        assets.insert(name, bytes);
      }
    }
    assets
  });

/// Command sent from a browser tab to the run loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiCommand {
  RunAll,
  RunFailed,
  RunGrep(String),
  RunTest(String),
  RunFile(String),
  Stop,
}

impl UiCommand {
  /// Parse a client websocket text frame: `{cmd:"runAll"}` |
  /// `{cmd:"runFailed"}` | `{cmd:"runGrep", pattern}` | `{cmd:"runTest", id}` |
  /// `{cmd:"runFile", file}` | `{cmd:"stop"}`.
  #[must_use]
  pub fn parse(text: &str) -> Option<Self> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    match value.get("cmd")?.as_str()? {
      "runAll" => Some(Self::RunAll),
      "runFailed" => Some(Self::RunFailed),
      "runGrep" => Some(Self::RunGrep(value.get("pattern")?.as_str()?.to_string())),
      "runTest" => Some(Self::RunTest(value.get("id")?.as_str()?.to_string())),
      "runFile" => Some(Self::RunFile(value.get("file")?.as_str()?.to_string())),
      "stop" => Some(Self::Stop),
      _ => None,
    }
  }
}

/// Last-known UI state, replayed to tabs that connect (or lag) mid-run.
#[derive(Default)]
struct UiSnapshot {
  /// Latest full `testList` message (statuses baked in at publish time).
  test_list: Option<serde_json::Value>,
  /// Latest status per test id, overlaid onto `test_list` on replay.
  statuses: rustc_hash::FxHashMap<String, String>,
  watch_status: String,
}

/// Shared server state: broadcast fan-out, command intake, artifact root,
/// and the replay snapshot.
pub struct UiState {
  events: broadcast::Sender<String>,
  commands: mpsc::UnboundedSender<UiCommand>,
  artifacts_root: PathBuf,
  snapshot: std::sync::RwLock<UiSnapshot>,
}

impl UiState {
  fn send(&self, message: &serde_json::Value) {
    let _ = self.events.send(message.to_string());
  }

  /// Publish a fresh test list built from a full plan. Existing statuses
  /// are preserved for tests that survive the rebuild.
  pub fn publish_test_list(&self, plan: &TestPlan) {
    let mut snapshot = self.snapshot.write().unwrap_or_else(std::sync::PoisonError::into_inner);
    let message = test_list_json(plan, &snapshot.statuses);
    snapshot.test_list = Some(message.clone());
    drop(snapshot);
    self.send(&message);
  }

  /// Publish and remember the watch status (`"running"` / `"idle"`).
  /// Returning to idle sweeps tests a cancelled run left in `"running"`
  /// back to `"idle"` so snapshots do not show phantom activity.
  pub fn set_watch_status(&self, status: &str) {
    let mut snapshot = self.snapshot.write().unwrap_or_else(std::sync::PoisonError::into_inner);
    snapshot.watch_status = status.to_string();
    if status == "idle" {
      for value in snapshot.statuses.values_mut() {
        if value == "running" {
          "idle".clone_into(value);
        }
      }
    }
    drop(snapshot);
    self.send(&serde_json::json!({ "type": "watchStatus", "status": status }));
  }

  /// Drain one run's reporter events into the broadcast channel, keeping
  /// the status snapshot current for late-joining tabs.
  pub async fn forward_run_events(self: Arc<Self>, mut subscription: Subscription) {
    while let Some(event) = subscription.rx.recv().await {
      match &event {
        ReporterEvent::TestStarted { test_id, .. } => {
          self
            .snapshot
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .statuses
            .insert(test_id.full_name(), "running".to_string());
        },
        ReporterEvent::TestFinished { test_id, outcome } => {
          self
            .snapshot
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .statuses
            .insert(test_id.full_name(), outcome.status.to_string());
        },
        _ => {},
      }
      self.send(&reporter_event_to_json(&event, &self.artifacts_root));
    }
  }

  /// Messages that bring a fresh (or lagged) tab up to date.
  fn snapshot_messages(&self) -> Vec<String> {
    let snapshot = self.snapshot.read().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut messages = Vec::with_capacity(2);
    if let Some(ref list) = snapshot.test_list {
      let mut list = list.clone();
      overlay_statuses(&mut list, &snapshot.statuses);
      messages.push(list.to_string());
    }
    let status = if snapshot.watch_status.is_empty() {
      "idle"
    } else {
      snapshot.watch_status.as_str()
    };
    messages.push(serde_json::json!({ "type": "watchStatus", "status": status }).to_string());
    messages
  }
}

/// A running UI server. `state` is shared with the run loop; `commands`
/// yields client commands in arrival order.
pub struct UiServer {
  pub addr: SocketAddr,
  pub state: Arc<UiState>,
  pub commands: mpsc::UnboundedReceiver<UiCommand>,
}

impl UiServer {
  /// Bind `127.0.0.1:<port>` (an ephemeral port when `None`) and serve
  /// the app in a background task.
  ///
  /// # Errors
  ///
  /// Errors if the listener cannot bind.
  pub async fn start(artifacts_root: PathBuf, port: Option<u16>) -> ferridriver::error::Result<Self> {
    use ferridriver::FerriError;

    let (events, _) = broadcast::channel(4096);
    let (commands_tx, commands_rx) = mpsc::unbounded_channel();
    let state = Arc::new(UiState {
      events,
      commands: commands_tx,
      artifacts_root,
      snapshot: std::sync::RwLock::new(UiSnapshot {
        watch_status: "idle".to_string(),
        ..UiSnapshot::default()
      }),
    });

    let addr = SocketAddr::from(([127, 0, 0, 1], port.unwrap_or(0)));
    let listener = tokio::net::TcpListener::bind(addr)
      .await
      .map_err(|e| FerriError::backend(format!("bind UI server {addr}: {e}")))?;
    let addr = listener
      .local_addr()
      .map_err(|e| FerriError::backend(format!("UI server local_addr: {e}")))?;

    let app = Router::new()
      .route("/", get(index))
      .route("/ws", get(ws_upgrade))
      .route("/artifact/{*path}", get(artifact))
      .route("/trace-viewer", get(trace_viewer_index))
      .route("/trace-viewer/", get(trace_viewer_index))
      .route("/trace-viewer/{*path}", get(trace_viewer_asset))
      .with_state(Arc::clone(&state));
    tokio::spawn(async move {
      let _ = axum::serve(listener, app).await;
    });

    Ok(Self {
      addr,
      state,
      commands: commands_rx,
    })
  }
}

async fn index() -> Html<&'static str> {
  Html(INDEX_HTML)
}

async fn trace_viewer_index() -> Response {
  serve_trace_viewer("index.html")
}

async fn trace_viewer_asset(UrlPath(path): UrlPath<String>) -> Response {
  serve_trace_viewer(&path)
}

/// Serve one embedded trace-viewer file. Correct Content-Type matters:
/// the viewer's service worker (`sw.bundle.js`) is rejected by the
/// browser unless it arrives as JavaScript.
fn serve_trace_viewer(path: &str) -> Response {
  let key = if path.is_empty() { "index.html" } else { path };
  match TRACE_VIEWER_ASSETS.get(key) {
    Some(bytes) => {
      let mime = mime_guess::from_path(key).first_or_octet_stream();
      Response::builder()
        .header(header::CONTENT_TYPE, mime.as_ref())
        .body(Body::from(bytes.clone()))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
    },
    None => StatusCode::NOT_FOUND.into_response(),
  }
}

async fn ws_upgrade(State(state): State<Arc<UiState>>, ws: WebSocketUpgrade) -> Response {
  ws.on_upgrade(move |socket| client_session(socket, state))
}

async fn client_session(mut socket: WebSocket, state: Arc<UiState>) {
  let mut events = state.events.subscribe();
  for message in state.snapshot_messages() {
    if socket.send(Message::Text(message.into())).await.is_err() {
      return;
    }
  }
  loop {
    tokio::select! {
      event = events.recv() => {
        match event {
          Ok(message) => {
            if socket.send(Message::Text(message.into())).await.is_err() {
              break;
            }
          },
          Err(broadcast::error::RecvError::Lagged(_)) => {
            // Best-effort fan-out dropped frames for this slow tab —
            // resend the snapshot so it reconverges.
            for message in state.snapshot_messages() {
              if socket.send(Message::Text(message.into())).await.is_err() {
                return;
              }
            }
          },
          Err(broadcast::error::RecvError::Closed) => break,
        }
      }
      incoming = socket.recv() => {
        match incoming {
          Some(Ok(Message::Text(text))) => {
            if let Some(command) = UiCommand::parse(&text) {
              let _ = state.commands.send(command);
            }
          },
          Some(Ok(Message::Close(_))) | None => break,
          Some(Ok(_)) => {},
          Some(Err(_)) => break,
        }
      }
    }
  }
}

/// Serve a file from the run's output directory. The path is confined to
/// the artifacts root (no traversal, no symlink escape) and responses
/// carry `Access-Control-Allow-Origin: *` so trace.playwright.dev can
/// fetch trace zips cross-origin.
async fn artifact(State(state): State<Arc<UiState>>, UrlPath(path): UrlPath<String>) -> Response {
  let Some(full_path) = resolve_artifact_path(&state.artifacts_root, &path) else {
    return StatusCode::NOT_FOUND.into_response();
  };
  match tokio::fs::read(&full_path).await {
    Ok(bytes) => {
      let mime = mime_guess::from_path(&full_path).first_or_octet_stream();
      Response::builder()
        .header(header::CONTENT_TYPE, mime.as_ref())
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .body(Body::from(bytes))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
    },
    Err(_) => StatusCode::NOT_FOUND.into_response(),
  }
}

/// Resolve a client-supplied relative path against the artifacts root.
/// Returns `None` for absolute paths, any `..`/root component, missing
/// files, or canonicalized paths that escape the root (symlinks).
fn resolve_artifact_path(root: &Path, relative: &str) -> Option<PathBuf> {
  let relative = Path::new(relative);
  if relative.is_absolute() {
    return None;
  }
  let mut clean = PathBuf::new();
  for component in relative.components() {
    match component {
      Component::Normal(part) => clean.push(part),
      Component::CurDir => {},
      Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
    }
  }
  if clean.as_os_str().is_empty() {
    return None;
  }
  let canonical_root = root.canonicalize().ok()?;
  let canonical = canonical_root.join(clean).canonicalize().ok()?;
  canonical.starts_with(&canonical_root).then_some(canonical)
}

/// Percent-encode a relative artifact path for use in a URL path,
/// keeping `/` separators intact.
fn encode_url_path(relative: &str) -> String {
  let mut encoded = String::with_capacity(relative.len());
  for byte in relative.bytes() {
    match byte {
      b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => encoded.push(byte as char),
      _ => {
        encoded.push('%');
        encoded.push_str(&format!("{byte:02X}"));
      },
    }
  }
  encoded
}

/// Build the `testList` message from a plan, overlaying known statuses.
fn test_list_json(plan: &TestPlan, statuses: &rustc_hash::FxHashMap<String, String>) -> serde_json::Value {
  let suites: Vec<serde_json::Value> = plan
    .suites
    .iter()
    .map(|suite| {
      let tests: Vec<serde_json::Value> = suite
        .tests
        .iter()
        .map(|test| {
          let id = test.id.full_name();
          let status = statuses.get(&id).map_or("idle", String::as_str);
          serde_json::json!({
            "id": id,
            "title": test.id.name,
            "file": test.id.file_location(),
            "status": status,
          })
        })
        .collect();
      serde_json::json!({
        "title": suite.name,
        "file": suite.file,
        "tests": tests,
      })
    })
    .collect();
  serde_json::json!({ "type": "testList", "suites": suites })
}

/// Overlay the latest statuses onto a cached `testList` message.
fn overlay_statuses(list: &mut serde_json::Value, statuses: &rustc_hash::FxHashMap<String, String>) {
  let Some(suites) = list.get_mut("suites").and_then(|s| s.as_array_mut()) else {
    return;
  };
  for suite in suites {
    let Some(tests) = suite.get_mut("tests").and_then(|t| t.as_array_mut()) else {
      continue;
    };
    for test in tests {
      let Some(id) = test.get("id").and_then(|i| i.as_str()) else {
        continue;
      };
      if let Some(status) = statuses.get(id) {
        test["status"] = serde_json::Value::String(status.clone());
      }
    }
  }
}

/// Map a [`ReporterEvent`] to its websocket JSON message. Explicit
/// mapping — the wire shape is API; internal reporter types stay
/// unserialized.
#[must_use]
pub fn reporter_event_to_json(event: &ReporterEvent, artifacts_root: &Path) -> serde_json::Value {
  match event {
    ReporterEvent::RunStarted {
      total_tests,
      num_workers,
      ..
    } => serde_json::json!({
      "type": "runStarted",
      "totalTests": total_tests,
      "workers": num_workers,
    }),
    ReporterEvent::WorkerStarted { worker_id } => serde_json::json!({
      "type": "workerStarted",
      "workerId": worker_id,
    }),
    ReporterEvent::TestStarted { test_id, attempt } => serde_json::json!({
      "type": "testStarted",
      "id": test_id.full_name(),
      "attempt": attempt,
    }),
    ReporterEvent::StepStarted(step) => serde_json::json!({
      "type": "stepStarted",
      "id": step.test_id.full_name(),
      "stepId": step.step_id,
      "parentStepId": step.parent_step_id,
      "title": step.title,
      "category": step.category.to_string(),
    }),
    ReporterEvent::StepFinished(step) => serde_json::json!({
      "type": "stepFinished",
      "id": step.test_id.full_name(),
      "stepId": step.step_id,
      "title": step.title,
      "category": step.category.to_string(),
      "durationMs": step.duration.as_millis() as u64,
      "error": step.error,
    }),
    ReporterEvent::TestFinished { test_id, outcome } => serde_json::json!({
      "type": "testFinished",
      "id": test_id.full_name(),
      "outcome": outcome_json(outcome, artifacts_root),
    }),
    ReporterEvent::WorkerFinished { worker_id } => serde_json::json!({
      "type": "workerFinished",
      "workerId": worker_id,
    }),
    ReporterEvent::RunFinished {
      total,
      passed,
      failed,
      skipped,
      flaky,
      duration,
    } => serde_json::json!({
      "type": "runFinished",
      "totals": {
        "total": total,
        "passed": passed,
        "failed": failed,
        "skipped": skipped,
        "flaky": flaky,
        "durationMs": duration.as_millis() as u64,
      },
    }),
  }
}

fn outcome_json(outcome: &TestOutcome, artifacts_root: &Path) -> serde_json::Value {
  let attachments: Vec<serde_json::Value> = outcome
    .attachments
    .iter()
    .map(|attachment| {
      let mut entry = serde_json::json!({
        "name": attachment.name,
        "contentType": attachment.content_type,
      });
      if let AttachmentBody::Path(ref path) = attachment.body {
        entry["path"] = serde_json::Value::String(path.display().to_string());
        if let Ok(relative) = path.strip_prefix(artifacts_root) {
          let encoded = encode_url_path(&relative.to_string_lossy());
          entry["urlPath"] = serde_json::Value::String(format!("/artifact/{encoded}"));
        }
      }
      entry
    })
    .collect();
  serde_json::json!({
    "status": outcome.status.to_string(),
    "durationMs": outcome.duration.as_millis() as u64,
    "attempt": outcome.attempt,
    "error": outcome.error.as_ref().map(ToString::to_string),
    "attachments": attachments,
    "stdout": outcome.stdout,
    "stderr": outcome.stderr,
  })
}

#[cfg(test)]
mod tests {
  use std::time::Duration;

  use super::*;
  use crate::model::{Attachment, TestId, TestStatus};

  fn test_id() -> TestId {
    TestId {
      file: "features/smoke.feature".into(),
      suite: Some("UI smoke".into()),
      name: "blank page".into(),
      line: Some(3),
    }
  }

  #[test]
  fn command_parse_accepts_all_four_commands() {
    assert_eq!(UiCommand::parse(r#"{"cmd":"runAll"}"#), Some(UiCommand::RunAll));
    assert_eq!(UiCommand::parse(r#"{"cmd":"runFailed"}"#), Some(UiCommand::RunFailed));
    assert_eq!(
      UiCommand::parse(r#"{"cmd":"runGrep","pattern":"smoke"}"#),
      Some(UiCommand::RunGrep("smoke".into()))
    );
    assert_eq!(
      UiCommand::parse(r#"{"cmd":"runTest","id":"a > b > c"}"#),
      Some(UiCommand::RunTest("a > b > c".into()))
    );
    assert_eq!(
      UiCommand::parse(r#"{"cmd":"runFile","file":"features/smoke.feature"}"#),
      Some(UiCommand::RunFile("features/smoke.feature".into()))
    );
    assert_eq!(UiCommand::parse(r#"{"cmd":"stop"}"#), Some(UiCommand::Stop));
    assert_eq!(UiCommand::parse(r#"{"cmd":"reboot"}"#), None);
    assert_eq!(UiCommand::parse("not json"), None);
    assert_eq!(UiCommand::parse(r#"{"cmd":"runGrep"}"#), None);
    assert_eq!(UiCommand::parse(r#"{"cmd":"runFile"}"#), None);
  }

  #[test]
  fn idle_transition_sweeps_running_statuses() {
    let (events, _) = tokio::sync::broadcast::channel(16);
    let (commands, _rx) = tokio::sync::mpsc::unbounded_channel();
    let state = UiState {
      events,
      commands,
      artifacts_root: PathBuf::from("/tmp"),
      snapshot: std::sync::RwLock::new(UiSnapshot::default()),
    };
    state.snapshot.write().expect("lock").statuses.extend([
      ("a".to_string(), "running".to_string()),
      ("b".to_string(), "passed".to_string()),
    ]);
    state.set_watch_status("idle");
    let snapshot = state.snapshot.read().expect("lock");
    assert_eq!(snapshot.statuses["a"], "idle");
    assert_eq!(snapshot.statuses["b"], "passed");
  }

  #[test]
  fn artifact_path_rejects_traversal_and_absolute_paths() {
    let root = tempfile::tempdir().expect("tempdir");
    let nested = root.path().join("suite").join("test");
    std::fs::create_dir_all(&nested).expect("mkdir");
    std::fs::write(nested.join("trace.zip"), b"zip").expect("write");
    std::fs::write(root.path().join("top.txt"), b"top").expect("write");

    let ok = resolve_artifact_path(root.path(), "suite/test/trace.zip").expect("valid path resolves");
    assert!(ok.ends_with("trace.zip"));
    assert!(resolve_artifact_path(root.path(), "top.txt").is_some());

    assert!(resolve_artifact_path(root.path(), "../top.txt").is_none());
    assert!(resolve_artifact_path(root.path(), "suite/../../top.txt").is_none());
    assert!(resolve_artifact_path(root.path(), "/etc/passwd").is_none());
    assert!(resolve_artifact_path(root.path(), "").is_none());
    assert!(resolve_artifact_path(root.path(), "suite/test/missing.zip").is_none());
  }

  #[test]
  fn artifact_path_rejects_symlink_escape() {
    let root = tempfile::tempdir().expect("tempdir");
    let outside = tempfile::tempdir().expect("tempdir");
    std::fs::write(outside.path().join("secret.txt"), b"secret").expect("write");
    std::os::unix::fs::symlink(outside.path().join("secret.txt"), root.path().join("link.txt")).expect("symlink");
    assert!(resolve_artifact_path(root.path(), "link.txt").is_none());
  }

  #[test]
  fn url_path_encoding_preserves_slashes_and_escapes_spaces() {
    assert_eq!(encode_url_path("a/b.zip"), "a/b.zip");
    assert_eq!(
      encode_url_path("smoke.feature > UI > blank/t-attempt1.trace.zip"),
      "smoke.feature%20%3E%20UI%20%3E%20blank/t-attempt1.trace.zip"
    );
  }

  #[test]
  fn test_finished_maps_outcome_with_artifact_url() {
    let root = tempfile::tempdir().expect("tempdir");
    let trace_path = root.path().join("key with space").join("t-attempt1.trace.zip");
    let outcome = TestOutcome {
      test_id: test_id(),
      status: TestStatus::Passed,
      duration: Duration::from_millis(1234),
      attempt: 1,
      max_attempts: 1,
      error: None,
      attachments: vec![Attachment {
        name: "trace".into(),
        content_type: "application/zip".into(),
        body: AttachmentBody::Path(trace_path),
      }],
      steps: Vec::new(),
      stdout: String::new(),
      stderr: String::new(),
      annotations: Vec::new(),
      metadata: serde_json::Value::Null,
    };
    let event = ReporterEvent::TestFinished {
      test_id: test_id(),
      outcome,
    };
    let json = reporter_event_to_json(&event, root.path());

    assert_eq!(json["type"].as_str(), Some("testFinished"));
    assert_eq!(
      json["id"].as_str(),
      Some("features/smoke.feature > UI smoke > blank page")
    );
    assert_eq!(json["outcome"]["status"].as_str(), Some("passed"));
    assert_eq!(json["outcome"]["durationMs"].as_u64(), Some(1234));
    assert!(json["outcome"]["error"].is_null());
    let attachment = &json["outcome"]["attachments"][0];
    assert_eq!(attachment["name"].as_str(), Some("trace"));
    assert_eq!(attachment["contentType"].as_str(), Some("application/zip"));
    assert_eq!(
      attachment["urlPath"].as_str(),
      Some("/artifact/key%20with%20space/t-attempt1.trace.zip")
    );
  }

  #[test]
  fn run_finished_maps_totals() {
    let event = ReporterEvent::RunFinished {
      total: 5,
      passed: 3,
      failed: 1,
      skipped: 1,
      flaky: 0,
      duration: Duration::from_secs(2),
    };
    let json = reporter_event_to_json(&event, Path::new("/tmp"));
    assert_eq!(json["type"].as_str(), Some("runFinished"));
    assert_eq!(json["totals"]["total"].as_u64(), Some(5));
    assert_eq!(json["totals"]["passed"].as_u64(), Some(3));
    assert_eq!(json["totals"]["failed"].as_u64(), Some(1));
    assert_eq!(json["totals"]["skipped"].as_u64(), Some(1));
    assert_eq!(json["totals"]["durationMs"].as_u64(), Some(2000));
  }

  #[test]
  fn step_events_map_ids_and_durations() {
    let started = ReporterEvent::StepStarted(Box::new(crate::reporter::StepStartedEvent {
      test_id: test_id(),
      step_id: "s1".into(),
      parent_step_id: None,
      title: "Given a blank page".into(),
      category: crate::model::StepCategory::TestStep,
    }));
    let json = reporter_event_to_json(&started, Path::new("/tmp"));
    assert_eq!(json["type"].as_str(), Some("stepStarted"));
    assert_eq!(json["stepId"].as_str(), Some("s1"));
    assert!(json["parentStepId"].is_null());
    assert_eq!(json["category"].as_str(), Some("test.step"));

    let finished = ReporterEvent::StepFinished(Box::new(crate::reporter::StepFinishedEvent {
      test_id: test_id(),
      step_id: "s1".into(),
      title: "Given a blank page".into(),
      category: crate::model::StepCategory::TestStep,
      duration: Duration::from_millis(88),
      error: Some("boom".into()),
      metadata: None,
    }));
    let json = reporter_event_to_json(&finished, Path::new("/tmp"));
    assert_eq!(json["type"].as_str(), Some("stepFinished"));
    assert_eq!(json["durationMs"].as_u64(), Some(88));
    assert_eq!(json["error"].as_str(), Some("boom"));
  }
}
