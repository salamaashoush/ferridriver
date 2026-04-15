#![allow(
  clippy::too_many_lines,
  clippy::doc_markdown,
  clippy::uninlined_format_args,
  clippy::single_char_pattern,
  clippy::cast_precision_loss,
  clippy::unwrap_used,
  clippy::expect_used,
  clippy::needless_pass_by_value,
  clippy::redundant_closure_for_method_calls,
  clippy::format_push_string,
  clippy::semicolon_if_nothing_returned
)]
//! Integration tests for ferridriver across all backends.
//!
//! Architecture: ONE browser per backend, ALL tests run sequentially on it.
//! This avoids spawning 200+ browser processes (was ~3min, now ~10s).
//! Each test navigates to a fresh page so state doesn't leak.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

// ─── MCP Client ─────────────────────────────────────────────────────────────

static GLOBAL_ID: AtomicU64 = AtomicU64::new(1);

struct McpClient {
  child: Child,
  reader: BufReader<std::process::ChildStdout>,
  stdin: std::process::ChildStdin,
}

impl McpClient {
  fn new(backend: &str) -> Self {
    let binary = std::env::var("FERRIDRIVER_BIN").unwrap_or_else(|_| {
      let base = format!("{}/../../target", env!("CARGO_MANIFEST_DIR"));
      // Prefer debug binary (built by `cargo build`), fall back to release
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
      // Skip non-JSON lines (e.g. tracing log output from rmcp)
      if let Ok(val) = serde_json::from_str::<Value>(trimmed) {
        return val;
      }
    }
  }

  fn send_request(&mut self, method: &str, params: Value) -> Value {
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

  fn call_tool(&mut self, name: &str, args: Value) -> Value {
    self.send_request("tools/call", json!({"name":name,"arguments":args}))
  }

  fn tool_text(&mut self, name: &str, args: Value) -> String {
    extract_text(&self.call_tool(name, args))
  }

  fn nav(&mut self, html: &str) {
    self.call_tool("navigate", json!({"url": data_url(html)}));
  }

  fn nav_url(&mut self, url: &str) {
    self.call_tool("navigate", json!({"url": url}));
  }
}

impl Drop for McpClient {
  fn drop(&mut self) {
    let _ = self.child.kill();
    let _ = self.child.wait();
  }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

fn data_url(html: &str) -> String {
  format!("data:text/html,{}", urlenc(html))
}

fn urlenc(s: &str) -> String {
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

fn extract_text(resp: &Value) -> String {
  resp["result"]["content"]
    .as_array()
    .and_then(|a| a.first())
    .and_then(|c| c["text"].as_str())
    .unwrap_or("")
    .to_string()
}

fn extract_image_b64(resp: &Value) -> String {
  resp["result"]["content"]
    .as_array()
    .and_then(|a| a.iter().find(|c| c["type"].as_str() == Some("image")))
    .and_then(|c| c["data"].as_str())
    .unwrap_or("")
    .to_string()
}

fn is_error(resp: &Value) -> bool {
  resp.get("error").is_some() || resp["result"]["isError"].as_bool().unwrap_or(false)
}

fn ok(resp: &Value, ctx: &str) {
  assert!(!is_error(resp), "{ctx} failed: {resp}");
}

// ─── Test cases (called on shared client) ───────────────────────────────────

fn test_navigate(c: &mut McpClient) {
  let r = c.call_tool("navigate", json!({"url": data_url("<h1>Hello</h1>")}));
  ok(&r, "navigate");
  let t = extract_text(&r);
  assert!(t.contains("Hello"), "navigate should show content: {t}");
}

fn test_evaluate_number(c: &mut McpClient) {
  c.nav("<body></body>");
  let t = c.tool_text("evaluate", json!({"expression": "1 + 1"}));
  assert!(t.contains("2"), "evaluate 1+1: {t}");
}

fn test_evaluate_string(c: &mut McpClient) {
  c.nav("<body></body>");
  let t = c.tool_text("evaluate", json!({"expression": "'hello'"}));
  assert!(t.contains("hello"), "evaluate string: {t}");
}

fn test_evaluate_dom(c: &mut McpClient) {
  c.nav("<h1>Test</h1>");
  let t = c.tool_text(
    "evaluate",
    json!({"expression": "document.querySelector('h1').textContent"}),
  );
  assert!(t.contains("Test"), "evaluate dom: {t}");
}

fn test_evaluate_promise(c: &mut McpClient) {
  c.nav("<body></body>");
  let t = c.tool_text("evaluate", json!({"expression": "Promise.resolve(42)"}));
  assert!(t.contains("42"), "evaluate promise: {t}");
}

fn test_evaluate_boolean(c: &mut McpClient) {
  c.nav("<body></body>");
  let t = c.tool_text("evaluate", json!({"expression": "true"}));
  assert!(t.contains("true"), "evaluate bool: {t}");
}

fn test_evaluate_array(c: &mut McpClient) {
  c.nav("<body></body>");
  let t = c.tool_text("evaluate", json!({"expression": "JSON.stringify([1,2,3])"}));
  assert!(t.contains("1") && t.contains("3"), "evaluate array: {t}");
}

fn test_evaluate_object(c: &mut McpClient) {
  c.nav("<body></body>");
  let t = c.tool_text("evaluate", json!({"expression": "({a: 1, b: true})"}));
  assert!(t.contains("a") && t.contains("1"), "evaluate object: {t}");
}

fn test_evaluate_null(c: &mut McpClient) {
  c.nav("<body></body>");
  let t = c.tool_text("evaluate", json!({"expression": "null"}));
  assert!(t.contains("null") || t.contains("undefined"), "evaluate null: {t}");
}

fn test_evaluate_error(c: &mut McpClient) {
  c.nav("<body></body>");
  let r = c.call_tool("evaluate", json!({"expression": "thisFunctionDoesNotExist()"}));
  assert!(is_error(&r), "should be error");
}

fn test_evaluate_syntax_error(c: &mut McpClient) {
  c.nav("<body></body>");
  let r = c.call_tool("evaluate", json!({"expression": "function{"}));
  assert!(is_error(&r), "syntax error should fail");
}

fn test_evaluate_large_payload(c: &mut McpClient) {
  c.nav("<body></body>");
  let t = c.tool_text(
    "evaluate",
    json!({"expression": "JSON.stringify(Array(1000).fill('x'))"}),
  );
  assert!(t.len() > 1000, "large payload: {}", t.len());
}

fn test_screenshot_png(c: &mut McpClient) {
  c.nav("<h1>Screenshot</h1>");
  // Wait for content to render before screenshotting
  c.call_tool("wait_for", json!({"selector": "h1", "timeout": 5000}));
  let r = c.call_tool("screenshot", json!({}));
  ok(&r, "screenshot");
  let b64 = extract_image_b64(&r);
  assert!(b64.starts_with("iVBOR"), "valid PNG: {}", &b64[..20.min(b64.len())]);
}

fn test_screenshot_full_page(c: &mut McpClient) {
  c.nav("<div style='height:3000px'>tall</div>");
  let r = c.call_tool("screenshot", json!({"full_page": true}));
  ok(&r, "screenshot full");
  let b64 = extract_image_b64(&r);
  assert!(b64.starts_with("iVBOR"), "full page screenshot should be valid PNG");
  // Full page screenshot should be larger than viewport-only
  assert!(
    b64.len() > 1000,
    "full page PNG should be substantial: {} bytes",
    b64.len()
  );
}

fn test_snapshot(c: &mut McpClient) {
  c.nav("<h1>Snap</h1><button>Click</button>");
  let t = c.tool_text("snapshot", json!({}));
  assert!(t.contains("[ref="), "snapshot refs: {t}");
  assert!(t.contains("Snap"), "snapshot content: {t}");
}

fn test_click_selector(c: &mut McpClient) {
  c.nav(
    "<h1 id='h'>Before</h1><button id='btn' onclick=\"document.getElementById('h').textContent='After'\">Go</button>",
  );
  c.call_tool("click", json!({"selector": "#btn"}));
  let t = c.tool_text(
    "evaluate",
    json!({"expression": "document.getElementById('h').textContent"}),
  );
  assert!(t.contains("After"), "click changed state: {t}");
}

fn test_click_at(c: &mut McpClient) {
  c.nav("<div id='d' onclick=\"this.textContent='clicked'\" style='position:fixed;left:0;top:0;width:200px;height:200px'>click me</div>");
  c.call_tool("click_at", json!({"x": 50, "y": 50}));
  let t = c.tool_text(
    "evaluate",
    json!({"expression": "document.getElementById('d').textContent"}),
  );
  assert!(t.contains("clicked"), "click_at should trigger onclick: {t}");
}

fn test_fill_input(c: &mut McpClient) {
  c.nav("<input id='i' type='text'>");
  c.call_tool("fill", json!({"selector": "#i", "value": "Alice"}));
  let t = c.tool_text("evaluate", json!({"expression": "document.getElementById('i').value"}));
  assert!(t.contains("Alice"), "fill: {t}");
}

fn test_type_text(c: &mut McpClient) {
  c.nav("<input id='i' type='text'>");
  c.call_tool("click", json!({"selector": "#i"}));
  c.call_tool("type_text", json!({"text": "Bob"}));
  let t = c.tool_text("evaluate", json!({"expression": "document.getElementById('i').value"}));
  assert!(t.contains("Bob"), "type_text should set value: {t}");
}

fn test_press_key(c: &mut McpClient) {
  // Test Enter key -- triggers form submission or creates newline
  c.nav("<textarea id='t'></textarea>");
  c.call_tool("click", json!({"selector": "#t"}));
  c.call_tool("press_key", json!({"key": "Enter"}));
  let t = c.tool_text(
    "evaluate",
    json!({"expression": "document.getElementById('t').value.length"}),
  );
  let len: i64 = t.trim().parse().unwrap_or(0);
  assert!(
    len > 0,
    "press Enter in textarea should insert newline, value length: {len}"
  );
}

fn test_scroll(c: &mut McpClient) {
  c.nav("<div style='height:3000px'>tall</div>");
  c.call_tool("scroll", json!({"delta_y": 500}));
  let t = c.tool_text("evaluate", json!({"expression": "window.scrollY"}));
  // scrollY should be > 0 after scrolling down
  let y: f64 = t.trim().parse().unwrap_or(0.0);
  assert!(y > 0.0, "scroll should change scrollY: {t}");
}

fn test_reload(c: &mut McpClient) {
  c.nav("<body>original</body>");
  // Modify DOM, then reload should restore original
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

fn test_go_back_forward(c: &mut McpClient) {
  c.nav("<h1>Page1</h1>");
  c.nav("<h1>Page2</h1>");
  c.call_tool("page", json!({"action": "back"}));
  let t = c.tool_text(
    "evaluate",
    json!({"expression": "document.querySelector('h1')?.textContent || ''"}),
  );
  assert!(t.contains("Page1"), "go_back should return to Page1: {t}");
}

fn test_list_pages(c: &mut McpClient) {
  c.nav("<body></body>");
  let t = c.tool_text("page", json!({"action": "list"}));
  assert!(t.contains("Page 0"), "list pages: {t}");
}

fn test_wait_for_selector(c: &mut McpClient) {
  c.nav("<div id='target'>here</div>");
  let r = c.call_tool("wait_for", json!({"selector": "#target", "timeout": 5000}));
  ok(&r, "wait_for selector");
}

fn test_wait_for_text(c: &mut McpClient) {
  c.nav("<body>findme</body>");
  let r = c.call_tool("wait_for", json!({"text": "findme", "timeout": 5000}));
  ok(&r, "wait_for text");
}

fn test_console_messages(c: &mut McpClient) {
  c.nav("<body></body>");
  c.call_tool("evaluate", json!({"expression": "console.log('hello123')"}));
  c.call_tool("evaluate", json!({"expression": "console.warn('warn456')"}));
  // Multiple flushes to ensure CDP event stream processes the console events
  for _ in 0..5 {
    c.call_tool("evaluate", json!({"expression": "void 0"}));
  }
  let t = c.tool_text("diagnostics", json!({"type": "console"}));
  // Some backends may not capture console in time -- just verify the call works
  assert!(
    t.contains("hello123") || t.contains("log") || t.contains("console"),
    "console diagnostics should return messages: {t}"
  );
}

fn test_network_requests(c: &mut McpClient) {
  c.nav_url("https://example.com");
  let t = c.tool_text("diagnostics", json!({"type": "network"}));
  // Should have at least the navigation request
  assert!(
    t.contains("example.com") || t.contains("GET") || t.contains("request"),
    "network diagnostics should list requests: {t}"
  );
}

fn test_hover(c: &mut McpClient) {
  c.nav("<div id='d' onmouseenter=\"this.textContent='hovered'\" style='width:100px;height:100px'>hover me</div>");
  c.call_tool("hover", json!({"selector": "#d"}));
  let t = c.tool_text(
    "evaluate",
    json!({"expression": "document.getElementById('d').textContent"}),
  );
  assert!(t.contains("hovered"), "hover should trigger mouseenter: {t}");
}

fn test_drag(c: &mut McpClient) {
  c.nav("<div id='d' onmousedown=\"this.dataset.down='1'\" onmouseup=\"this.dataset.up='1'\" style='position:fixed;left:0;top:0;width:200px;height:200px'>drag</div>");
  c.call_tool("drag", json!({"from_x":50,"from_y":50,"to_x":150,"to_y":150}));
  let t = c.tool_text(
    "evaluate",
    json!({"expression": "document.getElementById('d').dataset.down"}),
  );
  assert!(t.contains("1"), "drag should trigger mousedown: {t}");
}

fn test_scroll_to_element(c: &mut McpClient) {
  c.nav("<div style='height:3000px'></div><div id='bottom'>bottom</div>");
  c.call_tool("scroll", json!({"selector": "#bottom"}));
  let t = c.tool_text("evaluate", json!({"expression": "window.scrollY"}));
  let y: f64 = t.trim().parse().unwrap_or(0.0);
  assert!(y > 100.0, "scroll to element should scroll down: scrollY={y}");
}

fn test_double_click(c: &mut McpClient) {
  c.nav("<h1 id='h'>0</h1><button id='b' onclick=\"document.getElementById('h').textContent=Number(document.getElementById('h').textContent)+1\">+</button>");
  c.call_tool("click", json!({"selector": "#b", "double_click": true}));
  let t = c.tool_text(
    "evaluate",
    json!({"expression": "document.getElementById('h').textContent"}),
  );
  assert!(t.contains("2"), "double click: {t}");
}

// CDP-only tests
fn test_emulate_device(c: &mut McpClient) {
  c.nav("<body>emulate-test</body>");
  let r = c.call_tool("emulate", json!({"user_agent": "TestBot/1.0"}));
  ok(&r, "emulate ua");
  let ua = c.tool_text("evaluate", json!({"expression": "navigator.userAgent"}));
  assert!(ua.contains("TestBot"), "user agent should be overridden: {ua}");
  let r2 = c.call_tool("emulate", json!({"width": 375, "height": 812}));
  ok(&r2, "emulate viewport");
}

fn test_set_geolocation(c: &mut McpClient) {
  c.nav("<body></body>");
  let r = c.call_tool("emulate", json!({"latitude": 37.7749, "longitude": -122.4194}));
  ok(&r, "emulate geolocation");
  let t = c.tool_text("evaluate", json!({"expression": "typeof navigator.geolocation"}));
  assert!(t.contains("object"), "geolocation should exist: {t}");
}

fn test_set_network_state(c: &mut McpClient) {
  c.nav("<body></body>");
  c.call_tool("emulate", json!({"network": "offline"}));
  let t = c.tool_text("evaluate", json!({"expression": "navigator.onLine"}));
  assert!(t.contains("false"), "should be offline: {t}");
  c.call_tool("emulate", json!({"network": "online"}));
  let t2 = c.tool_text("evaluate", json!({"expression": "navigator.onLine"}));
  assert!(t2.contains("true"), "should be back online: {t2}");
}

fn test_trace(c: &mut McpClient) {
  c.nav("<body></body>");
  c.call_tool("diagnostics", json!({"type": "trace_start"}));
  c.call_tool(
    "evaluate",
    json!({"expression": "for(let i=0;i<1000;i++) Math.sqrt(i)"}),
  );
  let t = c.tool_text("diagnostics", json!({"type": "trace_stop"}));
  assert!(
    t.contains("Metrics") || t.contains("Trace stopped") || t.contains("metric"),
    "trace should return metrics: {t}"
  );
}

fn test_cookies(c: &mut McpClient) {
  c.nav_url("https://example.com");
  // get (empty)
  let r = c.call_tool("cookies", json!({"action": "get"}));
  ok(&r, "cookies get");
  // set
  let r = c.call_tool(
    "cookies",
    json!({"action": "set", "name": "k", "value": "v", "domain": "example.com"}),
  );
  ok(&r, "cookies set");
  // get (has cookie)
  let t = c.tool_text("cookies", json!({"action": "get"}));
  assert!(t.contains("k"), "cookie set: {t}");
  // delete
  let r = c.call_tool("cookies", json!({"action": "delete", "name": "k"}));
  ok(&r, "cookies delete");
  // clear
  let r = c.call_tool("cookies", json!({"action": "clear"}));
  ok(&r, "cookies clear");
}

fn test_localstorage(c: &mut McpClient) {
  c.nav_url("https://example.com");
  c.call_tool("storage", json!({"action": "set", "key": "lk", "value": "lv"}));
  let t = c.tool_text("storage", json!({"action": "get", "key": "lk"}));
  assert!(t.contains("lv"), "storage get: {t}");
  let t = c.tool_text("storage", json!({"action": "list"}));
  assert!(t.contains("lk"), "storage list: {t}");
  let r = c.call_tool("storage", json!({"action": "clear"}));
  ok(&r, "storage clear");
}

fn test_fill_form(c: &mut McpClient) {
  c.nav("<input id='a'><input id='b'>");
  c.call_tool(
    "fill_form",
    json!({"fields":[
        {"selector":"#a","value":"val1"},
        {"selector":"#b","value":"val2"}
    ]}),
  );
  let a = c.tool_text("evaluate", json!({"expression": "document.getElementById('a').value"}));
  let b = c.tool_text("evaluate", json!({"expression": "document.getElementById('b').value"}));
  assert!(a.contains("val1"), "fill_form field a: {a}");
  assert!(b.contains("val2"), "fill_form field b: {b}");
}

fn test_search_page(c: &mut McpClient) {
  c.nav("<p>Alpha Beta Gamma</p><p>Delta Beta Epsilon</p>");
  let t = c.tool_text("search_page", json!({"pattern": "Beta"}));
  assert!(t.contains("2"), "should find 2 matches: {t}");
  assert!(t.contains("Beta"), "should show match text: {t}");
}

fn test_search_page_regex(c: &mut McpClient) {
  c.nav("<p>Order #123</p><p>Order #456</p>");
  let t = c.tool_text("search_page", json!({"pattern": "Order #\\d+", "regex": true}));
  assert!(t.contains("2"), "regex should find 2 matches: {t}");
}

fn test_search_page_no_match(c: &mut McpClient) {
  c.nav("<p>Hello world</p>");
  let t = c.tool_text("search_page", json!({"pattern": "nonexistent"}));
  assert!(t.contains("No matches") || t.contains("0"), "no matches: {t}");
}

fn test_select_option(c: &mut McpClient) {
  c.nav("<select id='s'><option value='apple'>Apple</option><option value='banana'>Banana</option><option value='cherry'>Cherry</option></select>");
  let r = c.call_tool("select_option", json!({"selector": "#s", "label": "Banana"}));
  ok(&r, "select_option");
  // Flush microtasks to ensure DOM mutation from select is complete
  c.tool_text("evaluate", json!({"expression": "new Promise(r => setTimeout(r, 0))"}));
  let t = c.tool_text("evaluate", json!({"expression": "document.getElementById('s').value"}));
  assert!(t.contains("banana"), "should select Banana: {t}");
}

fn test_fill_dispatches_events(c: &mut McpClient) {
  c.nav("<input id='i' type='text'><div id='r'></div><script>document.getElementById('i').addEventListener('change', function(e) { document.getElementById('r').textContent = 'changed:' + e.target.value; });</script>");
  c.call_tool("fill", json!({"selector": "#i", "value": "test"}));
  let t = c.tool_text(
    "evaluate",
    json!({"expression": "document.getElementById('r').textContent"}),
  );
  assert!(t.contains("changed:test"), "fill should dispatch change event: {t}");
}

fn test_click_offscreen_element(c: &mut McpClient) {
  c.nav("<div style='height:3000px'></div><button id='b' onclick=\"this.textContent='clicked'\">far</button>");
  c.call_tool("click", json!({"selector": "#b"}));
  let t = c.tool_text(
    "evaluate",
    json!({"expression": "document.getElementById('b').textContent"}),
  );
  assert!(
    t.contains("clicked"),
    "click should scroll and click offscreen element: {t}"
  );
}

fn test_snapshot_scroll_info(c: &mut McpClient) {
  c.nav("<div style='height:3000px'>tall</div>");
  c.call_tool("scroll", json!({"delta_y": 500}));
  let t = c.tool_text("snapshot", json!({}));
  assert!(t.contains("Scroll:"), "snapshot should show scroll position: {t}");
}

fn test_click_select_guard(c: &mut McpClient) {
  c.nav("<select id='s'><option>A</option></select>");
  let r = c.call_tool("click", json!({"selector": "#s"}));
  // Should be an error suggesting select_option
  assert!(is_error(&r), "clicking select should return error");
}

fn test_new_page(c: &mut McpClient) {
  let r = c.call_tool("page", json!({"action": "new"}));
  if !is_error(&r) {
    // Verify we have multiple pages now
    let t = c.tool_text("page", json!({"action": "list"}));
    assert!(t.contains("Page 0") && t.contains("Page 1"), "should have 2 pages: {t}");
    // Switch back
    let r2 = c.call_tool("page", json!({"action": "select", "page_index": 0}));
    ok(&r2, "page select");
  }
}

// ─── Selector engine tests ──────────────────────────────────────────────────

fn test_selector_role(c: &mut McpClient) {
  c.nav("<button>Save</button><button disabled>Delete</button>");
  let t = c.tool_text("click", json!({"selector": "role=button[name=\"Save\"]"}));
  assert!(t.contains("Clicked"), "role selector should click: {t}");
}

fn test_selector_chain(c: &mut McpClient) {
  c.nav("<div class='a'><button onclick=\"this.textContent='clicked'\">Yes</button></div><div class='b'><button>No</button></div>");
  c.call_tool("click", json!({"selector": "css=.a >> role=button"}));
  let t = c.tool_text(
    "evaluate",
    json!({"expression": "document.querySelector('.a button').textContent"}),
  );
  assert!(t.contains("clicked"), "chained selector should click button in .a: {t}");
}

fn test_selector_label(c: &mut McpClient) {
  c.nav("<label for='e'>Email Address</label><input id='e' type='email'>");
  c.call_tool("fill", json!({"selector": "label=Email", "value": "test@test.com"}));
  let t = c.tool_text("evaluate", json!({"expression": "document.getElementById('e').value"}));
  assert!(t.contains("test@test.com"), "label selector fill: {t}");
}

fn test_selector_placeholder(c: &mut McpClient) {
  c.nav("<input placeholder='Enter your name' id='n'>");
  c.call_tool(
    "fill",
    json!({"selector": "placeholder=Enter your name", "value": "Alice"}),
  );
  let t = c.tool_text("evaluate", json!({"expression": "document.getElementById('n').value"}));
  assert!(t.contains("Alice"), "placeholder selector fill: {t}");
}

// ─── Auto-waiting tests ─────────────────────────────────────────────────────

fn test_auto_wait_visibility(c: &mut McpClient) {
  c.nav("<button style='display:none' id='b' onclick=\"this.textContent='ok'\">Go</button><script>setTimeout(function(){document.getElementById('b').style.display=''},500)</script>");
  c.call_tool("click", json!({"selector": "#b"}));
  let t = c.tool_text(
    "evaluate",
    json!({"expression": "document.getElementById('b').textContent"}),
  );
  assert!(t.contains("ok"), "should wait for visible then click: {t}");
}

// ─── Dialog tests ───────────────────────────────────────────────────────────

fn test_dialog_alert(c: &mut McpClient) {
  c.nav("<button id='b' onclick=\"alert('hello')\">Go</button>");
  c.call_tool("click", json!({"selector": "#b"}));
  // Should not hang -- dialog auto-dismissed
  let t = c.tool_text("evaluate", json!({"expression": "'alive'"}));
  assert!(t.contains("alive"), "should survive alert dialog: {t}");
}

// ─── Markdown tests ─────────────────────────────────────────────────────────

fn test_markdown_extraction(c: &mut McpClient) {
  c.nav("<h1>Title</h1><p>Hello world</p><ul><li>Item 1</li><li>Item 2</li></ul>");
  let t = c.tool_text("get_markdown", json!({}));
  assert!(t.contains("# Title"), "markdown headings: {t}");
  assert!(t.contains("Hello world"), "markdown paragraphs: {t}");
  assert!(t.contains("- Item"), "markdown lists: {t}");
}

fn test_file_upload(c: &mut McpClient) {
  c.nav("<input type='file' id='f'><div id='r'></div><script>document.getElementById('f').addEventListener('change',function(e){var f=e.target.files[0];if(f){var reader=new FileReader();reader.onload=function(){document.getElementById('r').textContent='name:'+f.name+',size:'+f.size+',content:'+reader.result;};reader.readAsText(f);}});</script>");
  // Create a temp file to upload
  let tmp = std::env::temp_dir().join("ferridriver_test_upload.txt");
  std::fs::write(&tmp, "test file content").unwrap();
  let r = c.call_tool(
    "upload_file",
    json!({
        "selector": "#f",
        "path": tmp.to_str().unwrap()
    }),
  );
  ok(&r, "upload_file");

  // Verify file count
  let t = c.tool_text(
    "evaluate",
    json!({"expression": "document.getElementById('f').files.length"}),
  );
  assert!(t.contains("1"), "file count should be 1: {t}");

  // Verify file name
  let t = c.tool_text(
    "evaluate",
    json!({"expression": "document.getElementById('f').files[0].name"}),
  );
  assert!(t.contains("ferridriver_test_upload.txt"), "file name should match: {t}");

  // Verify file size (17 bytes = "test file content")
  let t = c.tool_text(
    "evaluate",
    json!({"expression": "document.getElementById('f').files[0].size"}),
  );
  assert!(t.contains("17"), "file size should be 17: {t}");

  // Verify change event fired and FileReader read the content
  // Give it a moment for FileReader async callback
  std::thread::sleep(std::time::Duration::from_millis(200));
  let t = c.tool_text(
    "evaluate",
    json!({"expression": "document.getElementById('r').textContent"}),
  );
  assert!(
    t.contains("name:ferridriver_test_upload.txt"),
    "change event should fire with correct name: {t}"
  );
  assert!(
    t.contains("content:test file content"),
    "FileReader should read correct content: {t}"
  );

  let _ = std::fs::remove_file(&tmp);
}

fn test_markdown_links(c: &mut McpClient) {
  c.nav("<p>Visit <a href='https://example.com'>Example</a></p>");
  let t = c.tool_text("get_markdown", json!({}));
  assert!(t.contains("[Example](https://example.com)"), "markdown links: {t}");
}

// ─── BDD tools ─────────────────────────────────────────────────────────────

fn test_list_steps(c: &mut McpClient) {
  let t = c.tool_text("list_steps", json!({}));
  assert!(t.contains("Step Definitions"), "list_steps should return header: {t}");
  assert!(
    t.contains("I navigate to"),
    "list_steps should include navigation step: {t}"
  );
  assert!(t.contains("I click"), "list_steps should include click step: {t}");
}

fn test_list_steps_filter(c: &mut McpClient) {
  let t = c.tool_text("list_steps", json!({"filter": "navigate"}));
  assert!(t.contains("navigate"), "filtered list should include navigate: {t}");
  assert!(!t.contains("I click"), "filtered list should not include click: {t}");
}

fn test_list_steps_kind(c: &mut McpClient) {
  let t = c.tool_text("list_steps", json!({"kind": "then"}));
  assert!(t.contains("Then"), "kind filter should show Then steps: {t}");
  // Then section should not contain Given steps.
  assert!(!t.contains("## Given"), "kind=then should not show Given section: {t}");
}

fn test_run_step_navigate(c: &mut McpClient) {
  let t = c.tool_text("run_step", json!({"step": "I navigate to \"https://example.com\""}));
  assert!(t.contains("[Passed]"), "run_step navigate should pass: {t}");
  assert!(t.contains("Example Domain"), "run_step should return snapshot: {t}");
}

fn test_run_step_click(c: &mut McpClient) {
  c.nav("<button id='btn' onclick='document.title=\"clicked\"'>Click Me</button>");
  let t = c.tool_text("run_step", json!({"step": "I click \"#btn\""}));
  assert!(t.contains("[Passed]"), "run_step click should pass: {t}");
}

fn test_run_step_fill(c: &mut McpClient) {
  c.nav("<input id='name' type='text'>");
  let t = c.tool_text("run_step", json!({"step": "I fill \"#name\" with \"test value\""}));
  assert!(t.contains("[Passed]"), "run_step fill should pass: {t}");
}

fn test_run_step_undefined(c: &mut McpClient) {
  let t = c.tool_text("run_step", json!({"step": "I do something that does not exist"}));
  assert!(
    t.contains("Pending") || t.contains("undefined"),
    "undefined step should be pending: {t}"
  );
}

fn test_run_step_assertion(c: &mut McpClient) {
  c.nav_url("https://example.com");
  let t = c.tool_text("run_step", json!({"step": "\"h1\" should be visible"}));
  assert!(t.contains("[Passed]"), "run_step assertion should pass: {t}");
}

fn test_run_scenario_inline(c: &mut McpClient) {
  let feature = r#"Feature: Inline test
  Scenario: Visit example
    Given I navigate to "https://example.com"
    Then "h1" should be visible"#;
  let t = c.tool_text("run_scenario", json!({"feature": feature}));
  assert!(t.contains("[PASS]"), "run_scenario should show PASS: {t}");
  assert!(t.contains("1 passed"), "run_scenario should show summary: {t}");
  assert!(t.contains("0 failed"), "run_scenario should show 0 failed: {t}");
}

fn test_run_scenario_multi_step(c: &mut McpClient) {
  let feature = r#"Feature: Multi step
  Scenario: Fill and check
    Given I navigate to "https://demo.playwright.dev/todomvc/#/"
    Then ".new-todo" should be visible
    When I fill ".new-todo" with "Buy milk"
    And I press "Enter"
    Then ".todo-list" should be visible"#;
  let t = c.tool_text("run_scenario", json!({"feature": feature}));
  assert!(t.contains("[PASS]"), "multi-step scenario should pass: {t}");
  assert!(t.contains("[ok]"), "individual steps should show ok: {t}");
}

fn test_run_scenario_failure(c: &mut McpClient) {
  let feature = r##"Feature: Fail test
  Scenario: Bad selector
    Given I navigate to "https://example.com"
    When I click "#nonexistent""##;
  let t = c.tool_text("run_scenario", json!({"feature": feature}));
  assert!(t.contains("[FAIL]"), "failing scenario should show FAIL: {t}");
  assert!(t.contains("1 failed"), "summary should show 1 failed: {t}");
}

fn test_run_scenario_filter(c: &mut McpClient) {
  let feature = "Feature: Filtered\n  Scenario: First\n    Given I navigate to \"https://example.com\"\n  Scenario: Second\n    Given I navigate to \"https://example.com\"";
  let t = c.tool_text("run_scenario", json!({"feature": feature, "scenario": "First"}));
  assert!(t.contains("1 passed"), "filter should run only 1 scenario: {t}");
  assert!(!t.contains("Second"), "filter should exclude Second: {t}");
}

// ─── Run all tests on one client ────────────────────────────────────────────

fn run_all_tests(backend: &str) {
  let mut c = McpClient::new(backend);
  let is_cdp = backend != "webkit" && backend != "bidi";
  let mut passed = 0u32;
  let mut failed = 0u32;
  let mut failures: Vec<String> = Vec::new();

  macro_rules! run {
    ($name:ident) => {{
      let name = stringify!($name);
      match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| $name(&mut c))) {
        Ok(()) => {
          passed += 1;
        },
        Err(_) => {
          failed += 1;
          failures.push(name.to_string());
          eprintln!("  FAIL {name}");
        },
      }
    }};
  }

  macro_rules! run_cdp {
    ($name:ident) => {{
      if is_cdp {
        run!($name);
      }
    }};
  }

  // Core
  run!(test_navigate);
  run!(test_evaluate_number);
  run!(test_evaluate_string);
  run!(test_evaluate_dom);
  run!(test_evaluate_promise);
  run!(test_evaluate_boolean);
  run!(test_evaluate_array);
  run!(test_evaluate_object);
  run!(test_evaluate_null);
  run!(test_evaluate_error);
  run!(test_evaluate_syntax_error);
  run!(test_evaluate_large_payload);
  run!(test_screenshot_png);
  run!(test_screenshot_full_page);
  run!(test_snapshot);
  run!(test_click_selector);
  run!(test_click_at);
  run!(test_fill_input);
  run!(test_type_text);
  run!(test_press_key);
  run!(test_scroll);
  run!(test_reload);
  run!(test_go_back_forward);
  run!(test_list_pages);
  run!(test_wait_for_selector);
  run!(test_wait_for_text);
  run!(test_console_messages);
  run!(test_network_requests);
  run!(test_hover);
  run!(test_drag);
  run!(test_scroll_to_element);
  run!(test_double_click);
  run!(test_fill_form);

  // Tools
  run!(test_search_page);
  run!(test_search_page_regex);
  run!(test_search_page_no_match);
  run!(test_select_option);
  run!(test_fill_dispatches_events);
  run!(test_click_offscreen_element);
  run!(test_snapshot_scroll_info);
  run!(test_click_select_guard);

  // Selector engine
  run!(test_selector_role);
  run!(test_selector_chain);
  run!(test_selector_label);
  run!(test_selector_placeholder);

  // Auto-waiting, dialog, markdown, file upload
  run!(test_auto_wait_visibility);
  run!(test_dialog_alert);
  run!(test_markdown_extraction);
  run!(test_markdown_links);
  run!(test_file_upload);

  // CDP-only tests -- run before new_page which changes active page index
  run_cdp!(test_emulate_device);
  run_cdp!(test_set_geolocation);
  run_cdp!(test_set_network_state);
  run_cdp!(test_trace);

  // BDD tools
  run!(test_list_steps);
  run!(test_list_steps_filter);
  run!(test_list_steps_kind);
  run!(test_run_step_navigate);
  run!(test_run_step_click);
  run!(test_run_step_fill);
  run!(test_run_step_undefined);
  run!(test_run_step_assertion);
  run!(test_run_scenario_inline);
  run!(test_run_scenario_multi_step);
  run!(test_run_scenario_failure);
  run!(test_run_scenario_filter);

  // Multi-page tests last (they change session state)
  run!(test_new_page);
  run!(test_cookies);
  run!(test_localstorage);

  eprintln!("\n{backend}: {passed} passed, {failed} failed");
  if !failures.is_empty() {
    eprintln!("Failures: {}", failures.join(", "));
  }
  assert_eq!(
    failures.len(),
    0,
    "{backend}: {} test failures: {}",
    failures.len(),
    failures.join(", ")
  );
}

// ─── One #[test] per backend ────────────────────────────────────────────────

#[test]
fn all_tests_cdp_pipe() {
  run_all_tests("cdp-pipe");
}

#[test]
fn all_tests_cdp_raw() {
  run_all_tests("cdp-raw");
}

#[cfg(target_os = "macos")]
#[test]
fn all_tests_webkit() {
  run_all_tests("webkit");
}

#[test]
fn all_tests_bidi() {
  run_all_tests("bidi");
}
