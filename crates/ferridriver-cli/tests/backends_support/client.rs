//! MCP-over-stdio test client + response-extraction helpers.
//!
//! Spawns the ferridriver CLI as a child process per backend, speaks
//! JSON-RPC over its stdio, and exposes convenience wrappers for the
//! tools integration tests call the most (`navigate`, `run_script`).
//! Backends share the same client surface — the only per-backend
//! distinction is which CLI flag the child launches with.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

static GLOBAL_ID: AtomicU64 = AtomicU64::new(1);

/// Hard timeout per JSON-RPC request — if no matching reply arrives
/// within this window the client panics so the test surfaces the
/// hang with method+id instead of stalling libtest indefinitely.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Stdio-connected MCP client. Owns the child process and tears it
/// down on drop.
pub struct McpClient {
  child: Child,
  /// Background-thread receiver: every stdout line the child emits is
  /// forwarded here. `recv_timeout` lets the main test thread bound
  /// the wait so a hung MCP doesn't deadlock libtest.
  rx: mpsc::Receiver<String>,
  /// `Option` so `Drop` can close the write end of the pipe *before* killing
  /// the child — closing stdin lets the CLI's MCP transport return from
  /// `svc.waiting()` and drop its `BrowserState` cleanly (which in turn kills
  /// any spawned Chrome via `kill_on_drop`). Without the closure, we'd jump
  /// straight to SIGKILL and Chrome would leak.
  stdin: Option<std::process::ChildStdin>,
  #[allow(dead_code)]
  pub backend: String,
}

impl McpClient {
  pub fn new(backend: &str) -> Self {
    let binary = std::env::var("FERRIDRIVER_BIN").unwrap_or_else(|_| {
      let base = format!("{}/../../target", env!("CARGO_MANIFEST_DIR"));
      let debug = format!("{base}/debug/ferridriver");
      let release = format!("{base}/release/ferridriver");
      if std::path::Path::new(&debug).exists() {
        debug
      } else {
        release
      }
    });
    let mut cmd = Command::new(&binary);
    cmd.arg("mcp").arg("--backend").arg(backend);
    if std::env::var("FERRIDRIVER_HEADED").is_err() {
      cmd.arg("--headless");
    }
    let stderr_target = match std::env::var("FERRIDRIVER_MCP_STDERR_LOG") {
      Ok(path) => {
        let f = std::fs::OpenOptions::new()
          .create(true)
          .append(true)
          .open(&path)
          .unwrap_or_else(|e| panic!("open MCP stderr log {path}: {e}"));
        Stdio::from(f)
      },
      Err(_) => Stdio::null(),
    };
    let mut child = cmd
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .stderr(stderr_target)
      .env(
        "RUST_LOG",
        std::env::var("RUST_LOG").unwrap_or_else(|_| "ferridriver=debug,ferridriver_mcp=debug".into()),
      )
      // Put the MCP child (and every Chrome / Firefox / WebKit host
      // it spawns) into its own process group, with the child as
      // group leader. `Drop` then `kill(-pgid, …)`s the whole tree
      // in one shot. Without this, when the test harness escalates
      // to SIGKILL on a wedged CLI, Chrome processes get re-parented
      // to launchd and leak (their `kill_on_drop` Drop handlers
      // never run because SIGKILL skips destructors). Leaked Chromes
      // hold random `--remote-debugging-port=0` allocations, which
      // make port-collision-sensitive tests
      // (`navigation_response::test_goto_network_failure`) flake.
      .process_group(0)
      .spawn()
      .unwrap_or_else(|e| panic!("Failed to start: {binary}: {e}"));
    let stdout = child.stdout.take().unwrap();
    let stdin = child.stdin.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let backend_for_thread = backend.to_string();
    std::thread::Builder::new()
      .name(format!("mcp-stdout-reader-{backend}"))
      .spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
          let mut line = String::new();
          match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
              if tx.send(line).is_err() {
                break;
              }
            },
            Err(e) => {
              let _ = tx.send(format!("__READ_ERR__ backend={backend_for_thread}: {e}"));
              break;
            },
          }
        }
      })
      .expect("spawn mcp stdout reader thread");
    let mut c = McpClient {
      child,
      rx,
      stdin: Some(stdin),
      backend: backend.to_string(),
    };
    c.initialize();
    c.send_initialized_notification();
    c
  }

  fn send_raw(&mut self, msg: &Value) {
    let stdin = self.stdin.as_mut().expect("stdin already closed");
    writeln!(stdin, "{}", serde_json::to_string(msg).unwrap()).unwrap();
    stdin.flush().unwrap();
  }

  fn read_response_with_deadline(&mut self, ctx: &str, deadline: std::time::Instant) -> Value {
    loop {
      let remaining = deadline.saturating_duration_since(std::time::Instant::now());
      assert!(
        !remaining.is_zero(),
        "MCP request timed out after {REQUEST_TIMEOUT:?} (backend={}, {ctx})",
        self.backend
      );
      let line = match self.rx.recv_timeout(remaining) {
        Ok(l) => l,
        Err(mpsc::RecvTimeoutError::Timeout) => {
          panic!(
            "MCP request timed out after {REQUEST_TIMEOUT:?} (backend={}, {ctx})",
            self.backend
          );
        },
        Err(mpsc::RecvTimeoutError::Disconnected) => {
          panic!(
            "ferridriver MCP child closed stdout before responding (backend={}, {ctx}). \
             Check $FERRIDRIVER_MCP_STDERR_LOG for child stderr.",
            self.backend
          );
        },
      };
      assert!(
        !line.starts_with("__READ_ERR__"),
        "MCP stdout read error (backend={}, {ctx}): {}",
        self.backend,
        line.trim()
      );
      let trimmed = line.trim();
      if trimmed.is_empty() {
        continue;
      }
      if let Ok(val) = serde_json::from_str::<Value>(trimmed) {
        return val;
      }
      // non-JSON line (tracing log etc.) — keep reading
    }
  }

  pub fn send_request(&mut self, method: &str, params: Value) -> Value {
    let id = GLOBAL_ID.fetch_add(1, Ordering::SeqCst);
    let tool = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let start = std::time::Instant::now();
    let deadline = start + REQUEST_TIMEOUT;
    let trace = std::env::var("FERRIDRIVER_TEST_VERBOSE").is_ok();
    if trace {
      eprintln!(">>> [{}] id={id} method={method} tool={tool}", self.backend);
    }
    self.send_raw(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
    loop {
      let ctx = format!("id={id} method={method} tool={tool}");
      let resp = self.read_response_with_deadline(&ctx, deadline);
      if resp.get("id").and_then(|v| v.as_u64()) == Some(id) {
        if trace {
          eprintln!(
            "<<< [{}] id={id} method={method} tool={tool} ms={}",
            self.backend,
            start.elapsed().as_millis()
          );
        }
        return resp;
      }
    }
  }

  fn initialize(&mut self) -> Value {
    self.send_request(
      "initialize",
      json!({
          "protocolVersion":"2024-11-05","capabilities":{},
          "clientInfo":{"name":"test","version":"1.0.0"}
      }),
    )
  }

  fn send_initialized_notification(&mut self) {
    self.send_raw(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
  }

  pub fn call_tool(&mut self, name: &str, args: Value) -> Value {
    self.send_request("tools/call", json!({"name":name,"arguments":args}))
  }

  pub fn tool_text(&mut self, name: &str, args: Value) -> String {
    extract_text(&self.call_tool(name, args))
  }

  pub fn nav(&mut self, html: &str) {
    self.call_tool("navigate", json!({"url": data_url(html)}));
  }

  pub fn nav_url(&mut self, url: &str) {
    self.call_tool("navigate", json!({"url": url}));
  }

  /// Run a script with empty args and return the parsed `{status, value, ...}` payload.
  pub fn script(&mut self, source: &str) -> Value {
    self.script_with_args(source, json!([]))
  }

  /// Run a script with bound args and return the parsed payload.
  pub fn script_with_args(&mut self, source: &str, args: Value) -> Value {
    let resp = self.call_tool("run_script", json!({"source": source, "args": args}));
    ok(&resp, "run_script");
    extract_script_payload(&resp).expect("script response should carry a JSON payload")
  }

  /// Run a script expecting success; return the `value` from the payload.
  pub fn script_value(&mut self, source: &str) -> Value {
    let payload = self.script(source);
    assert_eq!(payload["status"].as_str(), Some("ok"), "script failed: {payload}");
    payload["value"].clone()
  }

  /// Run a script with args, expecting success; return the `value`.
  pub fn script_value_with_args(&mut self, source: &str, args: Value) -> Value {
    let payload = self.script_with_args(source, args);
    assert_eq!(payload["status"].as_str(), Some("ok"), "script failed: {payload}");
    payload["value"].clone()
  }

  /// Run a script with a wall-clock timeout (ms) and return the parsed
  /// payload. Used to drive the poisoning-timeout recovery path.
  pub fn script_with_timeout(&mut self, source: &str, timeout_ms: u64) -> Value {
    let resp = self.call_tool(
      "run_script",
      json!({"source": source, "args": [], "timeout_ms": timeout_ms}),
    );
    ok(&resp, "run_script");
    extract_script_payload(&resp).expect("script response should carry a JSON payload")
  }
}

impl Drop for McpClient {
  fn drop(&mut self) {
    // Close stdin first so the CLI's MCP stdio transport returns from
    // `svc.waiting()` and its tokio runtime shuts down gracefully —
    // that's what drops `BrowserState` and (via `kill_on_drop(true)`
    // on each backend's `Child`) kills Chrome / Firefox / WebKit-host
    // processes the *clean* way (Chrome flushes cookies on close,
    // etc.).
    drop(self.stdin.take());

    // Group identity for the negative-PID `kill` calls below — pinned
    // before any `wait`, since `Child::id` returns 0 once the process
    // is reaped.
    #[allow(clippy::cast_possible_wrap)]
    let pgid_arg = -(self.child.id() as i32);

    // Poll briefly for the graceful exit. If the CLI shut down on
    // its own and the cascade of `Drop` impls reached every browser
    // child, the trailing group-kill below is a no-op (sends signals
    // to an empty group → ESRCH, ignored).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut graceful = false;
    loop {
      match self.child.try_wait() {
        Ok(Some(_)) => {
          graceful = true;
          break;
        },
        Ok(None) if std::time::Instant::now() < deadline => {
          std::thread::sleep(std::time::Duration::from_millis(25));
        },
        _ => break,
      }
    }

    // If the graceful path didn't take, group-kill via SIGTERM with
    // a short grace window before escalating. Targets the whole
    // process group (set up via `.process_group(0)` at spawn) so
    // every browser child gets the signal, not just the MCP server.
    if !graceful {
      // SAFETY: `libc::kill` is sync, takes plain integer args.
      // The negative pid targets a process group that exists by
      // construction (`process_group(0)` at spawn).
      #[allow(unsafe_code)]
      unsafe {
        libc::kill(pgid_arg, libc::SIGTERM);
      }
      let term_deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
      while std::time::Instant::now() < term_deadline {
        match self.child.try_wait() {
          Ok(None) => std::thread::sleep(std::time::Duration::from_millis(25)),
          _ => break,
        }
      }
    }

    // ALWAYS group-kill at the end, even after a graceful MCP exit.
    // The MCP runtime sometimes shuts down before its `Drop` cascade
    // reaches every backend `Child`'s `kill_on_drop` handler (tokio
    // runtime shutdown can race the destructor chain). SIGKILL on
    // the group catches those orphans without affecting the MCP
    // server (already exited) or the test harness itself (which is
    // in a different group). Idempotent.
    // SAFETY: same as above.
    #[allow(unsafe_code)]
    unsafe {
      libc::kill(pgid_arg, libc::SIGKILL);
    }
    let _ = self.child.wait();

    // Backstop: ferridriver's CDP/BiDi backends spawn the browser
    // parent in its own session via `setsid` (so `killpg` cleanly
    // takes down every renderer/utility/GPU child). Chrome itself
    // also detaches its own helper subprocesses into independent
    // sessions in some cases — those survive the parent's group
    // kill and re-parent to launchd. They're identifiable by their
    // `--user-data-dir=…/ferridriver-<flavour>-…` arg (the prefix
    // every CDP launch path stamps) or `--profile …/.tmp…` pattern
    // for the BiDi Firefox launch. `pkill -f` matches the command
    // line and kills only those — no impact on unrelated Chromes
    // the user might have running.
    let _ = std::process::Command::new("pkill")
      .args(["-9", "-f", "ferridriver-pipe-|ferridriver-raw-|ferridriver-firefox-"])
      .stderr(Stdio::null())
      .stdout(Stdio::null())
      .status();
  }
}

// ─── Tool response helpers ──────────────────────────────────────────────────

pub fn data_url(html: &str) -> String {
  format!("data:text/html,{}", urlenc(html))
}

pub fn urlenc(s: &str) -> String {
  let mut out = String::with_capacity(s.len() * 3);
  for b in s.bytes() {
    match b {
      b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'!' | b'\'' | b'(' | b')' | b'*' => {
        out.push(b as char)
      },
      _ => out.push_str(&format!("%{:02X}", b)),
    }
  }
  out
}

pub fn extract_text(resp: &Value) -> String {
  resp["result"]["content"]
    .as_array()
    .and_then(|a| a.first())
    .and_then(|c| c["text"].as_str())
    .unwrap_or("")
    .to_string()
}

pub fn extract_image_b64(resp: &Value) -> String {
  resp["result"]["content"]
    .as_array()
    .and_then(|a| a.iter().find(|c| c["type"].as_str() == Some("image")))
    .and_then(|c| c["data"].as_str())
    .unwrap_or("")
    .to_string()
}

/// Find the content block that parses as the script engine's structured
/// payload (`{ status, value | error, duration_ms, console[] }`). The tool
/// returns one or two text blocks depending on outcome; we scan until we
/// find the JSON one.
pub fn extract_script_payload(resp: &Value) -> Option<Value> {
  let contents = resp["result"]["content"].as_array()?;
  for c in contents {
    if let Some(text) = c["text"].as_str() {
      if let Ok(parsed) = serde_json::from_str::<Value>(text) {
        if parsed.get("status").is_some() {
          return Some(parsed);
        }
      }
    }
  }
  None
}

pub fn is_error(resp: &Value) -> bool {
  resp.get("error").is_some() || resp["result"]["isError"].as_bool().unwrap_or(false)
}

pub fn ok(resp: &Value, ctx: &str) {
  assert!(!is_error(resp), "{ctx} failed: {resp}");
}
