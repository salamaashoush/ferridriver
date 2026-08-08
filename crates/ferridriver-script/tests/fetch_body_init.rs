#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Every WHATWG `BodyInit` type, through every entry point that accepts
//! one: `fetch(url, { body })`, `new Request(input, { body })` and
//! `new Response(body)`.
//!
//! Regression cover for a real corruption: each entry point used to
//! recognise its own subset of `BodyInit`, so a `Uint8Array` body left
//! `fetch` as `{"0":104,"1":105}` under `content-type: application/json`
//! while the same value through `new Request` was sent correctly. They
//! now share one "extract a body" step, and these tests assert the bytes
//! that actually reach the wire.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;

use ferridriver_script::{Outcome, PathSandbox, RunContext, RunOptions, ScriptEngine, ScriptEngineConfig};

/// Echoes the request back as JSON: the raw body bytes plus the request
/// head, so a test can assert both the payload and the `content-type`
/// the body type implied.
fn spawn_echo_body() -> (String, std::thread::JoinHandle<()>) {
  let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
  let addr = listener.local_addr().expect("addr");
  let url = format!("http://{addr}");
  let handle = std::thread::spawn(move || {
    for stream in listener.incoming().take(8) {
      let Ok(mut sock) = stream else { break };
      let mut buf = vec![0u8; 65536];
      let n = sock.read(&mut buf).unwrap_or(0);
      let raw = &buf[..n];
      let split = raw.windows(4).position(|w| w == b"\r\n\r\n");
      // Kept verbatim: reqwest already writes header names lowercase,
      // and the multipart boundary is case-sensitive.
      let head = String::from_utf8_lossy(&raw[..split.unwrap_or(0)]).to_string();
      let body = split.map(|i| raw[i + 4..].to_vec()).unwrap_or_default();
      let payload = serde_json::json!({
        "head": head,
        "body": String::from_utf8_lossy(&body),
        "bytes": body,
      })
      .to_string();
      let mut resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
      )
      .into_bytes();
      resp.extend_from_slice(payload.as_bytes());
      let _ = sock.write_all(&resp);
      let _ = sock.flush();
    }
  });
  (url, handle)
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
    extensions: Vec::new(),
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

/// `fetch` with `body`, returning the server's view of what arrived.
async fn post_body(url: &str, body_expr: &str) -> serde_json::Value {
  let o = run(&format!(
    "const r = await fetch('{url}/x', {{ method: 'POST', body: {body_expr} }});\
     return await r.json();"
  ))
  .await;
  val(&o).clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn fetch_sends_typed_array_bodies_verbatim() {
  let (url, _h) = spawn_echo_body();
  let seen = post_body(&url, "new Uint8Array([104, 105])").await;
  assert_eq!(
    seen["body"],
    serde_json::json!("hi"),
    "a Uint8Array body must reach the wire as its bytes, not as JSON: {seen}"
  );
  // Spec: a bare buffer body implies NO type, so `fetch` must send no
  // `content-type` at all. It used to inherit Playwright's `data`
  // default (`application/octet-stream`) because the fetch global lowered
  // through the `request` option bag; it now builds a `WhatwgRequest`.
  assert!(
    !seen["head"].as_str().unwrap().contains("content-type"),
    "a buffer body implies no content-type: {seen}"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn fetch_sends_non_u8_typed_array_and_dataview_bodies() {
  let (url, _h) = spawn_echo_body();
  // A non-`Uint8Array` view and a `DataView` are `BodyInit` too, and
  // must go out as the underlying bytes.
  let seen = post_body(&url, "new Uint16Array([0x6968])").await;
  assert_eq!(seen["bytes"], serde_json::json!([0x68, 0x69]), "Uint16Array: {seen}");

  let (url2, _h2) = spawn_echo_body();
  let seen = post_body(
    &url2,
    "(() => { const b = new ArrayBuffer(2); new DataView(b).setUint8(0, 1); return new DataView(b); })()",
  )
  .await;
  assert_eq!(seen["bytes"], serde_json::json!([1, 0]), "DataView: {seen}");

  // `Uint8ClampedArray` and `Float16Array` only became readable when the
  // vendored `ObjectBytes` was re-synced against llrt; before that they
  // fell through to the JSON branch and corrupted the body.
  let (url3, _h3) = spawn_echo_body();
  let seen = post_body(&url3, "new Uint8ClampedArray([7, 8])").await;
  assert_eq!(seen["bytes"], serde_json::json!([7, 8]), "Uint8ClampedArray: {seen}");

  let (url4, _h4) = spawn_echo_body();
  let seen = post_body(&url4, "new Float16Array([1])").await;
  assert_eq!(
    seen["bytes"].as_array().map(Vec::len),
    Some(2),
    "Float16Array is two bytes per element: {seen}"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn fetch_sends_array_buffer_body_verbatim() {
  let (url, _h) = spawn_echo_body();
  let seen = post_body(&url, "new Uint8Array([1, 2, 3]).buffer").await;
  assert_eq!(seen["bytes"], serde_json::json!([1, 2, 3]), "ArrayBuffer body: {seen}");
}

#[tokio::test(flavor = "multi_thread")]
async fn fetch_sends_url_search_params_as_form_urlencoded() {
  let (url, _h) = spawn_echo_body();
  let seen = post_body(&url, "new URLSearchParams({ a: '1', b: 'x y' })").await;
  assert_eq!(seen["body"], serde_json::json!("a=1&b=x+y"), "{seen}");
  assert!(
    seen["head"]
      .as_str()
      .unwrap()
      .contains("content-type: application/x-www-form-urlencoded"),
    "URLSearchParams implies a form content type: {seen}"
  );
}

/// Decode an HTTP/1.1 `Transfer-Encoding: chunked` body into
/// `(payload, frame sizes)`. The frame sizes are the interesting part:
/// they show the request was written incrementally rather than buffered
/// and sent in one piece.
fn dechunk(body: &str) -> (String, Vec<usize>) {
  let mut payload = String::new();
  let mut sizes = Vec::new();
  let mut rest = body;
  while let Some((size_line, after)) = rest.split_once("\r\n") {
    let Ok(size) = usize::from_str_radix(size_line.trim(), 16) else {
      break;
    };
    if size == 0 {
      break;
    }
    if after.len() < size {
      break;
    }
    payload.push_str(&after[..size]);
    sizes.push(size);
    rest = after[size..].strip_prefix("\r\n").unwrap_or("");
  }
  (payload, sizes)
}

#[tokio::test(flavor = "multi_thread")]
async fn fetch_streams_a_readable_stream_body_chunk_by_chunk() {
  let (url, _h) = spawn_echo_body();
  let seen = post_body(
    &url,
    "new ReadableStream({ start(c) { \
       c.enqueue(new TextEncoder().encode('chunk-1;')); \
       c.enqueue(new TextEncoder().encode('chunk-2')); \
       c.close(); } })",
  )
  .await;
  let head = seen["head"].as_str().unwrap();
  // The observable that ONLY holds when the body streamed: no
  // `content-length` (its size was unknown at send time), and each
  // enqueued chunk framed separately. A buffered body would have gone
  // out as one `content-length`-sized write.
  assert!(
    head.contains("transfer-encoding: chunked"),
    "a stream body is sent chunked: {seen}"
  );
  assert!(
    !head.contains("content-length"),
    "a stream body has no known length: {seen}"
  );
  let (payload, frames) = dechunk(seen["body"].as_str().unwrap());
  assert_eq!(payload, "chunk-1;chunk-2", "{seen}");
  assert_eq!(frames, vec![8, 7], "each enqueued chunk is its own frame: {seen}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_stream_body_fails_the_request() {
  // A source that throws mid-body must reject the fetch. Ending the
  // body early instead would hand the server a truncated payload it
  // would read as complete.
  let (url, _h) = spawn_echo_body();
  let o = run(&format!(
    "const body = new ReadableStream({{ \
       start(c) {{ c.enqueue(new TextEncoder().encode('partial')); }}, \
       pull(c) {{ throw new Error('source blew up'); }} }});\
     try {{ await fetch('{url}/x', {{ method: 'POST', body }}); return 'NO THROW'; }}\
     catch (e) {{ return 'threw'; }}"
  ))
  .await;
  assert_eq!(val(&o), &serde_json::json!("threw"), "a broken source fails the fetch");
}

#[tokio::test(flavor = "multi_thread")]
async fn fetch_sends_blob_body_with_its_own_type() {
  let (url, _h) = spawn_echo_body();
  let seen = post_body(&url, "new Blob(['hello'], { type: 'text/csv' })").await;
  assert_eq!(seen["body"], serde_json::json!("hello"), "{seen}");
  assert!(
    seen["head"].as_str().unwrap().contains("content-type: text/csv"),
    "a Blob types the body: {seen}"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn fetch_form_data_body_uses_the_core_multipart_serializer() {
  let (url, _h) = spawn_echo_body();
  let seen = post_body(
    &url,
    "(() => { const fd = new FormData(); fd.append('a', '1'); return fd; })()",
  )
  .await;
  let head = seen["head"].as_str().unwrap();
  let boundary = head
    .split("boundary=")
    .nth(1)
    .and_then(|rest| rest.split("\r\n").next())
    .expect("boundary in content-type");
  let body = seen["body"].as_str().unwrap();
  assert!(
    body.starts_with(&format!("--{boundary}\r\n")),
    "the sent boundary must match the declared one: {seen}"
  );
  assert!(
    body.contains("Content-Disposition: form-data; name=\"a\"\r\n\r\n1\r\n"),
    "{seen}"
  );
  assert!(body.ends_with(&format!("--{boundary}--\r\n")), "{seen}");
}

#[tokio::test(flavor = "multi_thread")]
async fn fetch_keeps_the_json_object_body_ergonomic() {
  // ferridriver's documented deviation from WebIDL (which would send
  // "[object Object]"): a plain object body is JSON. It must stay the
  // LAST branch, never shadowing a real BodyInit type.
  let (url, _h) = spawn_echo_body();
  let seen = post_body(&url, "{ a: 1 }").await;
  assert_eq!(seen["body"], serde_json::json!("{\"a\":1}"), "{seen}");
  assert!(
    seen["head"]
      .as_str()
      .unwrap()
      .contains("content-type: application/json"),
    "{seen}"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn constructors_extract_the_same_body_types_as_fetch() {
  let o = run(
    "const out = {};\
     out.usp = await new Request('http://x/', { method: 'POST', body: new URLSearchParams({ a: '1' }) }).text();\
     out.bin = await new Request('http://x/', { method: 'POST', body: new Uint8Array([104, 105]) }).text();\
     const r = new Response(new URLSearchParams({ b: '2' }));\
     out.respBody = await r.text();\
     out.respType = r.headers.get('content-type');\
     const streamed = new Response(new ReadableStream({ \
       start(c) { c.enqueue(new TextEncoder().encode('sss')); c.close(); } }));\
     out.stream = await streamed.text();\
     out.buf = Array.from(new Uint8Array(await new Response(new Uint8Array([7, 8]).buffer).arrayBuffer()));\
     return out;",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["usp"], serde_json::json!("a=1"));
  assert_eq!(v["bin"], serde_json::json!("hi"));
  assert_eq!(v["respBody"], serde_json::json!("b=2"));
  assert_eq!(
    v["respType"],
    serde_json::json!("application/x-www-form-urlencoded;charset=UTF-8")
  );
  assert_eq!(
    v["stream"],
    serde_json::json!("sss"),
    "a stream body is not drained early"
  );
  assert_eq!(v["buf"], serde_json::json!([7, 8]));
}

#[tokio::test(flavor = "multi_thread")]
async fn response_with_a_stream_body_exposes_it_as_body() {
  // The stream handed to the constructor IS the body stream — it is not
  // buffered behind the scenes, so reading `.body` yields those chunks.
  let o = run(
    "const r = new Response(new ReadableStream({ \
       start(c) { c.enqueue(new TextEncoder().encode('ab')); c.close(); } }));\
     const reader = r.body.getReader();\
     const first = await reader.read();\
     return { chunk: Array.from(first.value), done: first.done };",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["chunk"], serde_json::json!([97, 98]));
  assert_eq!(v["done"], serde_json::json!(false));
}

#[tokio::test(flavor = "multi_thread")]
async fn primitive_bodies_stringify_per_webidl() {
  let o = run("return { n: await new Response(42).text(), b: await new Response(true).text() };").await;
  let v = val(&o);
  assert_eq!(v["n"], serde_json::json!("42"), "a number body stringifies");
  assert_eq!(v["b"], serde_json::json!("true"));
}

#[tokio::test(flavor = "multi_thread")]
async fn headers_iteration_is_live_and_sorted() {
  // The `Headers` iterators project the core list per step rather than
  // snapshotting it, so an append made mid-iteration is observed.
  let o = run(
    "const h = new Headers({ b: '2' });\
     const seen = [];\
     for (const [k] of h) { seen.push(k); if (k === 'b') h.append('c', '3'); }\
     const sorted = [...new Headers([['z', '1'], ['a', '2']]).keys()];\
     return { seen, sorted };",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["seen"], serde_json::json!(["b", "c"]), "live iteration: {v}");
  assert_eq!(v["sorted"], serde_json::json!(["a", "z"]), "sorted by name: {v}");
}
