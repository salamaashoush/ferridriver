//! Playwright 1.59-1.61 gap-fill coverage through QuickJS `run_script`, on
//! every backend (Rule 9). Each test asserts a page- or protocol-visible
//! effect that only holds when the feature is wired end-to-end, not merely
//! that the call didn't throw.
//!
//! - `webError.location()` (1.60): source location of an unhandled error.
//! - `request.existingResponse()` (1.59): already-received response, no wait.
//! - `page.localStorage` / `page.sessionStorage` WebStorage (1.61).
//! - `apiResponse.serverAddr()` (1.61): resolved peer address.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::needless_pass_by_value)]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::thread;

use super::client::McpClient;

/// Spawn a throwaway localhost HTTP server that serves a minimal HTML
/// page for every request. Returns the bound port. `http://localhost`
/// is a secure, non-opaque origin where `localStorage` is available
/// (unlike `data:` / `about:blank`).
fn spawn_html_server() -> u16 {
  let listener = TcpListener::bind("127.0.0.1:0").expect("bind html server");
  let port = listener.local_addr().expect("addr").port();
  thread::spawn(move || {
    while let Ok((mut stream, _)) = listener.accept() {
      let mut reader = BufReader::new(stream.try_clone().expect("clone"));
      loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
          break;
        }
        if line == "\r\n" || line == "\n" {
          break;
        }
      }
      let body = "<!doctype html><body>web-storage</body>";
      let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
      );
      let _ = stream.write_all(resp.as_bytes());
    }
  });
  port
}

/// `context.waitForEvent('weberror')` yields a `WebError` whose
/// `location()` returns a `{ url, line, column }` shape captured from the
/// error's top stack frame. Before this landed, `location` was undefined.
pub fn test_web_error_location(c: &mut McpClient) {
  c.nav("<body>weberror</body>");
  let v = c.script_value(
    r"
    const [werr] = await Promise.all([
      context.waitForEvent('weberror', 5000),
      page.evaluate(() => { setTimeout(() => { throw new Error('boom-loc'); }, 10); }),
    ]);
    const loc = werr.location();
    return {
      name: werr.error().name,
      message: werr.error().message,
      url: loc.url,
      lineType: typeof loc.line,
      columnType: typeof loc.column,
      urlType: typeof loc.url,
    };
    ",
  );
  assert_eq!(v["message"].as_str(), Some("boom-loc"), "{v}");
  assert_eq!(v["name"].as_str(), Some("Error"), "{v}");
  assert_eq!(
    v["lineType"].as_str(),
    Some("number"),
    "location.line must be numeric: {v}"
  );
  assert_eq!(
    v["columnType"].as_str(),
    Some("number"),
    "location.column must be numeric: {v}"
  );
  assert_eq!(
    v["urlType"].as_str(),
    Some("string"),
    "location.url must be a string: {v}"
  );
}

/// `request.existingResponse()` returns the response already received for
/// a completed navigation, without awaiting, matching `request.response()`.
pub fn test_request_existing_response(c: &mut McpClient) {
  c.nav("<body>existing</body>");
  let v = c.script_value(
    r"
    const resp = await page.goto('data:text/html,<title>existing-response</title>');
    const req = resp.request();
    const existing = await req.existingResponse();
    const viaWait = await req.response();
    return {
      hasExisting: existing != null,
      matchesUrl: existing != null && existing.url() === resp.url(),
      matchesStatus: existing != null && viaWait != null && existing.status() === viaWait.status(),
    };
    ",
  );
  assert_eq!(
    v["hasExisting"].as_bool(),
    Some(true),
    "existingResponse should be present after navigation: {v}"
  );
  assert_eq!(v["matchesUrl"].as_bool(), Some(true), "{v}");
  assert_eq!(v["matchesStatus"].as_bool(), Some(true), "{v}");
}

/// `page.localStorage` / `page.sessionStorage` round-trip:
/// `setItem` → `getItem` → `items` → `removeItem` → `clear`, observed
/// against the live storage object on a real (`http://localhost`) origin.
pub fn test_web_storage(c: &mut McpClient) {
  let port = spawn_html_server();
  let v = c.script_value_with_args(
    r"
    const [url] = args;
    await page.goto(url);
    await page.localStorage.setItem('token', 'abc');
    await page.localStorage.setItem('user', 'sam');
    await page.sessionStorage.setItem('sid', 'sess-1');

    const token = await page.localStorage.getItem('token');
    const missing = await page.localStorage.getItem('nope');
    const items = await page.localStorage.items();
    // Cross-check against the live DOM API to prove we hit real storage.
    const domToken = await page.evaluate(() => window.localStorage.getItem('token'));
    const domSid = await page.evaluate(() => window.sessionStorage.getItem('sid'));

    await page.localStorage.removeItem('user');
    const afterRemove = (await page.localStorage.items()).map(i => i.name).sort();

    await page.localStorage.clear();
    const afterClear = await page.localStorage.items();

    return {
      token,
      missingIsNull: missing === null || missing === undefined,
      itemNames: items.map(i => i.name).sort(),
      tokenValue: (items.find(i => i.name === 'token') || {}).value,
      domToken,
      domSid,
      afterRemove,
      afterClearLen: afterClear.length,
    };
    ",
    serde_json::json!([format!("http://localhost:{port}/store")]),
  );
  assert_eq!(
    v["token"].as_str(),
    Some("abc"),
    "getItem should read the set value: {v}"
  );
  assert_eq!(v["missingIsNull"].as_bool(), Some(true), "absent key must be null: {v}");
  assert_eq!(
    v["domToken"].as_str(),
    Some("abc"),
    "binding must write real DOM storage: {v}"
  );
  assert_eq!(
    v["domSid"].as_str(),
    Some("sess-1"),
    "sessionStorage must be separate + real: {v}"
  );
  let names: Vec<&str> = v["itemNames"]
    .as_array()
    .unwrap()
    .iter()
    .filter_map(|n| n.as_str())
    .collect();
  assert_eq!(names, vec!["token", "user"], "items() must list both entries: {v}");
  assert_eq!(v["tokenValue"].as_str(), Some("abc"), "{v}");
  let after_remove: Vec<&str> = v["afterRemove"]
    .as_array()
    .unwrap()
    .iter()
    .filter_map(|n| n.as_str())
    .collect();
  assert_eq!(
    after_remove,
    vec!["token"],
    "removeItem must drop only the named key: {v}"
  );
  assert_eq!(v["afterClearLen"].as_i64(), Some(0), "clear must empty the store: {v}");
}

/// `apiResponse.serverAddr()` reports the resolved peer address. Fetch
/// the localhost server and assert the loopback ip + the server's port.
pub fn test_api_response_server_addr(c: &mut McpClient) {
  let port = spawn_html_server();
  let v = c.script_value_with_args(
    r"
    const [url, expectedPort] = args;
    const resp = await request.get(url);
    const addr = resp.serverAddr();
    return {
      status: resp.status(),
      hasAddr: addr != null,
      ip: addr ? addr.ipAddress : null,
      portMatches: addr ? addr.port === expectedPort : false,
    };
    ",
    serde_json::json!([format!("http://127.0.0.1:{port}/api"), port]),
  );
  assert_eq!(v["status"].as_i64(), Some(200), "{v}");
  assert_eq!(v["hasAddr"].as_bool(), Some(true), "serverAddr must be present: {v}");
  assert_eq!(v["ip"].as_str(), Some("127.0.0.1"), "loopback ip expected: {v}");
  assert_eq!(
    v["portMatches"].as_bool(),
    Some(true),
    "serverAddr.port must match server: {v}"
  );
}

pub fn register(set: &mut super::super::TestSet<'_>) {
  set.run(
    "backends_support::pw_159_161::test_web_error_location",
    test_web_error_location,
  );
  set.run(
    "backends_support::pw_159_161::test_request_existing_response",
    test_request_existing_response,
  );
  set.run("backends_support::pw_159_161::test_web_storage", test_web_storage);
  set.run(
    "backends_support::pw_159_161::test_api_response_server_addr",
    test_api_response_server_addr,
  );
}
