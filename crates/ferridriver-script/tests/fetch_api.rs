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
