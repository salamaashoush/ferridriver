#![allow(
  clippy::too_many_lines,
  clippy::doc_markdown,
  clippy::uninlined_format_args,
  clippy::single_char_pattern,
  clippy::unwrap_used,
  clippy::expect_used
)]
//! Navigation tests, extracted from backends.rs.

use serde_json::json;

use super::client::{McpClient, data_url, extract_text, ok};

pub fn test_navigate(c: &mut McpClient) {
  let r = c.call_tool("navigate", json!({"url": data_url("<h1>Hello</h1>")}));
  ok(&r, "navigate");
  let t = extract_text(&r);
  assert!(t.contains("Hello"), "navigate should show content: {t}");
}

pub fn test_page_list(c: &mut McpClient) {
  c.nav("<body></body>");
  let t = c.tool_text("page", json!({"action": "list"}));
  assert!(t.contains("Page 0"), "list pages: {t}");
}

pub fn test_page_reload(c: &mut McpClient) {
  c.nav("<body>original</body>");
  c.call_tool(
    "evaluate",
    json!({"expression": "document.body.textContent = 'modified'"}),
  );
  let modified = c.tool_text("evaluate", json!({"expression": "document.body.textContent"}));
  assert!(modified.contains("modified"), "should be modified: {modified}");
  c.call_tool("page", json!({"action": "reload"}));
  let after = c.tool_text("evaluate", json!({"expression": "document.body.textContent"}));
  assert!(
    after.contains("original"),
    "reload should restore original content: {after}"
  );
}

pub fn test_page_back_forward(c: &mut McpClient) {
  c.nav("<h1>Page1</h1>");
  c.nav("<h1>Page2</h1>");
  c.call_tool("page", json!({"action": "back"}));
  let t = c.tool_text(
    "evaluate",
    json!({"expression": "document.querySelector('h1')?.textContent || ''"}),
  );
  assert!(t.contains("Page1"), "go_back should return to Page1: {t}");
}

pub fn test_wait_for_url_options(c: &mut McpClient) {
  // `{ timeout }` must bound the URL poll (400ms, not the 30s
  // navigation default); `{ waitUntil }` must accept lifecycle states on
  // the success path.
  c.nav("<h1>here</h1>");
  let v = c.script_value(
    "await page.waitForURL(/^data:/, { waitUntil: 'domcontentloaded' }); \
     await page.waitForURL(/^data:/, { waitUntil: 'commit' }); \
     const t = Date.now(); \
     let failed = false; \
     let msg = ''; \
     try { await page.waitForURL(/never-matches/, { timeout: 400 }); } \
     catch (e) { failed = true; msg = String(e.message); } \
     return { failed, msg, elapsed: Date.now() - t };",
  );
  assert_eq!(v["failed"], json!(true), "non-matching URL must time out: {v}");
  let elapsed = v["elapsed"].as_i64().unwrap_or(0);
  assert!(elapsed < 3000, "timeout 400 should fail fast: {elapsed}ms");
  let msg = v["msg"].as_str().unwrap_or_default();
  assert!(msg.contains("400"), "error should carry the effective timeout: {msg}");
}

pub fn test_wait_for_load_state_options(c: &mut McpClient) {
  // On an already-loaded page the state check passes before the
  // deadline check, so even a 1ms timeout succeeds; `networkidle` with a
  // real budget completes on a quiet data: page.
  c.nav("<h1>loaded</h1>");
  let v = c.script_value(
    "await page.waitForLoadState('load', { timeout: 1 }); \
     await page.waitForLoadState('networkidle', { timeout: 10000 }); \
     return 'ok';",
  );
  assert_eq!(v, json!("ok"));
}

pub fn test_wait_for_url_spa_push_state(c: &mut McpClient) {
  // Same-document navigations (history.pushState / replaceState /
  // location.hash) must update the tracked page URL: app-shell SPAs
  // route without a document swap, and `page.url()` / `waitForURL` /
  // `toHaveURL` have to observe it. Backed by CDP
  // `Page.navigatedWithinDocument`, WebKit ditto, and BiDi
  // `browsingContext.historyUpdated` / `fragmentNavigated`.
  // data: URLs cannot pushState, so serve a real http page
  // (thread-per-connection + Connection: close, per the speculative
  // preconnect lesson).
  let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind spa server");
  let port = listener.local_addr().expect("addr").port();
  std::thread::spawn(move || {
    for stream in listener.incoming() {
      let Ok(mut stream) = stream else { break };
      std::thread::spawn(move || {
        let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone"));
        loop {
          let mut line = String::new();
          if std::io::BufRead::read_line(&mut reader, &mut line).unwrap_or(0) == 0 {
            return;
          }
          if line == "\r\n" || line == "\n" {
            break;
          }
        }
        let body = "<html><body><h1 id='home'>home</h1></body></html>";
        let resp = format!(
          "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
          body.len(),
          body
        );
        let _ = std::io::Write::write_all(&mut stream, resp.as_bytes());
      });
    }
  });

  let url = format!("http://127.0.0.1:{port}/home");
  let v = c.script_value_with_args(
    "const [url] = args; \
     await page.goto(url); \
     await page.evaluate(\"history.pushState({}, '', '/file/1234')\"); \
     await page.waitForURL(/\\/file\\/1234/, { timeout: 5000, waitUntil: 'commit' }); \
     const pushed = page.url(); \
     await page.evaluate(\"history.replaceState({}, '', '/settings')\"); \
     await expect(page).toHaveURL(/settings/, { timeout: 5000 }); \
     await page.evaluate(\"location.hash = 'frag'\"); \
     await page.waitForURL(/#frag/, { timeout: 5000, waitUntil: 'commit' }); \
     return { pushed };",
    serde_json::json!([url]),
  );
  let pushed = v["pushed"].as_str().unwrap_or_default();
  assert!(pushed.contains("/file/1234"), "pushState URL must be tracked: {v}");
}

pub fn register(set: &mut crate::TestSet<'_>) {
  set.run("backends_support::nav::test_navigate", test_navigate);
  set.run("backends_support::nav::test_page_list", test_page_list);
  set.run("backends_support::nav::test_page_reload", test_page_reload);
  set.run("backends_support::nav::test_page_back_forward", test_page_back_forward);
  set.run(
    "backends_support::nav::test_wait_for_url_options",
    test_wait_for_url_options,
  );
  set.run(
    "backends_support::nav::test_wait_for_load_state_options",
    test_wait_for_load_state_options,
  );
  set.run(
    "backends_support::nav::test_wait_for_url_spa_push_state",
    test_wait_for_url_spa_push_state,
  );
}
