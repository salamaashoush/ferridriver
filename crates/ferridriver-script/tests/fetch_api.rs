#![allow(clippy::expect_used, clippy::unwrap_used)]
//! WHATWG `fetch` / `Headers` / `Response` over the shared HTTP core.
//! A throwaway loopback server avoids any external network.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;

use ferridriver_script::{Outcome, PathSandbox, RunContext, RunOptions, ScriptEngine, ScriptEngineConfig};

/// Tiny HTTP/1.1 server: replies `{"method","path","body"}`. Lives for
/// the test, handles a handful of sequential requests, then the socket
/// closes when the listener drops.
fn spawn_echo() -> (String, std::thread::JoinHandle<()>) {
  let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
  let addr = listener.local_addr().expect("addr");
  let url = format!("http://{addr}");
  let h = std::thread::spawn(move || {
    for stream in listener.incoming().take(8) {
      let Ok(mut s) = stream else { break };
      let mut buf = [0u8; 8192];
      let n = s.read(&mut buf).unwrap_or(0);
      let req = String::from_utf8_lossy(&buf[..n]);
      let line = req.lines().next().unwrap_or("");
      let mut it = line.split_whitespace();
      let method = it.next().unwrap_or("GET").to_string();
      let path = it.next().unwrap_or("/").to_string();
      let body = req.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
      let payload = serde_json::json!({ "method": method, "path": path, "body": body }).to_string();
      let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-Test: hello\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        payload.len(),
        payload
      );
      let _ = s.write_all(resp.as_bytes());
      let _ = s.flush();
    }
  });
  (url, h)
}

async fn run(src: &str) -> Outcome {
  let tmp = tempfile::tempdir().expect("tempdir");
  let ctx = RunContext {
    vars: Arc::new(ferridriver_script::InMemoryVars::new()),
    sandbox: Arc::new(PathSandbox::new(tmp.path()).expect("sandbox")),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    plugins: Vec::new(),
    trusted_modules: false,
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
  };
  ScriptEngine::new(ScriptEngineConfig::default())
    .run(src, &[], RunOptions::default(), ctx)
    .await
    .outcome
}

fn val(o: &Outcome) -> &serde_json::Value {
  match o {
    Outcome::Ok { success } => &success.value,
    Outcome::Error { error } => panic!("expected ok, got error: {error:?}"),
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn fetch_get_exposes_status_headers_and_json() {
  let (url, _h) = spawn_echo();
  let o = run(&format!(
    "const r = await fetch('{url}/hello');\
     const j = await r.json();\
     return {{ ok: r.ok, status: r.status, ct: r.headers.get('content-type'), \
       xtest: r.headers.get('X-Test'), method: j.method, path: j.path }};"
  ))
  .await;
  let v = val(&o);
  assert_eq!(v["ok"], serde_json::json!(true));
  assert_eq!(v["status"], serde_json::json!(200));
  assert_eq!(v["method"], serde_json::json!("GET"));
  assert_eq!(v["path"], serde_json::json!("/hello"));
  assert_eq!(v["ct"], serde_json::json!("application/json"));
  assert_eq!(
    v["xtest"],
    serde_json::json!("hello"),
    "Headers.get is case-insensitive"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn fetch_post_sends_method_and_json_body() {
  let (url, _h) = spawn_echo();
  let o = run(&format!(
    "const r = await fetch('{url}/x', {{ method: 'POST', body: {{ a: 1 }}, \
       headers: {{ 'X-Y': 'z' }} }});\
     const j = await r.json();\
     return {{ method: j.method, body: j.body }};"
  ))
  .await;
  let v = val(&o);
  assert_eq!(v["method"], serde_json::json!("POST"));
  assert_eq!(
    v["body"]
      .as_str()
      .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok()),
    Some(serde_json::json!({ "a": 1 })),
    "object body serialized as JSON: {v}"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn headers_class_is_constructible_and_iterable() {
  let o = run(
    "const h = new Headers({ 'A': '1' }); h.append('b', '2'); \
     return { a: h.get('a'), has: h.has('B'), n: h.entries().length };",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["a"], serde_json::json!("1"));
  assert_eq!(v["has"], serde_json::json!(true));
  assert_eq!(v["n"], serde_json::json!(2));
}
