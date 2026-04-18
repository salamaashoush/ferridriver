//! MCP-over-stdio test client + response-extraction helpers.
//!
//! Spawns the ferridriver CLI as a child process per backend, speaks
//! JSON-RPC over its stdio, and exposes convenience wrappers for the
//! tools integration tests call the most (`navigate`, `run_script`).
//! Backends share the same client surface — the only per-backend
//! distinction is which CLI flag the child launches with.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static GLOBAL_ID: AtomicU64 = AtomicU64::new(1);

/// Stdio-connected MCP client. Owns the child process and tears it
/// down on drop.
pub struct McpClient {
  child: Child,
  reader: BufReader<std::process::ChildStdout>,
  stdin: std::process::ChildStdin,
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
    cmd.arg("--backend").arg(backend);
    if std::env::var("FERRIDRIVER_HEADED").is_err() {
      cmd.arg("--headless");
    }
    let mut child = cmd
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .stderr(Stdio::null())
      .spawn()
      .unwrap_or_else(|e| panic!("Failed to start: {binary}: {e}"));
    let stdout = child.stdout.take().unwrap();
    let stdin = child.stdin.take().unwrap();
    let mut c = McpClient {
      child,
      reader: BufReader::new(stdout),
      stdin,
      backend: backend.to_string(),
    };
    c.initialize();
    c.send_initialized_notification();
    c
  }

  fn send_raw(&mut self, msg: &Value) {
    writeln!(self.stdin, "{}", serde_json::to_string(msg).unwrap()).unwrap();
    self.stdin.flush().unwrap();
  }

  fn read_response(&mut self) -> Value {
    loop {
      let mut line = String::new();
      self.reader.read_line(&mut line).expect("read stdout");
      let trimmed = line.trim();
      if trimmed.is_empty() {
        continue;
      }
      // Skip non-JSON lines (e.g. tracing log output from rmcp).
      if let Ok(val) = serde_json::from_str::<Value>(trimmed) {
        return val;
      }
    }
  }

  pub fn send_request(&mut self, method: &str, params: Value) -> Value {
    let id = GLOBAL_ID.fetch_add(1, Ordering::SeqCst);
    self.send_raw(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
    loop {
      let resp = self.read_response();
      if resp.get("id").and_then(|v| v.as_u64()) == Some(id) {
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
}

impl Drop for McpClient {
  fn drop(&mut self) {
    let _ = self.child.kill();
    let _ = self.child.wait();
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
