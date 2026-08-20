#![allow(clippy::expect_used, clippy::unwrap_used)]
//! WHATWG `fetch` / `Headers` / `Response` over the shared HTTP core.
//! A throwaway loopback server avoids any external network.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;

use ferridriver_script::{Outcome, RunContext, RunOptions, ScriptEngine, ScriptEngineConfig};

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

/// A server that accepts a connection then sleeps before replying, so an
/// in-flight `fetch` can be aborted before any response arrives.
fn spawn_slow() -> (String, std::thread::JoinHandle<()>) {
  let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
  let addr = listener.local_addr().expect("addr");
  let url = format!("http://{addr}");
  let h = std::thread::spawn(move || {
    for stream in listener.incoming().take(2) {
      let Ok(mut s) = stream else { break };
      let mut buf = [0u8; 1024];
      let _ = s.read(&mut buf);
      std::thread::sleep(std::time::Duration::from_millis(1500));
      let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi");
      let _ = s.flush();
    }
  });
  (url, h)
}

/// Echoes the raw request body verbatim (no JSON wrapping) so a
/// multipart payload survives intact for inspection.
fn spawn_raw() -> (String, std::thread::JoinHandle<()>) {
  let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
  let addr = listener.local_addr().expect("addr");
  let url = format!("http://{addr}");
  let h = std::thread::spawn(move || {
    for stream in listener.incoming().take(2) {
      let Ok(mut s) = stream else { break };
      let mut buf = [0u8; 16384];
      let n = s.read(&mut buf).unwrap_or(0);
      let raw = &buf[..n];
      let body = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| raw[i + 4..].to_vec())
        .unwrap_or_default();
      let mut resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
      )
      .into_bytes();
      resp.extend_from_slice(&body);
      let _ = s.write_all(&resp);
      let _ = s.flush();
    }
  });
  (url, h)
}

async fn run(src: &str) -> Outcome {
  let tmp = tempfile::tempdir().expect("tempdir");
  let ctx = RunContext {
    vars: Arc::new(ferridriver_script::InMemoryVars::new()),
    script_root: tmp.path().into(),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    extensions: Vec::new(),
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
    session: None,
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
     return { a: h.get('a'), has: h.has('B'), n: [...h.entries()].length };",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["a"], serde_json::json!("1"));
  assert_eq!(v["has"], serde_json::json!(true), "has is case-insensitive");
  assert_eq!(v["n"], serde_json::json!(2));
}

#[tokio::test(flavor = "multi_thread")]
async fn headers_append_combines_and_set_cookie_stays_separate() {
  let o = run(
    "const h = new Headers(); \
     h.append('Accept-Encoding', 'gzip'); h.append('accept-encoding', 'br'); \
     h.append('Set-Cookie', 'a=1'); h.append('set-cookie', 'b=2'); \
     h.set('X-One', 'first'); h.set('x-one', 'second'); \
     return { ae: h.get('accept-encoding'), \
       sc: h.get('set-cookie'), scList: h.getSetCookie(), \
       one: h.get('X-One'), missing: h.get('nope') };",
  )
  .await;
  let v = val(&o);
  assert_eq!(
    v["ae"],
    serde_json::json!("gzip, br"),
    "same-name values combine with ', '"
  );
  assert_eq!(
    v["sc"],
    serde_json::json!("a=1, b=2"),
    "get('set-cookie') returns the combined value"
  );
  assert_eq!(
    v["scList"],
    serde_json::json!(["a=1", "b=2"]),
    "getSetCookie returns each set-cookie separately"
  );
  assert_eq!(v["one"], serde_json::json!("second"), "set replaces all of a name");
  assert_eq!(v["missing"], serde_json::Value::Null, "absent header is null");
}

#[tokio::test(flavor = "multi_thread")]
async fn headers_real_iterators_and_sorted_order() {
  let o = run(
    "const h = new Headers([['x-b','2'],['x-a','1']]); h.append('x-a','3'); \
     const it = h.entries(); const first = it.next(); \
     const rest = [...it]; \
     return { firstDone: first.done, first: first.value, rest, \
       keys: [...h.keys()], vals: [...h.values()], \
       selfIter: typeof h[Symbol.iterator], \
       spread: [...h], \
       reIter: [...h.keys()[Symbol.iterator]()] };",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["firstDone"], serde_json::json!(false));
  // Sorted by name: x-a (combined) before x-b.
  assert_eq!(v["first"], serde_json::json!(["x-a", "1, 3"]));
  assert_eq!(
    v["rest"],
    serde_json::json!([["x-b", "2"]]),
    "iterator continues from cursor"
  );
  assert_eq!(v["keys"], serde_json::json!(["x-a", "x-b"]));
  assert_eq!(v["vals"], serde_json::json!(["1, 3", "2"]));
  assert_eq!(v["selfIter"], serde_json::json!("function"));
  assert_eq!(v["spread"], serde_json::json!([["x-a", "1, 3"], ["x-b", "2"]]));
  assert_eq!(
    v["reIter"],
    serde_json::json!(["x-a", "x-b"]),
    "iterator is itself iterable (Symbol.iterator yields a fresh cursor)"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn headers_for_each_normalization_and_validation() {
  let o = run(
    "const h = new Headers(); h.set('X-Trim', '  spaced\\tvalue  '); \
     const seen = []; h.forEach((v, k) => seen.push([k, v])); \
     let threwName = false; try { h.set('bad name', 'x'); } catch (e) { threwName = e instanceof TypeError; } \
     let threwCtor = false; try { new Headers(5); } catch (e) { threwCtor = e instanceof TypeError; } \
     const copy = new Headers(h); \
     return { trimmed: h.get('x-trim'), seen, threwName, threwCtor, copy: copy.get('x-trim') };",
  )
  .await;
  let v = val(&o);
  assert_eq!(
    v["trimmed"],
    serde_json::json!("spaced\tvalue"),
    "leading/trailing HTTP whitespace stripped, inner kept"
  );
  assert_eq!(v["seen"], serde_json::json!([["x-trim", "spaced\tvalue"]]));
  assert_eq!(v["threwName"], serde_json::json!(true), "invalid name -> TypeError");
  assert_eq!(v["threwCtor"], serde_json::json!(true), "Headers(number) -> TypeError");
  assert_eq!(
    v["copy"],
    serde_json::json!("spaced\tvalue"),
    "constructible from a Headers"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn response_is_constructible_with_spec_surface() {
  let o = run(
    "const r = new Response('hi', { status: 201, statusText: 'Created', headers: { 'X-A': 'b' } }); \
     const beforeUsed = r.bodyUsed; \
     const cloned = r.clone(); \
     const body = await r.text(); \
     let reread = false; try { await r.text(); } catch (e) { reread = e instanceof TypeError; } \
     let cloneAfter = false; try { r.clone(); } catch (e) { cloneAfter = e instanceof TypeError; } \
     return { status: r.status, ok: r.ok, statusText: r.statusText, type: r.type, \
       url: r.url, redirected: r.redirected, xa: r.headers.get('x-a'), \
       beforeUsed, afterUsed: r.bodyUsed, body, reread, cloneAfter, \
       clonedBody: await cloned.text(), \
       isResp: r instanceof Response };",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["status"], serde_json::json!(201));
  assert_eq!(v["ok"], serde_json::json!(true), "201 is ok");
  assert_eq!(v["statusText"], serde_json::json!("Created"));
  assert_eq!(v["type"], serde_json::json!("default"));
  assert_eq!(v["url"], serde_json::json!(""));
  assert_eq!(v["redirected"], serde_json::json!(false));
  assert_eq!(v["xa"], serde_json::json!("b"));
  assert_eq!(v["beforeUsed"], serde_json::json!(false));
  assert_eq!(v["afterUsed"], serde_json::json!(true));
  assert_eq!(v["body"], serde_json::json!("hi"));
  assert_eq!(v["reread"], serde_json::json!(true), "second body read -> TypeError");
  assert_eq!(v["cloneAfter"], serde_json::json!(true), "clone after use -> TypeError");
  assert_eq!(
    v["clonedBody"],
    serde_json::json!("hi"),
    "clone keeps an independent body"
  );
  assert_eq!(v["isResp"], serde_json::json!(true), "instanceof Response");
}

#[tokio::test(flavor = "multi_thread")]
async fn response_static_helpers() {
  let o = run(
    "const j = Response.json({ a: 1 }, { status: 202 }); \
     const e = Response.error(); \
     const rd = Response.redirect('http://x/y', 301); \
     let badRange = false; try { Response.redirect('http://x', 200); } catch (er) { badRange = er instanceof RangeError; } \
     return { jStatus: j.status, jCt: j.headers.get('content-type'), jBody: await j.json(), \
       eStatus: e.status, eType: e.type, \
       rdStatus: rd.status, rdLoc: rd.headers.get('location'), badRange };",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["jStatus"], serde_json::json!(202));
  assert_eq!(v["jCt"], serde_json::json!("application/json"));
  assert_eq!(v["jBody"], serde_json::json!({ "a": 1 }));
  assert_eq!(v["eStatus"], serde_json::json!(0), "Response.error() status 0");
  assert_eq!(v["eType"], serde_json::json!("error"));
  assert_eq!(v["rdStatus"], serde_json::json!(301));
  assert_eq!(v["rdLoc"], serde_json::json!("http://x/y"));
  assert_eq!(
    v["badRange"],
    serde_json::json!(true),
    "non-redirect status -> RangeError"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn request_is_constructible_and_clonable() {
  let o = run(
    "const a = new Request('http://x/p', { method: 'post', headers: { 'X-A': 'b' }, body: 'hello', \
       redirect: 'manual', credentials: 'include' }); \
     const b = new Request(a); \
     const ab = await a.text(); \
     let reread = false; try { await a.text(); } catch (e) { reread = e instanceof TypeError; } \
     return { url: a.url, method: a.method, xa: a.headers.get('x-a'), \
       redirect: a.redirect, credentials: a.credentials, ab, reread, \
       bUrl: b.url, bMethod: b.method, isReq: a instanceof Request };",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["url"], serde_json::json!("http://x/p"));
  assert_eq!(v["method"], serde_json::json!("POST"), "method upper-cased");
  assert_eq!(v["xa"], serde_json::json!("b"));
  assert_eq!(v["redirect"], serde_json::json!("manual"));
  assert_eq!(v["credentials"], serde_json::json!("include"));
  assert_eq!(v["ab"], serde_json::json!("hello"));
  assert_eq!(v["reread"], serde_json::json!(true));
  assert_eq!(
    v["bUrl"],
    serde_json::json!("http://x/p"),
    "constructible from a Request"
  );
  assert_eq!(v["bMethod"], serde_json::json!("POST"));
  assert_eq!(v["isReq"], serde_json::json!(true), "instanceof Request");
}

#[tokio::test(flavor = "multi_thread")]
async fn fetch_accepts_a_request_instance() {
  let (url, _h) = spawn_echo();
  let o = run(&format!(
    "const req = new Request('{url}/r', {{ method: 'POST', body: {{ a: 1 }} }}); \
     const r = await fetch(req); const j = await r.json(); \
     return {{ method: j.method, path: j.path, body: j.body, type: r.type }};"
  ))
  .await;
  let v = val(&o);
  assert_eq!(
    v["method"],
    serde_json::json!("POST"),
    "fetch reads method off a Request"
  );
  assert_eq!(v["path"], serde_json::json!("/r"));
  assert_eq!(
    v["body"]
      .as_str()
      .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok()),
    Some(serde_json::json!({ "a": 1 })),
    "Request body forwarded"
  );
  assert_eq!(v["type"], serde_json::json!("basic"), "fetched Response type is basic");
}

#[tokio::test(flavor = "multi_thread")]
async fn abort_controller_signal_and_listeners() {
  let o = run(
    "const c = new AbortController(); const s = c.signal; \
     const before = s.aborted; let fired = null; let evt = 0; \
     s.onabort = () => { fired = s.reason && s.reason.name; }; \
     s.addEventListener('abort', () => { evt++; }); \
     c.abort(); c.abort(); \
     return { before, after: s.aborted, fired, evt, reasonName: s.reason && s.reason.name, \
       isSignal: s instanceof AbortSignal };",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["before"], serde_json::json!(false));
  assert_eq!(v["after"], serde_json::json!(true));
  assert_eq!(
    v["fired"],
    serde_json::json!("AbortError"),
    "onabort fired and saw the DOMException reason on the signal"
  );
  assert_eq!(
    v["evt"],
    serde_json::json!(1),
    "listener fires exactly once (abort is idempotent)"
  );
  assert_eq!(v["reasonName"], serde_json::json!("AbortError"));
  assert_eq!(v["isSignal"], serde_json::json!(true));
}

#[tokio::test(flavor = "multi_thread")]
async fn abort_custom_reason_throw_if_aborted_and_statics() {
  let o = run(
    "const c = new AbortController(); c.abort('boom'); \
     let t = false; try { c.signal.throwIfAborted(); } catch (e) { t = (e === 'boom'); } \
     const sa = AbortSignal.abort('x'); \
     const c2 = new AbortController(); const any = AbortSignal.any([c2.signal, c.signal]); \
     return { reason: c.signal.reason, t, saAborted: sa.aborted, saReason: sa.reason, \
       anyAborted: any.aborted };",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["reason"], serde_json::json!("boom"), "custom reason preserved");
  assert_eq!(v["t"], serde_json::json!(true), "throwIfAborted throws the reason");
  assert_eq!(
    v["saAborted"],
    serde_json::json!(true),
    "AbortSignal.abort is pre-aborted"
  );
  assert_eq!(v["saReason"], serde_json::json!("x"));
  assert_eq!(
    v["anyAborted"],
    serde_json::json!(true),
    "AbortSignal.any is aborted if an input already is"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn abort_signal_timeout_and_any_propagation() {
  let o = run(
    "const t = AbortSignal.timeout(10); const t0 = t.aborted; \
     const c = new AbortController(); const any = AbortSignal.any([c.signal]); \
     let anyFired = false; any.addEventListener('abort', () => { anyFired = true; }); \
     await new Promise((r) => setTimeout(r, 80)); \
     c.abort(); \
     return { t0, tAborted: t.aborted, tName: t.reason && t.reason.name, \
       anyAborted: any.aborted, anyFired };",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["t0"], serde_json::json!(false), "timeout signal starts un-aborted");
  assert_eq!(v["tAborted"], serde_json::json!(true), "timeout fires after the delay");
  assert_eq!(v["tName"], serde_json::json!("TimeoutError"));
  assert_eq!(v["anyAborted"], serde_json::json!(true), "any() follows a later abort");
  assert_eq!(v["anyFired"], serde_json::json!(true), "any() forwards the abort event");
}

#[tokio::test(flavor = "multi_thread")]
async fn fetch_rejects_when_signal_already_aborted() {
  let o = run(
    "const c = new AbortController(); c.abort(); let err = null; \
     try { await fetch('http://127.0.0.1:1/', { signal: c.signal }); } \
     catch (e) { err = String(e.message || e); } return { err };",
  )
  .await;
  let v = val(&o);
  let err = v["err"].as_str().unwrap_or_default();
  assert!(
    err.to_lowercase().contains("abort"),
    "an already-aborted signal must reject fetch before I/O, got: {err}"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn fetch_aborts_an_in_flight_request() {
  let (url, _h) = spawn_slow();
  let started = std::time::Instant::now();
  let o = run(&format!(
    "const c = new AbortController(); \
     const p = fetch('{url}/slow', {{ signal: c.signal }}); \
     setTimeout(() => c.abort(), 30); \
     let err = null; try {{ await p; }} catch (e) {{ err = String(e.message || e); }} \
     return {{ err }};"
  ))
  .await;
  let elapsed = started.elapsed();
  let v = val(&o);
  let err = v["err"].as_str().unwrap_or_default();
  assert!(
    err.to_lowercase().contains("abort"),
    "in-flight fetch must reject on abort, got: {err}"
  );
  assert!(
    elapsed < std::time::Duration::from_millis(1200),
    "abort must drop the request future, not wait for the 1.5s server: {elapsed:?}"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn response_body_is_a_readable_stream() {
  let (url, _h) = spawn_echo();
  let o = run(&format!(
    "const r = await fetch('{url}/s'); \
     const reader = r.body.getReader(); const dec = new TextDecoder(); let out = ''; \
     for (;;) {{ const {{ value, done }} = await reader.read(); if (done) break; out += dec.decode(value); }} \
     const after = await reader.read(); \
     const j = JSON.parse(out); \
     return {{ path: j.path, method: j.method, doneAgain: after.done, isStream: r.body instanceof ReadableStream }};"
  ))
  .await;
  let v = val(&o);
  assert_eq!(v["path"], serde_json::json!("/s"), "stream reassembles the body");
  assert_eq!(v["method"], serde_json::json!("GET"));
  assert_eq!(v["doneAgain"], serde_json::json!(true), "reader is done after drain");
  assert_eq!(
    v["isStream"],
    serde_json::json!(true),
    "Response.body instanceof ReadableStream"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn response_body_async_iteration() {
  let (url, _h) = spawn_echo();
  let o = run(&format!(
    "const r = await fetch('{url}/ai'); const dec = new TextDecoder(); let out = ''; \
     for await (const chunk of r.body) {{ out += dec.decode(chunk); }} \
     return {{ path: JSON.parse(out).path }};"
  ))
  .await;
  let v = val(&o);
  assert_eq!(
    v["path"],
    serde_json::json!("/ai"),
    "for-await over Response.body works"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn readable_stream_constructible_and_locking() {
  let o = run(
    "const s = new ReadableStream({ start(c) { c.enqueue('ab'); c.enqueue(new Uint8Array([99])); c.close(); } }); \
     const before = s.locked; const rd = s.getReader(); const afterLock = s.locked; \
     let dbl = false; try { s.getReader(); } catch (e) { dbl = e instanceof TypeError; } \
     const a = await rd.read(); const b = await rd.read(); const end = await rd.read(); \
     rd.releaseLock(); const unlocked = s.locked; \
     return { before, afterLock, dbl, a: a.value, aDone: a.done, \
       b: Array.from(b.value), endDone: end.done, unlocked, \
       isReader: rd instanceof ReadableStreamDefaultReader };",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["before"], serde_json::json!(false));
  assert_eq!(v["afterLock"], serde_json::json!(true), "getReader locks the stream");
  assert_eq!(v["dbl"], serde_json::json!(true), "second getReader -> TypeError");
  assert_eq!(
    v["a"],
    serde_json::json!("ab"),
    "a default ReadableStream passes chunks through untouched (no byte coercion)"
  );
  assert_eq!(v["aDone"], serde_json::json!(false));
  assert_eq!(v["b"], serde_json::json!([99]), "Uint8Array chunk preserved");
  assert_eq!(v["endDone"], serde_json::json!(true), "closed stream ends");
  assert_eq!(v["unlocked"], serde_json::json!(false), "releaseLock unlocks");
  assert_eq!(v["isReader"], serde_json::json!(true));
}

#[tokio::test(flavor = "multi_thread")]
async fn blob_construct_slice_and_stream() {
  let o = run(
    "const b = new Blob(['ab', new Uint8Array([99]), new Blob(['d'])], { type: 'TEXT/Plain' }); \
     const sl = b.slice(1, 3); \
     const r = b.stream().getReader(); const first = await r.read(); \
     return { size: b.size, type: b.type, text: await b.text(), \
       slice: await sl.text(), sliceType: sl.type, \
       isBlob: b instanceof Blob, streamIsStream: b.stream() instanceof ReadableStream, \
       firstChunk: Array.from(first.value) };",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["size"], serde_json::json!(4), "ab + 0x63 + d");
  assert_eq!(v["type"], serde_json::json!("text/plain"), "type lowercased");
  assert_eq!(v["text"], serde_json::json!("abcd"));
  assert_eq!(v["slice"], serde_json::json!("bc"), "slice(1,3) of abcd");
  assert_eq!(v["sliceType"], serde_json::json!(""));
  assert_eq!(v["isBlob"], serde_json::json!(true));
  assert_eq!(v["streamIsStream"], serde_json::json!(true));
  assert_eq!(
    v["firstChunk"],
    serde_json::json!([97, 98, 99, 100]),
    "stream yields the bytes"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn formdata_surface() {
  let o = run(
    "const fd = new FormData(); fd.append('a', '1'); fd.append('a', '2'); \
     fd.append('f', new Blob(['hi'], { type: 'text/plain' }), 'note.txt'); \
     fd.set('a', 'only'); \
     const fileVal = fd.get('f'); \
     const seen = []; fd.forEach((v, k) => seen.push(k)); \
     return { a: fd.get('a'), all: fd.getAll('a'), hasF: fd.has('f'), \
       fileIsBlob: fileVal instanceof Blob, fileText: await fileVal.text(), \
       keys: [...fd.keys()], entriesLen: [...fd.entries()].length, seen, \
       isFD: fd instanceof FormData, removed: (fd.delete('a'), fd.has('a')) };",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["a"], serde_json::json!("only"), "set replaces all of a name");
  assert_eq!(v["all"], serde_json::json!(["only"]));
  assert_eq!(v["hasF"], serde_json::json!(true));
  assert_eq!(
    v["fileIsBlob"],
    serde_json::json!(true),
    "file entry reads back as a Blob"
  );
  assert_eq!(v["fileText"], serde_json::json!("hi"));
  assert_eq!(v["keys"], serde_json::json!(["a", "f"]));
  assert_eq!(v["entriesLen"], serde_json::json!(2));
  assert_eq!(v["seen"], serde_json::json!(["a", "f"]), "forEach yields (value, key)");
  assert_eq!(v["isFD"], serde_json::json!(true));
  assert_eq!(v["removed"], serde_json::json!(false), "delete removes the name");
}

#[tokio::test(flavor = "multi_thread")]
async fn fetch_blob_and_formdata_bodies() {
  let (url, _h) = spawn_echo();
  let blob = run(&format!(
    "const r = await fetch('{url}/b', {{ method: 'POST', body: new Blob(['payload']) }}); \
     return {{ body: (await r.json()).body }};"
  ))
  .await;
  assert_eq!(
    val(&blob)["body"],
    serde_json::json!("payload"),
    "Blob body sent as raw bytes"
  );

  let (url2, _h2) = spawn_raw();
  let fd = run(&format!(
    "const fd = new FormData(); fd.append('field', 'value'); \
     fd.append('file', new Blob(['filedata'], {{ type: 'text/plain' }}), 'a.txt'); \
     const r = await fetch('{url2}/m', {{ method: 'POST', body: fd }}); \
     return {{ body: await r.text() }};"
  ))
  .await;
  let body = val(&fd)["body"].as_str().unwrap_or_default();
  assert!(
    body.contains("Content-Disposition: form-data; name=\"field\"") && body.contains("value"),
    "multipart contains the text field: {body}"
  );
  assert!(
    body.contains("filename=\"a.txt\"") && body.contains("filedata") && body.contains("Content-Type: text/plain"),
    "multipart contains the file part: {body}"
  );
}

/// Sends the body in two parts with a gap, so a reader that gets the
/// first chunk before the second is written proves incremental (not
/// fully-buffered) delivery.
fn spawn_chunked() -> (String, std::thread::JoinHandle<()>) {
  let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
  let addr = listener.local_addr().expect("addr");
  let url = format!("http://{addr}");
  let h = std::thread::spawn(move || {
    for stream in listener.incoming().take(2) {
      let Ok(mut s) = stream else { break };
      let mut buf = [0u8; 1024];
      let _ = s.read(&mut buf);
      let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nAAA");
      let _ = s.flush();
      std::thread::sleep(std::time::Duration::from_millis(400));
      let _ = s.write_all(b"BBB");
      let _ = s.flush();
    }
  });
  (url, h)
}

#[tokio::test(flavor = "multi_thread")]
async fn response_body_streams_incrementally() {
  let (url, _h) = spawn_chunked();
  let started = std::time::Instant::now();
  let o = run(&format!(
    "const r = await fetch('{url}/c'); const rd = r.body.getReader(); const dec = new TextDecoder(); \
     const a = await rd.read(); \
     const rest = []; for (;;) {{ const x = await rd.read(); if (x.done) break; rest.push(dec.decode(x.value)); }} \
     return {{ first: dec.decode(a.value), all: dec.decode(a.value) + rest.join('') }};"
  ))
  .await;
  let first_read_elapsed = started.elapsed();
  let v = val(&o);
  assert_eq!(
    v["first"],
    serde_json::json!("AAA"),
    "first chunk arrives before the rest"
  );
  assert_eq!(
    v["all"],
    serde_json::json!("AAABBB"),
    "stream reassembles the full body"
  );
  // The whole read finishes only after the 400ms gap, but the point is
  // the body was delivered in >1 network chunk (incremental, not one
  // buffered blob) — assert it did not hang absurdly.
  assert!(
    first_read_elapsed < std::time::Duration::from_secs(3),
    "streamed read completed: {first_read_elapsed:?}"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn fetch_to_cloud_metadata_is_blocked_by_default() {
  // No allow-list (the default top-level-script posture): the cloud
  // instance-metadata endpoint must still be refused — closes the
  // default-open SSRF. No I/O happens; preflight denies.
  let o = run(
    "try { await fetch('http://169.254.169.254/latest/meta-data/'); return 'REACHED'; } \
     catch (e) { return String(e); }",
  )
  .await;
  let v = val(&o);
  let s = v.as_str().unwrap_or("");
  assert!(s != "REACHED", "metadata endpoint must not be reachable");
  assert!(s.contains("blocked address"), "blocked with a clear reason: {s}");
}

#[tokio::test(flavor = "multi_thread")]
async fn atob_is_whatwg_forgiving() {
  // Embedded ASCII whitespace is stripped and missing '=' padding is
  // tolerated (forgiving-base64) — `base64::STANDARD` does neither.
  let o = run(
    "return { ws: atob(' a G V s b G 8 = '), nopad: atob('aGVsbG8'), \
       rt: atob(btoa('xy')) };",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["ws"], serde_json::json!("hello"), "whitespace stripped");
  assert_eq!(v["nopad"], serde_json::json!("hello"), "missing padding tolerated");
  assert_eq!(v["rt"], serde_json::json!("xy"), "round-trips");
}

#[tokio::test(flavor = "multi_thread")]
async fn url_component_setters_roundtrip() {
  let o = run(
    "const u = new URL('https://old.test/a?x=1#h'); \
     u.protocol = 'http:'; u.hostname = 'new.test'; u.port = '8080'; \
     u.pathname = '/b/c'; u.search = '?y=2'; u.hash = '#z'; \
     u.username = 'usr'; u.password = 'pw'; \
     return { href: u.href, user: u.username, pass: u.password, port: u.port };",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["href"], serde_json::json!("http://usr:pw@new.test:8080/b/c?y=2#z"));
  assert_eq!(v["user"], serde_json::json!("usr"));
  assert_eq!(v["pass"], serde_json::json!("pw"));
  assert_eq!(v["port"], serde_json::json!("8080"));
}

#[tokio::test(flavor = "multi_thread")]
async fn response_null_body_status_throws() {
  let o = run(
    "try { new Response('x', { status: 204 }); return 'NO_THROW'; } \
     catch (e) { return e.name; }",
  )
  .await;
  assert_eq!(val(&o), &serde_json::json!("TypeError"));
}

#[tokio::test(flavor = "multi_thread")]
async fn json_arg_proto_key_does_not_pollute() {
  // A `__proto__` key in untrusted arg JSON must become an own data
  // property, not retarget the object's prototype or pollute globals.
  let tmp = tempfile::tempdir().expect("tempdir");
  let ctx = RunContext {
    vars: Arc::new(ferridriver_script::InMemoryVars::new()),
    script_root: tmp.path().into(),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    extensions: Vec::new(),
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
    session: None,
  };
  let args = vec![serde_json::json!({ "__proto__": { "polluted": true } })];
  let out = ScriptEngine::new(ScriptEngineConfig::default())
    .run(
      "return { own: Object.prototype.hasOwnProperty.call(args[0], '__proto__'), \
         globalClean: ({}).polluted === undefined };",
      &args,
      RunOptions::default(),
      ctx,
    )
    .await
    .outcome;
  let v = val(&out);
  assert_eq!(v["own"], serde_json::json!(true), "__proto__ is an own data property");
  assert_eq!(
    v["globalClean"],
    serde_json::json!(true),
    "Object.prototype not polluted"
  );
}

/// Router for the WHATWG redirect/credentials tests. Routes:
/// - `/redirect` -> 302 `Location: /landed`
/// - `/landed`   -> 200 "landed"
/// - `/set`      -> 200 `Set-Cookie: sid=abc; Path=/`, body "set"
/// - `/echo`     -> 200, body = the received `Cookie` header (or "none")
fn spawn_router() -> (String, std::thread::JoinHandle<()>) {
  let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
  let addr = listener.local_addr().expect("addr");
  let url = format!("http://{addr}");
  let h = std::thread::spawn(move || {
    for stream in listener.incoming().take(16) {
      let Ok(mut s) = stream else { break };
      let mut buf = [0u8; 8192];
      let n = s.read(&mut buf).unwrap_or(0);
      let req = String::from_utf8_lossy(&buf[..n]);
      let path = req
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .to_string();
      let cookie = req
        .lines()
        .find_map(|l| l.strip_prefix("Cookie: ").or_else(|| l.strip_prefix("cookie: ")))
        .map_or_else(|| "none".to_string(), |v| v.trim().to_string());
      let resp = if path == "/redirect" {
        "HTTP/1.1 302 Found\r\nLocation: /landed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
      } else if path == "/set" {
        "HTTP/1.1 200 OK\r\nSet-Cookie: sid=abc; Path=/\r\nContent-Length: 3\r\nConnection: close\r\n\r\nset"
          .to_string()
      } else if path == "/echo" {
        format!(
          "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{cookie}",
          cookie.len()
        )
      } else {
        "HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nlanded".to_string()
      };
      let _ = s.write_all(resp.as_bytes());
      let _ = s.flush();
    }
  });
  (url, h)
}

#[tokio::test(flavor = "multi_thread")]
async fn fetch_follows_redirect_marks_type_basic_and_redirected() {
  let (base, _h) = spawn_router();
  let o = run(&format!(
    r#"const r = await fetch("{base}/redirect");
       return {{ status: r.status, redirected: r.redirected, type: r.type,
                 url: r.url, body: await r.text() }};"#
  ))
  .await;
  let v = val(&o);
  assert_eq!(v["status"], 200);
  assert_eq!(v["redirected"], true, "a followed hop sets redirected");
  assert_eq!(v["type"], "basic");
  assert!(
    v["url"].as_str().unwrap().ends_with("/landed"),
    "final url: {}",
    v["url"]
  );
  assert_eq!(v["body"], "landed");
}

#[tokio::test(flavor = "multi_thread")]
async fn fetch_redirect_manual_yields_opaqueredirect() {
  let (base, _h) = spawn_router();
  let o = run(&format!(
    r#"const r = await fetch("{base}/redirect", {{ redirect: "manual" }});
       return {{ status: r.status, type: r.type, url: r.url, ok: r.ok }};"#
  ))
  .await;
  let v = val(&o);
  // WHATWG opaque-redirect filtered response: type opaqueredirect, status
  // 0, empty url, not ok.
  assert_eq!(v["type"], "opaqueredirect");
  assert_eq!(v["status"], 0);
  assert_eq!(v["url"], "");
  assert_eq!(v["ok"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn fetch_redirect_error_rejects_with_typeerror() {
  let (base, _h) = spawn_router();
  let o = run(&format!(
    r#"try {{ const r = await fetch("{base}/redirect", {{ redirect: "error" }}); return "no-throw:" + r.status; }}
       catch (e) {{ return {{ name: e.name, msg: String(e.message || e) }}; }}"#
  ))
  .await;
  let v = val(&o);
  assert_ne!(v, "no-throw", "redirect:error must reject on a 3xx");
  assert_eq!(v["name"], "TypeError", "a fetch network failure is a TypeError");
  assert!(v["msg"].as_str().unwrap().contains("redirect"), "msg: {}", v["msg"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn fetch_signal_carried_on_request_instance_aborts() {
  let (base, _h) = spawn_echo();
  let o = run(&format!(
    r#"const c = new AbortController();
       c.abort();
       const req = new Request("{base}/x", {{ signal: c.signal }});
       try {{ await fetch(req); return "no-throw"; }}
       catch (e) {{ return String(e.message || e); }}"#
  ))
  .await;
  let v = val(&o);
  let err = v.as_str().unwrap_or_default();
  // The signal is carried on the Request instance (not init), so this
  // rejects only if `fetch(request)` forwards it. (`AbortError` surfaces
  // via message here — this runtime has no DOMException class yet.)
  assert!(
    err.to_lowercase().contains("abort"),
    "a signal carried on the Request must reject fetch, got: {err}"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn fetch_credentials_omit_skips_stored_cookies() {
  let (base, _h) = spawn_router();
  let o = run(&format!(
    r#"await fetch("{base}/set");
       const withCreds = await (await fetch("{base}/echo")).text();
       const omitted = await (await fetch("{base}/echo", {{ credentials: "omit" }})).text();
       return {{ withCreds, omitted }};"#
  ))
  .await;
  let v = val(&o);
  // The jar-backed default sends the stored cookie; `omit` rides a
  // jar-less client so none is sent.
  assert_eq!(v["withCreds"], "sid=abc");
  assert_eq!(v["omitted"], "none");
}

#[tokio::test(flavor = "multi_thread")]
async fn response_body_is_one_stable_stream_and_text_drains_it() {
  let (url, _h) = spawn_echo();
  let o = run(&format!(
    "const r = await fetch('{url}/s');\
     const same = r.body === r.body;\
     const t = await r.text();\
     let second = null; try {{ await r.text(); }} catch (e) {{ second = e.message; }}\
     return {{ same, hasBody: t.length > 0, used: r.bodyUsed, second }};"
  ))
  .await;
  let v = val(&o);
  assert_eq!(
    v["same"],
    serde_json::json!(true),
    "`.body` returns the same stream object every time"
  );
  assert_eq!(v["hasBody"], serde_json::json!(true), "text() drained the stream");
  assert_eq!(v["used"], serde_json::json!(true));
  assert!(
    v["second"]
      .as_str()
      .unwrap_or_default()
      .contains("already been consumed"),
    "a second read is a TypeError: {v}"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn response_text_after_get_reader_is_refused() {
  let (url, _h) = spawn_echo();
  let o = run(&format!(
    "const r = await fetch('{url}/s'); r.body.getReader();\
     try {{ await r.text(); return 'no throw'; }} catch (e) {{ return e.message; }}"
  ))
  .await;
  assert!(
    val(&o).as_str().unwrap_or_default().contains("locked"),
    "a body locked to a reader cannot also be drained by text(): {}",
    val(&o)
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn clone_tees_an_unread_streamed_body() {
  // The body has NOT been buffered when `clone()` runs — the two
  // responses must each read the full payload off one socket via a tee.
  let (url, _h) = spawn_chunked();
  let o = run(&format!(
    "const r = await fetch('{url}/c'); const c = r.clone();\
     const [a, b] = await Promise.all([r.text(), c.text()]);\
     return {{ a, b }};"
  ))
  .await;
  let v = val(&o);
  assert_eq!(
    v["a"],
    serde_json::json!("AAABBB"),
    "the original still reads the whole body"
  );
  assert_eq!(
    v["b"],
    serde_json::json!("AAABBB"),
    "the clone reads the same bytes from the tee'd branch"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn clone_of_a_used_response_throws() {
  let (url, _h) = spawn_echo();
  let o = run(&format!(
    "const r = await fetch('{url}/s'); await r.text();\
     try {{ r.clone(); return 'no throw'; }} catch (e) {{ return e.message; }}"
  ))
  .await;
  assert!(
    val(&o).as_str().unwrap_or_default().contains("used Response"),
    "cloning a consumed Response is a TypeError: {}",
    val(&o)
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn response_body_pipes_through_a_transform_stream() {
  let (url, _h) = spawn_chunked();
  let o = run(&format!(
    "const r = await fetch('{url}/c'); const dec = new TextDecoder();\
     const t = new TransformStream({{ transform(chunk, c) {{ c.enqueue(dec.decode(chunk).toLowerCase()); }} }});\
     const rd = r.body.pipeThrough(t).getReader();\
     const out = []; for (;;) {{ const v = await rd.read(); if (v.done) break; out.push(v.value); }}\
     return out.join('');"
  ))
  .await;
  assert_eq!(
    val(&o),
    &serde_json::json!("aaabbb"),
    "a live network body pipes through a TransformStream"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn abort_reason_is_a_dom_exception() {
  let o = run(
    "const ac = new AbortController(); ac.abort();\
     return { name: ac.signal.reason.name, isDom: ac.signal.reason instanceof DOMException, \
       aborted: ac.signal.aborted };",
  )
  .await;
  let v = val(&o);
  assert_eq!(
    v["name"],
    serde_json::json!("AbortError"),
    "the default abort reason is a DOMException named AbortError"
  );
  assert_eq!(v["isDom"], serde_json::json!(true));
  assert_eq!(v["aborted"], serde_json::json!(true));
}

#[tokio::test(flavor = "multi_thread")]
async fn response_body_mixin_blob_bytes_and_array_buffer() {
  let (url, _h) = spawn_echo();
  let o = run(&format!(
    "const r1 = await fetch('{url}/x');\
     const b = await r1.blob();\
     const r2 = await fetch('{url}/x');\
     const u8 = await r2.bytes();\
     const r3 = await fetch('{url}/x');\
     const ab = await r3.arrayBuffer();\
     return {{ blobType: b.type, blobText: await b.text(), \
       isU8: u8 instanceof Uint8Array, u8Len: u8.length, \
       isAb: ab instanceof ArrayBuffer, abLen: ab.byteLength }};"
  ))
  .await;
  let v = val(&o);
  assert_eq!(
    v["blobType"],
    serde_json::json!("application/json"),
    "blob() types the Blob from the response content-type"
  );
  assert!(
    v["blobText"]
      .as_str()
      .unwrap_or_default()
      .contains("\"method\":\"GET\""),
    "blob() carries the body bytes: {v:?}"
  );
  assert_eq!(v["isU8"], serde_json::json!(true), "bytes() resolves a Uint8Array");
  assert_eq!(v["isAb"], serde_json::json!(true));
  assert_eq!(v["u8Len"], v["abLen"], "both readers see the same byte count");
}

#[tokio::test(flavor = "multi_thread")]
async fn request_gains_the_full_body_mixin() {
  let o = run(
    "const r = new Request('http://x.test/', { method: 'POST', body: 'hello' });\
     const ab = await r.arrayBuffer();\
     const r2 = new Request('http://x.test/', { method: 'POST', body: 'hello' });\
     const u8 = await r2.bytes();\
     const r3 = new Request('http://x.test/', { method: 'POST', body: 'hello' });\
     const b = await r3.blob();\
     return { abLen: ab.byteLength, isU8: u8 instanceof Uint8Array, \
       blobText: await b.text(), blobType: b.type, used: r3.bodyUsed };",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["abLen"], serde_json::json!(5));
  assert_eq!(v["isU8"], serde_json::json!(true));
  assert_eq!(v["blobText"], serde_json::json!("hello"));
  assert_eq!(
    v["blobType"],
    // Lowercased by the Blob constructor (WHATWG Blob: "convert every
    // character to ASCII lowercase"), which the previous hand-written
    // Blob did not do.
    serde_json::json!("text/plain;charset=utf-8"),
    "a string body sets the content-type the Blob inherits"
  );
  assert_eq!(v["used"], serde_json::json!(true), "a mixin read marks the body used");
}

#[tokio::test(flavor = "multi_thread")]
async fn form_data_mixin_round_trips_multipart_and_urlencoded() {
  let o = run(
    "const fd = new FormData(); fd.append('field', 'value');\
     fd.append('file', new Blob(['filedata'], { type: 'text/plain' }), 'a.txt');\
     const rq = new Request('http://x.test/', { method: 'POST', body: fd });\
     const back = await rq.formData();\
     const f = back.get('file');\
     const enc = new Response('a=1&b=two+words&b=%40x', \
       { headers: { 'content-type': 'application/x-www-form-urlencoded' } });\
     const encBack = await enc.formData();\
     return { field: back.get('field'), fileName: f.name, fileType: f.type, \
       fileText: await f.text(), isFile: f instanceof File, \
       a: encBack.get('a'), b: encBack.getAll('b') };",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["field"], serde_json::json!("value"));
  assert_eq!(v["fileName"], serde_json::json!("a.txt"), "the filename survives");
  assert_eq!(v["fileType"], serde_json::json!("text/plain"));
  assert_eq!(v["fileText"], serde_json::json!("filedata"));
  assert_eq!(v["isFile"], serde_json::json!(true), "a file part reads back as a File");
  assert_eq!(v["a"], serde_json::json!("1"));
  assert_eq!(
    v["b"],
    serde_json::json!(["two words", "@x"]),
    "urlencoded decodes + and %XX and keeps repeats"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn form_data_mixin_rejects_an_unsupported_content_type() {
  let o = run(
    "const r = new Response('{}', { headers: { 'content-type': 'application/json' } });\
     try { await r.formData(); return { threw: false }; } \
     catch (e) { return { threw: true, message: String(e.message ?? e) }; }",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["threw"], serde_json::json!(true));
  assert!(
    v["message"].as_str().unwrap_or_default().contains("FormData"),
    "message names the failure: {v:?}"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn request_body_is_a_readable_stream_and_clone_tees_it() {
  let o = run(
    "const r = new Request('http://x.test/', { method: 'POST', body: 'streamed' });\
     const isStream = r.body instanceof ReadableStream;\
     const same = r.body === r.body;\
     const copy = r.clone();\
     const dec = new TextDecoder();\
     const rd = r.body.getReader(); const first = await rd.read();\
     return { isStream, same, mine: dec.decode(first.value), theirs: await copy.text() };",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["isStream"], serde_json::json!(true));
  assert_eq!(
    v["same"],
    serde_json::json!(true),
    "the same stream object every access"
  );
  assert_eq!(v["mine"], serde_json::json!("streamed"));
  assert_eq!(
    v["theirs"],
    serde_json::json!("streamed"),
    "clone() tees, so a vended stream does not empty the clone"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn request_spec_attributes_round_trip_through_init_and_clone() {
  let o = run(
    "const ac = new AbortController();\
     const r = new Request('http://x.test/p', { method: 'POST', body: 'b', cache: 'no-store', \
       mode: 'same-origin', referrer: 'http://ref.test/', referrerPolicy: 'no-referrer', \
       integrity: 'sha256-abc', keepalive: true, signal: ac.signal });\
     const c = r.clone();\
     return { cache: r.cache, mode: r.mode, referrer: r.referrer, \
       referrerPolicy: r.referrerPolicy, integrity: r.integrity, keepalive: r.keepalive, \
       destination: r.destination, signalIsSame: r.signal === ac.signal, \
       clonedCache: c.cache, clonedIntegrity: c.integrity, clonedKeepalive: c.keepalive };",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["cache"], serde_json::json!("no-store"));
  assert_eq!(v["mode"], serde_json::json!("same-origin"));
  assert_eq!(v["referrer"], serde_json::json!("http://ref.test/"));
  assert_eq!(v["referrerPolicy"], serde_json::json!("no-referrer"));
  assert_eq!(v["integrity"], serde_json::json!("sha256-abc"));
  assert_eq!(v["keepalive"], serde_json::json!(true));
  assert_eq!(
    v["destination"],
    serde_json::json!(""),
    "a script-built Request has an empty destination"
  );
  assert_eq!(
    v["signalIsSame"],
    serde_json::json!(true),
    "the signal getter returns the caller's own AbortSignal"
  );
  assert_eq!(v["clonedCache"], serde_json::json!("no-store"));
  assert_eq!(v["clonedIntegrity"], serde_json::json!("sha256-abc"));
  assert_eq!(v["clonedKeepalive"], serde_json::json!(true));
}

#[tokio::test(flavor = "multi_thread")]
async fn request_defaults_match_the_spec() {
  let o = run(
    "const r = new Request('http://x.test/');\
     return { cache: r.cache, mode: r.mode, referrer: r.referrer, integrity: r.integrity, \
       keepalive: r.keepalive, hasSignal: r.signal instanceof AbortSignal, aborted: r.signal.aborted };",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["cache"], serde_json::json!("default"));
  assert_eq!(v["mode"], serde_json::json!("cors"));
  assert_eq!(v["referrer"], serde_json::json!("about:client"));
  assert_eq!(v["integrity"], serde_json::json!(""));
  assert_eq!(v["keepalive"], serde_json::json!(false));
  assert_eq!(
    v["hasSignal"],
    serde_json::json!(true),
    "spec always exposes a signal, even with none passed"
  );
  assert_eq!(v["aborted"], serde_json::json!(false));
}

#[tokio::test(flavor = "multi_thread")]
async fn fetching_a_request_still_sends_a_body_after_touching_dot_body() {
  let (url, _h) = spawn_echo();
  let o = run(&format!(
    "const r = new Request('{url}/p', {{ method: 'POST', body: 'payload' }});\
     const isStream = r.body instanceof ReadableStream;\
     const echoed = await (await fetch(r)).json();\
     return {{ isStream, body: echoed.body, method: echoed.method }};"
  ))
  .await;
  let v = val(&o);
  assert_eq!(v["isStream"], serde_json::json!(true));
  assert_eq!(
    v["body"],
    serde_json::json!("payload"),
    "merely vending .body must not empty the request"
  );
  assert_eq!(v["method"], serde_json::json!("POST"));
}

#[tokio::test(flavor = "multi_thread")]
async fn fetching_a_request_with_a_read_body_is_a_type_error() {
  let (url, _h) = spawn_echo();
  let o = run(&format!(
    "const r = new Request('{url}/p', {{ method: 'POST', body: 'payload' }});\
     await r.text();\
     try {{ await fetch(r); return {{ threw: false }}; }}\
     catch (e) {{ return {{ threw: true, message: String(e.message ?? e) }}; }}"
  ))
  .await;
  let v = val(&o);
  assert_eq!(v["threw"], serde_json::json!(true));
  assert!(
    v["message"].as_str().unwrap_or_default().contains("already been read"),
    "message explains the refusal: {v:?}"
  );
}
