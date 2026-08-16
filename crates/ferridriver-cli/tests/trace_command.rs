#![allow(clippy::expect_used, clippy::unwrap_used)]
//! `ferridriver trace` end to end: a synthetic trace on disk, read back
//! through the built binary the way a person or an agent would.
//!
//! No browser needed — the whole point of `trace show` / `trace ls` is that
//! they work where one is not available. `trace view` is exercised through
//! `--no-open`, which serves the embedded viewer and prints its URL instead
//! of opening a window.
//!
//! Requires a built `ferridriver` binary (`FERRIDRIVER_BIN` or
//! `target/{debug,release}/ferridriver`).

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};

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

const TRACE: &str = concat!(
  r#"{"version":8,"type":"context-options","origin":"library","browserName":"chromium","platform":"darwin","wallTime":1000,"monotonicTime":0,"title":"checkout > pays","options":{},"sdkLanguage":"javascript"}"#,
  "\n",
  r#"{"type":"before","callId":"call@1","startTime":0,"class":"Page","method":"goto","title":"page.goto","params":{"url":"http://app.local/"},"pageId":"page@1","stack":[{"file":"/spec.ts","line":7,"column":3}]}"#,
  "\n",
  r#"{"type":"after","callId":"call@1","endTime":412}"#,
  "\n",
  r##"{"type":"before","callId":"call@2","startTime":500,"class":"Locator","method":"click","title":"locator.click","params":{"selector":"#submit"},"pageId":"page@1","parentId":"call@1"}"##,
  "\n",
  r##"{"type":"log","callId":"call@2","time":600,"message":"waiting for locator('#submit')"}"##,
  "\n",
  r#"{"type":"after","callId":"call@2","endTime":1700,"error":{"name":"TimeoutError","message":"Timeout 1000ms exceeded"}}"#,
  "\n",
  r#"{"type":"console","time":50,"messageType":"error","text":"kaboom","pageId":"page@1","location":{"url":"http://app.local/","lineNumber":3,"columnNumber":1}}"#,
  "\n",
);

const NETWORK: &str = concat!(
  r#"{"type":"resource-snapshot","snapshot":{"time":12,"request":{"method":"GET","url":"http://app.local/app.js"},"response":{"status":200,"content":{"mimeType":"text/javascript"}}}}"#,
  "\n",
  r#"{"type":"resource-snapshot","snapshot":{"time":3,"request":{"method":"POST","url":"http://app.local/api/pay"},"response":{"status":500,"content":{"mimeType":"application/json"}}}}"#,
  "\n",
);

/// A run's output directory: one finished `trace.zip` under a test folder.
fn write_workspace() -> tempfile::TempDir {
  let dir = tempfile::tempdir().expect("tempdir");
  std::fs::write(
    dir.path().join("ferridriver.toml"),
    "[test]\noutputDir = \"test-results\"\n",
  )
  .expect("write config");

  let results = dir.path().join("test-results").join("checkout-pays");
  std::fs::create_dir_all(&results).expect("mkdir");
  let zip_path = results.join("pays-attempt1.trace.zip");
  let file = std::fs::File::create(&zip_path).expect("create zip");
  let mut zip = zip::ZipWriter::new(file);
  let options = zip::write::SimpleFileOptions::default();
  zip.start_file("trace.trace", options).expect("entry");
  zip.write_all(TRACE.as_bytes()).expect("write");
  zip.start_file("trace.network", options).expect("entry");
  zip.write_all(NETWORK.as_bytes()).expect("write");
  zip.finish().expect("finish");
  dir
}

fn run(dir: &std::path::Path, args: &[&str]) -> (bool, String) {
  let output = Command::new(bin())
    .args(args)
    .current_dir(dir)
    .output()
    .expect("run ferridriver");
  let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
  text.push_str(&String::from_utf8_lossy(&output.stderr));
  (output.status.success(), text)
}

#[test]
fn show_renders_the_call_tree_of_the_newest_trace() {
  let workspace = write_workspace();
  let (ok, out) = run(workspace.path(), &["trace", "show"]);
  assert!(ok, "trace show failed:\n{out}");

  assert!(out.contains("checkout > pays"), "{out}");
  assert!(out.contains("page.goto http://app.local/"), "{out}");
  assert!(out.contains("412ms"), "{out}");
  assert!(out.contains("locator.click #submit"), "{out}");
  assert!(out.contains("TimeoutError"), "{out}");
  assert!(out.contains("waiting for locator('#submit')"), "{out}");
  assert!(out.contains("at /spec.ts:7"), "{out}");
  assert!(out.contains("2 requests, 1 failed"), "{out}");
  assert!(out.contains("1 message, 1 error(s)"), "{out}");
}

#[test]
fn show_errors_keeps_only_the_failure() {
  let workspace = write_workspace();
  let (ok, out) = run(workspace.path(), &["trace", "show", "--errors"]);
  assert!(ok, "{out}");
  assert!(!out.contains("page.goto"), "passing call survived --errors:\n{out}");
  assert!(out.contains("locator.click"), "{out}");
  assert!(!out.contains("app.js"), "healthy request survived --errors:\n{out}");
  assert!(out.contains("/api/pay"), "{out}");
}

#[test]
fn show_hides_the_sections_it_is_told_to() {
  let workspace = write_workspace();
  let (ok, out) = run(
    workspace.path(),
    &[
      "trace", "show", "--hide", "console", "--hide", "network", "--hide", "logs",
    ],
  );
  assert!(ok, "{out}");
  assert!(out.contains("locator.click"), "{out}");
  assert!(!out.contains("kaboom"), "{out}");
  assert!(!out.contains("app.js"), "{out}");
  assert!(!out.contains("waiting for locator"), "{out}");
}

#[test]
fn show_json_is_machine_readable() {
  let workspace = write_workspace();
  let (ok, out) = run(workspace.path(), &["trace", "show", "--json"]);
  assert!(ok, "{out}");
  let json: serde_json::Value = serde_json::from_str(&out).expect("valid json");
  let context = &json["contexts"][0];
  assert_eq!(context["browserName"], "chromium");
  assert_eq!(context["actions"][1]["error"]["name"], "TimeoutError");
  assert_eq!(context["actions"][1]["durationMs"], 1200.0);
  assert_eq!(context["network"][1]["status"], 500);
}

#[test]
fn ls_lists_the_run_and_flags_the_failure() {
  let workspace = write_workspace();
  let (ok, out) = run(workspace.path(), &["trace", "ls"]);
  assert!(ok, "{out}");
  assert!(out.contains("pays-attempt1.trace.zip"), "{out}");
  assert!(out.contains("1 failed"), "{out}");

  let (ok, json_out) = run(workspace.path(), &["trace", "ls", "--json"]);
  assert!(ok, "{json_out}");
  let entries: serde_json::Value = serde_json::from_str(&json_out).expect("valid json");
  assert_eq!(entries.as_array().map(Vec::len), Some(1));
  assert!(entries[0]["summary"].as_str().is_some_and(|s| s.contains("chromium")));
}

#[test]
fn missing_trace_says_where_it_looked() {
  let dir = tempfile::tempdir().expect("tempdir");
  std::fs::write(
    dir.path().join("ferridriver.toml"),
    "[test]\noutputDir = \"test-results\"\n",
  )
  .expect("config");
  let (ok, out) = run(dir.path(), &["trace", "show"]);
  assert!(!ok, "expected failure:\n{out}");
  assert!(out.contains("no traces under"), "{out}");
}

#[test]
fn view_serves_the_embedded_viewer_and_the_trace_behind_it() {
  let workspace = write_workspace();
  let mut child = Command::new(bin())
    .args(["trace", "view", "--no-open", "--port", "0"])
    .current_dir(workspace.path())
    .stdout(Stdio::piped())
    .stderr(Stdio::inherit())
    .spawn()
    .expect("spawn trace view");
  let stdout = child.stdout.take().expect("stdout");
  let _guard = KillOnDrop(child);

  let mut url = String::new();
  for line in BufReader::new(stdout).lines().map_while(Result::ok) {
    if let Some(rest) = line.strip_prefix("Serving the trace viewer on ") {
      url = rest.trim().to_string();
      break;
    }
  }
  assert!(url.starts_with("http://127.0.0.1:"), "no URL printed, got {url:?}");
  assert!(url.contains("/trace/index.html?trace=file"), "{url}");

  let base = url.split("/trace/").next().expect("base").to_string();
  for (path, expected) in [
    ("/trace/index.html", "text/html"),
    ("/trace/sw.bundle.js", "javascript"),
  ] {
    let (status, content_type, _) = http_get(&format!("{base}{path}"));
    assert_eq!(status, 200, "{path} -> {status}");
    assert!(content_type.contains(expected), "{path} served as {content_type}");
  }

  // The viewer reads the trace through the file route; a path outside the
  // trace's own directory is refused.
  let zip = workspace
    .path()
    .join("test-results/checkout-pays/pays-attempt1.trace.zip")
    .canonicalize()
    .expect("canonicalize");
  let (status, _, body) = http_get(&format!("{base}/trace/file?path={}", encode(&zip.to_string_lossy())));
  assert_eq!(status, 200);
  assert!(body.starts_with(b"PK"), "not a zip: {:?}", &body[..body.len().min(8)]);

  let (status, _, _) = http_get(&format!("{base}/trace/file?path={}", encode("/etc/passwd")));
  assert_eq!(status, 403, "file route must stay inside the trace directory");
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

/// Minimal blocking HTTP GET: (status, content-type, body).
fn http_get(url: &str) -> (u16, String, Vec<u8>) {
  use std::io::Read as _;

  let rest = url.strip_prefix("http://").expect("http url");
  let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
  let mut stream = std::net::TcpStream::connect(host).expect("connect");
  write!(
    stream,
    "GET /{path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
  )
  .expect("request");
  let mut raw = Vec::new();
  stream.read_to_end(&mut raw).expect("read");

  let split = raw
    .windows(4)
    .position(|window| window == b"\r\n\r\n")
    .expect("headers end");
  let head = String::from_utf8_lossy(&raw[..split]).into_owned();
  let body = raw[split + 4..].to_vec();
  let status = head
    .lines()
    .next()
    .and_then(|line| line.split_whitespace().nth(1))
    .and_then(|code| code.parse().ok())
    .unwrap_or(0);
  let content_type = head
    .lines()
    .find(|line| line.to_ascii_lowercase().starts_with("content-type:"))
    .map(|line| line[13..].trim().to_string())
    .unwrap_or_default();
  (status, content_type, body)
}
