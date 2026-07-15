//! Integration tests for the QuickJS `page` bindings added for
//! Playwright parity: the event emitter (`on` / `once` / `off` /
//! `removeAllListeners`), `waitForTimeout`, `addScriptTag` /
//! `addStyleTag`, `setExtraHTTPHeaders`, `isEditable`, `viewportSize`,
//! and the `context()` accessor.
//!
//! Each test observes a page-visible / protocol-visible effect (Rule 9)
//! and runs on every backend the harness drives. Event listeners fire
//! cross-task (a backend tokio task re-enters the script VM), so the
//! event tests yield via real page round-trips (`page.title()`) to let
//! the dispatch task deliver — a synchronous `while` busy-loop would
//! starve it.

#![allow(
  clippy::too_many_lines,
  clippy::doc_markdown,
  clippy::uninlined_format_args,
  clippy::unwrap_used,
  clippy::expect_used,
  clippy::needless_pass_by_value
)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;

use serde_json::json;

use super::client::McpClient;

fn urlencoding(s: &str) -> String {
  s.replace(' ', "%20").replace('#', "%23").replace('"', "%22")
}

const H1_HTML: &str = "<!doctype html><html><body><h1>page-api</h1></body></html>";

// Playwright: `page.$eval(sel, fn, arg?)` runs the function on the FIRST
// match; `page.$$eval(sel, fn, arg?)` on ALL matches as an array. Each
// assertion observes a DOM-derived value that only appears when the
// function actually ran against the resolved element(s).
fn test_script_eval_on_selector(c: &mut McpClient) {
  c.nav("<ul><li data-v='a'>one</li><li data-v='b'>two</li></ul>");
  let first = c.script_value("return await page.$eval('li', el => el.getAttribute('data-v'));");
  assert_eq!(first, json!("a"), "$eval ran fn on the first match: {first}");

  let with_arg = c.script_value("return await page.$eval('li', (el, s) => el.textContent + s, '!');");
  assert_eq!(with_arg, json!("one!"), "$eval forwards the arg to the fn: {with_arg}");

  let all = c.script_value("return await page.$$eval('li', els => els.map(e => e.textContent));");
  assert_eq!(all, json!(["one", "two"]), "$$eval ran fn over all matches: {all}");

  // $eval throws when nothing matches (Playwright's evalOnSelector).
  let miss = c.script("return await page.$eval('.nope', el => el.tagName);");
  assert_ne!(
    miss["status"].as_str(),
    Some("ok"),
    "$eval rejects when the selector matches nothing: {miss}"
  );
}

// Playwright: `page.pause()`. ferridriver has no Inspector UI, so it
// rejects with a typed Unsupported error rather than a silent no-op.
fn test_script_page_pause_unsupported(c: &mut McpClient) {
  c.nav("<h1>x</h1>");
  let r = c.script("await page.pause(); return 'unreached';");
  assert_ne!(
    r["status"].as_str(),
    Some("ok"),
    "page.pause() must reject (Unsupported), not resolve: {r}"
  );
  let msg = r["error"]["message"].as_str().unwrap_or("").to_lowercase();
  assert!(
    msg.contains("pause") || msg.contains("inspector") || msg.contains("unsupported"),
    "pause() error explains the missing Inspector: {r}"
  );
}

pub fn register(set: &mut crate::TestSet<'_>) {
  set.run("page_api::test_script_eval_on_selector", test_script_eval_on_selector);
  set.run(
    "page_api::test_script_page_pause_unsupported",
    test_script_page_pause_unsupported,
  );
  set.run("page_api::test_page_on_receives_console", test_page_on_receives_console);
  set.run("page_api::test_page_off_stops_delivery", test_page_off_stops_delivery);
  set.run("page_api::test_page_once_fires_once", test_page_once_fires_once);
  set.run(
    "page_api::test_page_remove_all_listeners",
    test_page_remove_all_listeners,
  );
  set.run(
    "page_api::test_page_on_pageerror_is_error",
    test_page_on_pageerror_is_error,
  );
  set.run("page_api::test_page_wait_for_timeout", test_page_wait_for_timeout);
  set.run("page_api::test_page_bring_to_front", test_page_bring_to_front);
  set.run("page_api::test_page_add_script_tag", test_page_add_script_tag);
  set.run("page_api::test_page_add_style_tag", test_page_add_style_tag);
  set.run("page_api::test_page_is_editable", test_page_is_editable);
  set.run("page_api::test_page_viewport_size", test_page_viewport_size);
  set.run("page_api::test_page_context_accessor", test_page_context_accessor);
  set.run(
    "page_api::test_page_set_extra_http_headers",
    test_page_set_extra_http_headers,
  );
  set.run("page_api::test_page_off_by_function", test_page_off_by_function);
  set.run("page_api::test_wait_for_event_predicate", test_wait_for_event_predicate);
  set.run("page_api::test_page_console_messages", test_page_console_messages);
  set.run("page_api::test_page_page_errors", test_page_page_errors);
  set.run("page_api::test_page_request_gc", test_page_request_gc);
  set.run("page_api::test_locator_describe", test_locator_describe);
  set.run("page_api::test_timeout_error_name", test_timeout_error_name);
}

/// Timeouts surface as a real JS `Error` with `name === 'TimeoutError'`
/// and the core message (Playwright shape) — not the mangled
/// "Error converting from js ..." TypeError the ctx-free conversion
/// produced.
fn test_timeout_error_name(c: &mut McpClient) {
  c.nav_url(&format!("data:text/html,{}", urlencoding(H1_HTML)));
  let v = c.script_value(
    r"
    try {
      await page.waitForSelector('#does-not-exist', { timeout: 250 });
      return { threw: false };
    } catch (e) {
      return {
        threw: true,
        name: e.name,
        isError: e instanceof Error,
        message: String(e.message),
      };
    }
  ",
  );
  assert_eq!(v["threw"].as_bool(), Some(true), "waitForSelector should time out: {v}");
  assert_eq!(
    v["name"].as_str(),
    Some("TimeoutError"),
    "error name must be TimeoutError: {v}"
  );
  assert_eq!(v["isError"].as_bool(), Some(true), "must be instanceof Error: {v}");
  assert!(
    v["message"]
      .as_str()
      .unwrap_or("")
      .starts_with("Timeout 250ms exceeded"),
    "message should be the core timeout message, not a conversion wrapper: {v}"
  );
}

/// `page.on('console', cb)` delivers a live `ConsoleMessage` instance
/// (`type()` / `text()` methods — same object `waitForEvent('console')`
/// resolves to, mirroring Playwright and the NAPI binding).
fn test_page_on_receives_console(c: &mut McpClient) {
  c.nav_url(&format!("data:text/html,{}", urlencoding(H1_HTML)));
  let v = c.script_value(
    r"
    const got = [];
    page.on('console', (msg) => got.push({ type: msg.type(), text: msg.text() }));
    await page.evaluate(() => console.log('on-hello', 7));
    const deadline = Date.now() + 4000;
    while (got.length === 0 && Date.now() < deadline) {
      await page.title();
    }
    return { count: got.length, first: got[0] || null };
  ",
  );
  assert!(
    v["count"].as_u64().unwrap_or(0) >= 1,
    "page.on('console') should deliver: {v}"
  );
  let first = &v["first"];
  assert_eq!(first["type"].as_str(), Some("log"), "console type should be 'log': {v}");
  assert!(
    first["text"].as_str().unwrap_or("").contains("on-hello"),
    "console text should carry the payload: {v}"
  );
}

/// `page.off(id)` removes a listener: the event after `off` is not
/// delivered, while the one before it was.
fn test_page_off_stops_delivery(c: &mut McpClient) {
  c.nav_url(&format!("data:text/html,{}", urlencoding(H1_HTML)));
  let v = c.script_value(
    r"
    const got = [];
    const id = page.on('console', (msg) => got.push(msg.text()));
    await page.evaluate(() => console.log('before-off'));
    let deadline = Date.now() + 4000;
    while (got.length === 0 && Date.now() < deadline) { await page.title(); }
    const afterFirst = got.length;
    page.off(id);
    await page.evaluate(() => console.log('after-off'));
    // Give any (incorrect) delivery a chance to land.
    for (let i = 0; i < 5; i++) { await page.title(); }
    return { afterFirst, total: got.length, texts: got };
  ",
  );
  assert!(
    v["afterFirst"].as_u64().unwrap_or(0) >= 1,
    "listener should fire before off: {v}"
  );
  assert_eq!(
    v["total"].as_u64(),
    v["afterFirst"].as_u64(),
    "no console should be delivered after page.off(id): {v}"
  );
}

/// `page.once(event, cb)` fires at most once even when the event
/// recurs (core auto-removes after the first emit).
fn test_page_once_fires_once(c: &mut McpClient) {
  c.nav_url(&format!("data:text/html,{}", urlencoding(H1_HTML)));
  let v = c.script_value(
    r"
    const got = [];
    page.once('console', (msg) => got.push(msg.text()));
    await page.evaluate(() => console.log('once-1'));
    let deadline = Date.now() + 4000;
    while (got.length === 0 && Date.now() < deadline) { await page.title(); }
    await page.evaluate(() => console.log('once-2'));
    for (let i = 0; i < 5; i++) { await page.title(); }
    return { count: got.length, texts: got };
  ",
  );
  assert_eq!(
    v["count"].as_u64(),
    Some(1),
    "page.once should deliver exactly once: {v}"
  );
}

/// `page.removeAllListeners()` drops every registered listener.
fn test_page_remove_all_listeners(c: &mut McpClient) {
  c.nav_url(&format!("data:text/html,{}", urlencoding(H1_HTML)));
  let v = c.script_value(
    r"
    const got = [];
    page.on('console', (msg) => got.push(msg.text()));
    await page.evaluate(() => console.log('pre-clear'));
    let deadline = Date.now() + 4000;
    while (got.length === 0 && Date.now() < deadline) { await page.title(); }
    const before = got.length;
    page.removeAllListeners();
    await page.evaluate(() => console.log('post-clear'));
    for (let i = 0; i < 5; i++) { await page.title(); }
    return { before, total: got.length };
  ",
  );
  assert!(
    v["before"].as_u64().unwrap_or(0) >= 1,
    "listener should fire before clear: {v}"
  );
  assert_eq!(
    v["total"].as_u64(),
    v["before"].as_u64(),
    "removeAllListeners should stop further delivery: {v}"
  );
}

/// `page.on('pageerror', cb)` hands the listener a native JS `Error`
/// (`instanceof Error`), matching Playwright and the NAPI binding.
fn test_page_on_pageerror_is_error(c: &mut McpClient) {
  c.nav_url(&format!("data:text/html,{}", urlencoding(H1_HTML)));
  let v = c.script_value(
    r"
    const got = [];
    page.on('pageerror', (err) => got.push({
      isError: err instanceof Error,
      message: err.message,
      name: err.name,
    }));
    await page.evaluate(() => {
      setTimeout(() => { throw new Error('listener-boom'); }, 5);
    });
    const deadline = Date.now() + 5000;
    while (Date.now() < deadline) {
      if (got.some(e => (e.message || '').indexOf('listener-boom') !== -1)) break;
      await page.title();
    }
    return got.find(e => (e.message || '').indexOf('listener-boom') !== -1) || null;
  ",
  );
  assert!(!v.is_null(), "page.on('pageerror') should deliver the error: {v}");
  assert_eq!(
    v["isError"].as_bool(),
    Some(true),
    "pageerror arg must be instanceof Error: {v}"
  );
  assert_eq!(v["name"].as_str(), Some("Error"), "pageerror name: {v}");
}

/// `page.waitForTimeout(ms)` sleeps at least `ms` (core async timer; the
/// QuickJS engine has no `setTimeout`).
fn test_page_wait_for_timeout(c: &mut McpClient) {
  c.nav_url(&format!("data:text/html,{}", urlencoding(H1_HTML)));
  let v = c.script_value(
    r"
    const t0 = Date.now();
    await page.waitForTimeout(150);
    return { dt: Date.now() - t0 };
  ",
  );
  let dt = v["dt"].as_f64().unwrap_or(0.0);
  assert!(
    dt >= 120.0,
    "waitForTimeout(150) should sleep ~150ms, observed {dt}ms: {v}"
  );
}

/// `page.bringToFront()` activates the page — `document.visibilityState`
/// is `'visible'` afterwards.
fn test_page_bring_to_front(c: &mut McpClient) {
  c.nav_url(&format!("data:text/html,{}", urlencoding(H1_HTML)));
  let v = c.script_value(
    r"
    await page.bringToFront();
    return { state: await page.evaluate(() => document.visibilityState) };
  ",
  );
  assert_eq!(
    v["state"].as_str(),
    Some("visible"),
    "bringToFront should leave the page visible: {v}"
  );
}

/// `page.addScriptTag({ content })` injects and runs a `<script>` — the
/// global it sets is then readable from the page.
fn test_page_add_script_tag(c: &mut McpClient) {
  c.nav_url(&format!("data:text/html,{}", urlencoding(H1_HTML)));
  let v = c.script_value(
    r"
    await page.addScriptTag({ content: 'window.__addedByTag = \'script-ok\';' });
    return { v: await page.evaluate(() => window.__addedByTag) };
  ",
  );
  assert_eq!(
    v["v"].as_str(),
    Some("script-ok"),
    "addScriptTag content should execute: {v}"
  );
}

/// `page.addStyleTag({ content })` injects CSS — the computed style of a
/// matching element reflects it.
fn test_page_add_style_tag(c: &mut McpClient) {
  c.nav_url(&format!("data:text/html,{}", urlencoding(H1_HTML)));
  let v = c.script_value(
    r"
    await page.addStyleTag({ content: 'h1 { color: rgb(1, 2, 3); }' });
    return { color: await page.evaluate(() => getComputedStyle(document.querySelector('h1')).color) };
  ",
  );
  assert_eq!(
    v["color"].as_str(),
    Some("rgb(1, 2, 3)"),
    "addStyleTag CSS should apply to the element: {v}"
  );
}

/// `page.isEditable(selector)` — true for a plain input, false for a
/// disabled one.
fn test_page_is_editable(c: &mut McpClient) {
  let html = "<!doctype html><html><body><input id=a><input id=b disabled></body></html>";
  c.nav_url(&format!("data:text/html,{}", urlencoding(html)));
  let v = c.script_value(
    r"
    return { a: await page.isEditable('#a'), b: await page.isEditable('#b') };
  ",
  );
  assert_eq!(v["a"].as_bool(), Some(true), "plain input should be editable: {v}");
  assert_eq!(
    v["b"].as_bool(),
    Some(false),
    "disabled input should not be editable: {v}"
  );
}

/// `page.setViewportSize(...)` then `page.viewportSize()` round-trips a
/// `{ width, height }` object.
fn test_page_viewport_size(c: &mut McpClient) {
  c.nav_url(&format!("data:text/html,{}", urlencoding(H1_HTML)));
  let v = c.script_value(
    r"
    await page.setViewportSize({ width: 820, height: 610 });
    const vs = await page.viewportSize();
    return vs;
  ",
  );
  assert_eq!(
    v["width"].as_i64(),
    Some(820),
    "viewportSize width should match what was set: {v}"
  );
  assert_eq!(
    v["height"].as_i64(),
    Some(610),
    "viewportSize height should match what was set: {v}"
  );
}

/// `page.context()` returns the owning `BrowserContext` — a real binding
/// with the context surface on it.
fn test_page_context_accessor(c: &mut McpClient) {
  c.nav_url(&format!("data:text/html,{}", urlencoding(H1_HTML)));
  let v = c.script_value(
    r"
    const ctx = page.context();
    return {
      notNull: ctx !== null && ctx !== undefined,
      hasNewPage: typeof ctx.newPage === 'function',
    };
  ",
  );
  assert_eq!(
    v["notNull"].as_bool(),
    Some(true),
    "page.context() should be non-null: {v}"
  );
  assert_eq!(
    v["hasNewPage"].as_bool(),
    Some(true),
    "page.context() should expose the BrowserContext surface: {v}"
  );
}

/// `page.setExtraHTTPHeaders(headers)` attaches the header to every
/// subsequent request — observed by a one-shot echo server that reflects
/// the inbound header.
fn test_page_set_extra_http_headers(c: &mut McpClient) {
  let listener = TcpListener::bind("127.0.0.1:0").expect("bind echo server");
  let port = listener.local_addr().expect("addr").port();
  let (tx, rx) = mpsc::channel::<String>();
  thread::spawn(move || {
    if let Ok((mut stream, _)) = listener.accept() {
      let mut reader = BufReader::new(stream.try_clone().expect("clone"));
      let mut header_value = String::new();
      let mut content_length = 0usize;
      loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
          break;
        }
        if line == "\r\n" || line == "\n" {
          break;
        }
        if let Some(rest) = line.to_ascii_lowercase().strip_prefix("x-page-extra:") {
          header_value = rest.trim().to_string();
        }
        if let Some(rest) = line.to_ascii_lowercase().strip_prefix("content-length:") {
          content_length = rest.trim().parse().unwrap_or(0);
        }
      }
      if content_length > 0 {
        let mut buf = vec![0u8; content_length];
        let _ = reader.read_exact(&mut buf);
      }
      let body = format!("HEADER:{header_value}");
      let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
      );
      let _ = stream.write_all(resp.as_bytes());
      let _ = tx.send(header_value);
    }
  });

  let url = format!("http://127.0.0.1:{port}/page-extra");
  c.script_value_with_args(
    r"
    const [url] = args;
    await page.setExtraHTTPHeaders({ 'x-page-extra': 'present' });
    await page.goto(url);
    return { ok: true };
  ",
    json!([url]),
  );
  let server_seen = rx.recv_timeout(std::time::Duration::from_secs(8)).unwrap_or_default();
  assert_eq!(
    server_seen, "present",
    "echo server should observe the page.setExtraHTTPHeaders header on the request"
  );
}

/// `page.off(event, listener)` removes the registration matching the
/// given function by `===` identity (Playwright's `off` shape) while a
/// second listener for the same event keeps firing.
fn test_page_off_by_function(c: &mut McpClient) {
  c.nav_url(&format!("data:text/html,{}", urlencoding(H1_HTML)));
  let v = c.script_value(
    r"
    const a = [];
    const b = [];
    const la = (msg) => a.push(msg.text());
    const lb = (msg) => b.push(msg.text());
    page.on('console', la);
    page.on('console', lb);
    await page.evaluate(() => console.log('both'));
    let deadline = Date.now() + 4000;
    while ((a.length === 0 || b.length === 0) && Date.now() < deadline) { await page.title(); }
    page.off('console', la);
    await page.evaluate(() => console.log('only-b'));
    deadline = Date.now() + 4000;
    while (!b.some(t => t.includes('only-b')) && Date.now() < deadline) { await page.title(); }
    page.off('console', lb);
    return {
      aGotBoth: a.some(t => t.includes('both')),
      aGotOnlyB: a.some(t => t.includes('only-b')),
      bGotBoth: b.some(t => t.includes('both')),
      bGotOnlyB: b.some(t => t.includes('only-b')),
    };
  ",
  );
  assert_eq!(
    v["aGotBoth"].as_bool(),
    Some(true),
    "listener a should fire before off: {v}"
  );
  assert_eq!(
    v["aGotOnlyB"].as_bool(),
    Some(false),
    "off(event, fn) must stop only the matching listener: {v}"
  );
  assert_eq!(
    v["bGotBoth"].as_bool(),
    Some(true),
    "listener b should fire before off: {v}"
  );
  assert_eq!(
    v["bGotOnlyB"].as_bool(),
    Some(true),
    "listener b should keep firing after a's off: {v}"
  );
}

/// `page.waitForEvent(event, { predicate })` skips non-matching events
/// and resolves with the first live object the predicate accepts
/// (Playwright's optionsOrPredicate shape).
fn test_wait_for_event_predicate(c: &mut McpClient) {
  c.nav_url(&format!("data:text/html,{}", urlencoding(H1_HTML)));
  let v = c.script_value(
    r"
    const waiter = page.waitForEvent('console', {
      predicate: (msg) => msg.text().includes('pick-me'),
      timeout: 8000,
    });
    await page.evaluate(() => { console.log('skip-1'); console.log('pick-me'); });
    const msg = await waiter;
    return { text: msg.text(), type: msg.type() };
  ",
  );
  assert!(
    v["text"].as_str().unwrap_or("").contains("pick-me"),
    "predicate should select the matching console message: {v}"
  );
  assert_eq!(v["type"].as_str(), Some("log"), "live ConsoleMessage type(): {v}");
}

/// `page.consoleMessages()` returns the retained history: the default
/// filter only spans messages after the last main-frame navigation,
/// `{ filter: 'all' }` spans page lifetime, and
/// `page.clearConsoleMessages()` drops everything.
fn test_page_console_messages(c: &mut McpClient) {
  c.nav_url(&format!("data:text/html,{}", urlencoding(H1_HTML)));
  let v = c.script_value(
    r"
    await page.evaluate(() => console.log('before-nav-msg'));
    // Reload starts a new since-navigation window.
    await page.reload();
    await page.evaluate(() => console.log('after-nav-msg'));
    const poll = async (pred) => {
      const deadline = Date.now() + 4000;
      let out = [];
      while (Date.now() < deadline) {
        out = await page.consoleMessages();
        if (pred(out)) break;
        await page.title();
      }
      return out;
    };
    const since = await poll(m => m.some(x => x.text().includes('after-nav-msg')));
    const all = await page.consoleMessages({ filter: 'all' });
    const sinceTexts = since.map(m => m.text());
    const allTexts = all.map(m => m.text());
    const types = since.map(m => m.type());
    page.clearConsoleMessages();
    const cleared = await page.consoleMessages({ filter: 'all' });
    return { sinceTexts, allTexts, types, clearedLen: cleared.length };
  ",
  );
  let since: Vec<&str> = v["sinceTexts"]
    .as_array()
    .unwrap()
    .iter()
    .filter_map(|x| x.as_str())
    .collect();
  let all: Vec<&str> = v["allTexts"]
    .as_array()
    .unwrap()
    .iter()
    .filter_map(|x| x.as_str())
    .collect();
  assert!(
    since.iter().any(|t| t.contains("after-nav-msg")),
    "since-navigation window should hold the post-reload message: {v}"
  );
  assert!(
    !since.iter().any(|t| t.contains("before-nav-msg")),
    "since-navigation window must not hold the pre-reload message: {v}"
  );
  assert!(
    all.iter().any(|t| t.contains("before-nav-msg")) && all.iter().any(|t| t.contains("after-nav-msg")),
    "filter:'all' should span both navigations: {v}"
  );
  assert_eq!(
    v["clearedLen"].as_u64(),
    Some(0),
    "clearConsoleMessages should empty the history: {v}"
  );
}

/// `page.pageErrors()` returns retained uncaught exceptions as native
/// `Error`s; `page.clearPageErrors()` drops them.
fn test_page_page_errors(c: &mut McpClient) {
  c.nav_url(&format!("data:text/html,{}", urlencoding(H1_HTML)));
  let v = c.script_value(
    r"
    await page.evaluate(() => {
      setTimeout(() => { throw new Error('retained-boom'); }, 5);
    });
    const deadline = Date.now() + 5000;
    let errs = [];
    while (Date.now() < deadline) {
      errs = await page.pageErrors();
      if (errs.some(e => (e.message || '').includes('retained-boom'))) break;
      await page.title();
    }
    const hit = errs.find(e => (e.message || '').includes('retained-boom'));
    page.clearPageErrors();
    const cleared = await page.pageErrors({ filter: 'all' });
    return {
      found: !!hit,
      isError: hit instanceof Error,
      name: hit ? hit.name : null,
      clearedLen: cleared.length,
    };
  ",
  );
  assert_eq!(
    v["found"].as_bool(),
    Some(true),
    "pageErrors should retain the uncaught exception: {v}"
  );
  assert_eq!(
    v["isError"].as_bool(),
    Some(true),
    "pageErrors entries must be instanceof Error: {v}"
  );
  assert_eq!(v["name"].as_str(), Some("Error"), "error name should round-trip: {v}");
  assert_eq!(
    v["clearedLen"].as_u64(),
    Some(0),
    "clearPageErrors should empty the history: {v}"
  );
}

/// `page.requestGC()` collects unreachable objects — observed via a
/// `WeakRef` whose referent was dropped (Playwright's own
/// `page-request-gc.spec.ts` pattern). On BiDi/Firefox the call needs a
/// `TestUtils.gc()`-exposing build; absent that it must surface the
/// typed Unsupported error rather than silently succeeding.
fn test_page_request_gc(c: &mut McpClient) {
  c.nav_url(&format!("data:text/html,{}", urlencoding(H1_HTML)));
  let v = c.script_value(
    r"
    await page.evaluate(() => {
      globalThis.objectToDestroy = { hello: 'world' };
      globalThis.weakRef = new WeakRef(globalThis.objectToDestroy);
    });
    try {
      await page.requestGC();
    } catch (e) {
      return { unsupported: true, message: String(e && e.message || e) };
    }
    const live = await page.evaluate(() => globalThis.weakRef.deref() ? 'live' : 'collected');
    await page.evaluate(() => { globalThis.objectToDestroy = null; });
    let after = 'live';
    for (let i = 0; i < 10 && after === 'live'; i++) {
      await page.requestGC();
      after = await page.evaluate(() => globalThis.weakRef.deref() ? 'live' : 'collected');
    }
    return { unsupported: false, live, after };
  ",
  );
  if v["unsupported"].as_bool() == Some(true) {
    assert_eq!(
      c.backend, "bidi",
      "requestGC may only be unsupported on bidi (TestUtils.gc), got it on {}: {v}",
      c.backend
    );
    assert!(
      v["message"].as_str().unwrap_or("").contains("requestGC"),
      "unsupported error should name requestGC: {v}"
    );
    return;
  }
  assert_eq!(
    v["live"].as_str(),
    Some("live"),
    "reachable object must survive GC: {v}"
  );
  assert_eq!(
    v["after"].as_str(),
    Some("collected"),
    "unreachable object should be collected: {v}"
  );
}

/// `locator.describe(description)` decorates the selector without
/// affecting matching — the described locator still resolves and acts.
fn test_locator_describe(c: &mut McpClient) {
  let html = "<!doctype html><html><body><button id=go onclick='window.__describedClick = 1'>Go</button></body></html>";
  c.nav_url(&format!("data:text/html,{}", urlencoding(html)));
  let v = c.script_value(
    r"
    const plain = page.locator('#go');
    const described = plain.describe('the go button');
    const count = await described.count();
    await described.click();
    const clicked = await page.evaluate(() => window.__describedClick === 1);
    return { count, clicked };
  ",
  );
  assert_eq!(
    v["count"].as_i64(),
    Some(1),
    "described locator should still match exactly one element: {v}"
  );
  assert_eq!(
    v["clicked"].as_bool(),
    Some(true),
    "described locator should still act on the element: {v}"
  );
}
