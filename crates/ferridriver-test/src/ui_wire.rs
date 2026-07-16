//! Harness-side wire client for `ferridriver test --ui`.
//!
//! The CLI hosts the web UI server ([`crate::ui_server`]) in its own
//! process and spawns cargo test cycles as children. It exports
//! [`UI_SOCK_ENV`]; every `#[ferritest]` harness binary in the cycle
//! connects back over that unix socket and streams newline-delimited
//! JSON: one `testList` hello carrying the discovered (pre-filter) plan,
//! then each reporter event mapped through
//! [`crate::ui_server::reporter_event_to_json`] — the exact wire shape
//! browser tabs already consume, so the CLI forwards lines verbatim.
//!
//! Commands flow the other way out-of-band: the CLI encodes them as
//! `FERRITEST_*` environment variables on the next spawned cycle and
//! kills the child's process group for Stop, so the socket stays
//! one-way.
//!
//! Live traces cannot be exported by the CLI (the recorder spool lives
//! in this process), so each harness serves its own `/live-trace`
//! endpoint ([`crate::ui_server::start_live_trace_server`]) and rewrites
//! the `testStarted.liveTraceUrl` to the absolute URL.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::config::{CliOverrides, TestConfig};
use crate::model::TestPlan;
use crate::reporter::{Reporter, ReporterEvent};

/// Unix-socket path the CLI's UI bridge listens on.
pub const UI_SOCK_ENV: &str = "FERRITEST_UI_SOCK";
/// Absolute artifacts root the CLI serves under `/artifact/`. Harness
/// output is redirected there so attachment `urlPath`s resolve across
/// processes (a harness's default cwd-relative `test-results` would
/// resolve against the package dir, not the dir the CLI serves).
pub const UI_ARTIFACTS_ENV: &str = "FERRITEST_UI_ARTIFACTS";
/// List-only cycle: send the `testList` hello and exit without running.
pub const LIST_ENV: &str = "FERRITEST_LIST";

/// Line-oriented JSON writer over the UI unix socket. Writes are
/// synchronous — the socket is local and lines are small, so a blocking
/// `write_all` under the mutex is cheaper and more deterministic at
/// process exit than an async writer task that must be drained. A write
/// timeout bounds the damage of a wedged CLI (stopped in a debugger,
/// SIGSTOP): the send fails after two seconds instead of blocking a
/// runtime worker for the rest of the run.
pub struct UiSock {
  stream: Mutex<std::os::unix::net::UnixStream>,
}

impl UiSock {
  /// Connect to the CLI's listener at `path`.
  ///
  /// # Errors
  ///
  /// Errors if the socket cannot be connected.
  pub fn connect(path: &Path) -> std::io::Result<Self> {
    let stream = std::os::unix::net::UnixStream::connect(path)?;
    let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(2)));
    Ok(Self {
      stream: Mutex::new(stream),
    })
  }

  /// Send one message as an NDJSON line. Failures are ignored: a dead
  /// CLI (user hit ctrl-C mid-cycle) must not fail the test run itself.
  pub fn send(&self, message: &serde_json::Value) {
    let mut line = message.to_string();
    line.push('\n');
    let mut stream = self.stream.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let _ = stream.write_all(line.as_bytes());
  }
}

/// Reporter that mirrors every event onto the UI socket in the app's
/// wire shape.
pub struct UiSockReporter {
  sock: Arc<UiSock>,
  artifacts_root: PathBuf,
  /// `http://127.0.0.1:<port>` of this process's live-trace server;
  /// prefixed onto the relative `liveTraceUrl` of `testStarted`.
  live_trace_base: Option<String>,
}

#[async_trait::async_trait]
impl Reporter for UiSockReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    // Step events never reach browser tabs (the embedded trace viewer
    // is the step list) — skip the serialization and socket traffic.
    if matches!(event, ReporterEvent::StepStarted(_) | ReporterEvent::StepFinished(_)) {
      return;
    }
    let mut message = crate::ui_server::reporter_event_to_json(event, &self.artifacts_root);
    if let (Some(base), Some(relative)) = (
      self.live_trace_base.as_deref(),
      message.get("liveTraceUrl").and_then(|v| v.as_str()),
    ) {
      message["liveTraceUrl"] = serde_json::Value::String(format!("{base}{relative}"));
    }
    self.sock.send(&message);
  }
}

/// The `testList` hello: the full discovered plan plus the binary key
/// the CLI aggregates sidebars under (several test binaries can
/// participate in one cycle).
fn hello_message(plan: &TestPlan) -> serde_json::Value {
  let mut message = crate::ui_server::test_list_json(plan, &rustc_hash::FxHashMap::default());
  message["binary"] = serde_json::Value::String(binary_key());
  message
}

/// Stable identity for this test binary across rebuilds. Cargo names
/// test executables `<target>-<16-hex-hash>` and the hash changes with
/// every code edit; keeping it would leave stale sidebar entries behind
/// after each watch-triggered rebuild.
fn binary_key() -> String {
  let stem = std::env::current_exe()
    .ok()
    .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
    .unwrap_or_else(|| "harness".to_string());
  match stem.rsplit_once('-') {
    Some((base, hash)) if hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit()) && !base.is_empty() => {
      base.to_string()
    },
    _ => stem,
  }
}

/// Harness entry for a UI cycle: hello with the full plan, then either
/// exit (list cycle) or run with the socket reporter attached. Traces
/// are forced on, mirroring `TestRunner::run_ui`, so every finished
/// test carries a trace attachment for the embedded viewer.
pub async fn run_harness_ui(sock_path: PathBuf, mut config: TestConfig, overrides: CliOverrides) -> i32 {
  if let Some(dir) = std::env::var_os(UI_ARTIFACTS_ENV).filter(|v| !v.is_empty()) {
    config.output_dir = PathBuf::from(dir);
  }
  if config.trace == crate::config::TraceMode::Off {
    config.trace = crate::config::TraceMode::On;
  }
  let plan = crate::discovery::collect_rust_tests(&config);

  let sock = match UiSock::connect(&sock_path) {
    Ok(sock) => Arc::new(sock),
    Err(e) => {
      eprintln!(
        "ferritest: UI server socket {} unreachable ({e}); running without UI streaming",
        sock_path.display()
      );
      let mut runner = crate::runner::TestRunner::new(config, overrides);
      return Box::pin(runner.run(plan)).await;
    },
  };
  sock.send(&hello_message(&plan));
  if overrides.list_only {
    return 0;
  }

  let live_trace_base = match crate::ui_server::start_live_trace_server().await {
    Ok(addr) => Some(format!("http://{addr}")),
    Err(e) => {
      tracing::warn!(target: "ferridriver::ui", "live-trace server failed to start: {e}");
      None
    },
  };
  let artifacts_root = config.output_dir.clone();
  let mut runner = crate::runner::TestRunner::new(config, overrides);
  runner.add_reporter(Box::new(UiSockReporter {
    sock,
    artifacts_root,
    live_trace_base,
  }));
  Box::pin(runner.run(plan)).await
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn hello_carries_binary_and_suites() {
    let plan = TestPlan {
      suites: Vec::new(),
      total_tests: 0,
      shard: None,
    };
    let message = hello_message(&plan);
    assert_eq!(message["type"].as_str(), Some("testList"));
    assert!(message["binary"].as_str().is_some_and(|b| !b.is_empty()));
    assert!(message["suites"].is_array());
  }

  #[test]
  fn sock_streams_ndjson_lines() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ui.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
    let sock = UiSock::connect(&path).expect("connect");
    sock.send(&serde_json::json!({ "type": "testStarted", "id": "a" }));
    sock.send(&serde_json::json!({ "type": "testFinished", "id": "a" }));
    drop(sock);

    let (stream, _) = listener.accept().expect("accept");
    let reader = std::io::BufReader::new(stream);
    let lines: Vec<serde_json::Value> = std::io::BufRead::lines(reader)
      .map(|l| serde_json::from_str(&l.expect("line")).expect("json"))
      .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["type"].as_str(), Some("testStarted"));
    assert_eq!(lines[1]["id"].as_str(), Some("a"));
  }
}
