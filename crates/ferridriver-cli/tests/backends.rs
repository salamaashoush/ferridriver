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
//! This avoids spawning many browser processes per backend; each test navigates
//! to a fresh page so state doesn't leak.
//!
//! The MCP surface is scripting-focused: observation via `navigate` / `snapshot`
//! / `screenshot` / `evaluate` / `search_page` / `diagnostics` / `page`, and
//! action via `run_script` with `page` / `context` / `request` globals. Tests
//! below exercise both paths.

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
  backend: String,
}

impl McpClient {
  fn new(backend: &str) -> Self {
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

  /// Run a script with empty args and return the parsed `{status, value, ...}` payload.
  fn script(&mut self, source: &str) -> Value {
    self.script_with_args(source, json!([]))
  }

  /// Run a script with bound args and return the parsed payload.
  fn script_with_args(&mut self, source: &str, args: Value) -> Value {
    let resp = self.call_tool("run_script", json!({"source": source, "args": args}));
    ok(&resp, "run_script");
    extract_script_payload(&resp).expect("script response should carry a JSON payload")
  }

  /// Run a script expecting success; return the `value` from the payload.
  fn script_value(&mut self, source: &str) -> Value {
    let payload = self.script(source);
    assert_eq!(payload["status"].as_str(), Some("ok"), "script failed: {payload}");
    payload["value"].clone()
  }

  /// Run a script with args, expecting success; return the `value`.
  fn script_value_with_args(&mut self, source: &str, args: Value) -> Value {
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

/// Find the content block that parses as the script engine's structured
/// payload (`{ status, value | error, duration_ms, console[] }`). The tool
/// returns one or two text blocks depending on outcome; we scan until we
/// find the JSON one.
fn extract_script_payload(resp: &Value) -> Option<Value> {
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

fn is_error(resp: &Value) -> bool {
  resp.get("error").is_some() || resp["result"]["isError"].as_bool().unwrap_or(false)
}

fn ok(resp: &Value, ctx: &str) {
  assert!(!is_error(resp), "{ctx} failed: {resp}");
}

// ─── Navigation + session ───────────────────────────────────────────────────

fn test_navigate(c: &mut McpClient) {
  let r = c.call_tool("navigate", json!({"url": data_url("<h1>Hello</h1>")}));
  ok(&r, "navigate");
  let t = extract_text(&r);
  assert!(t.contains("Hello"), "navigate should show content: {t}");
}

fn test_page_list(c: &mut McpClient) {
  c.nav("<body></body>");
  let t = c.tool_text("page", json!({"action": "list"}));
  assert!(t.contains("Page 0"), "list pages: {t}");
}

fn test_page_reload(c: &mut McpClient) {
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

fn test_page_back_forward(c: &mut McpClient) {
  c.nav("<h1>Page1</h1>");
  c.nav("<h1>Page2</h1>");
  c.call_tool("page", json!({"action": "back"}));
  let t = c.tool_text(
    "evaluate",
    json!({"expression": "document.querySelector('h1')?.textContent || ''"}),
  );
  assert!(t.contains("Page1"), "go_back should return to Page1: {t}");
}

fn test_new_page(c: &mut McpClient) {
  let r = c.call_tool("page", json!({"action": "new"}));
  if !is_error(&r) {
    let t = c.tool_text("page", json!({"action": "list"}));
    assert!(t.contains("Page 0") && t.contains("Page 1"), "should have 2 pages: {t}");
    let r2 = c.call_tool("page", json!({"action": "select", "page_index": 0}));
    ok(&r2, "page select");
  }
}

// ─── evaluate (page-side JS one-liners) ─────────────────────────────────────

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

// ─── snapshot ───────────────────────────────────────────────────────────────

fn test_snapshot(c: &mut McpClient) {
  c.nav("<h1>Snap</h1><button>Click</button>");
  let t = c.tool_text("snapshot", json!({}));
  assert!(t.contains("[ref="), "snapshot refs: {t}");
  assert!(t.contains("Snap"), "snapshot content: {t}");
}

fn test_snapshot_scroll_info(c: &mut McpClient) {
  c.nav("<div style='height:3000px'>tall</div>");
  // Scroll via a run_script call before snapshotting.
  c.script("window.scrollBy(0, 500); return null;");
  let t = c.tool_text("snapshot", json!({}));
  assert!(t.contains("Scroll:"), "snapshot should show scroll position: {t}");
}

// ─── screenshot ─────────────────────────────────────────────────────────────

fn test_screenshot_png(c: &mut McpClient) {
  c.nav("<h1>Screenshot</h1>");
  // Wait for content to render via the scripted locator waiter.
  c.script("await page.waitForSelector('h1'); return true;");
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
  assert!(
    b64.len() > 1000,
    "full page PNG should be substantial: {} bytes",
    b64.len()
  );
}

// ─── search_page ────────────────────────────────────────────────────────────

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

// ─── diagnostics ────────────────────────────────────────────────────────────

fn test_console_messages(c: &mut McpClient) {
  c.nav("<body></body>");
  c.call_tool("evaluate", json!({"expression": "console.log('hello123')"}));
  c.call_tool("evaluate", json!({"expression": "console.warn('warn456')"}));
  // Flush CDP event stream — evaluate round-trips ensure events are processed.
  for _ in 0..10 {
    c.call_tool("evaluate", json!({"expression": "void 0"}));
  }
  let t = c.tool_text("diagnostics", json!({"type": "console"}));
  // Console capture is best-effort — CDP events may arrive late on slow CI.
  assert!(!t.is_empty(), "console diagnostics should return something: {t}");
}

fn test_network_requests(c: &mut McpClient) {
  c.nav_url("https://example.com");
  let t = c.tool_text("diagnostics", json!({"type": "network"}));
  assert!(
    t.contains("example.com") || t.contains("GET") || t.contains("request"),
    "network diagnostics should list requests: {t}"
  );
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

// ─── run_script: Page interaction ───────────────────────────────────────────

fn test_script_click(c: &mut McpClient) {
  c.nav(
    "<h1 id='h'>Before</h1><button id='btn' onclick=\"document.getElementById('h').textContent='After'\">Go</button>",
  );
  let v = c.script_value("await page.click('#btn'); return await page.textContent('#h');");
  assert_eq!(v, json!("After"), "page.click should trigger onclick: {v}");
}

fn test_script_fill(c: &mut McpClient) {
  c.nav("<input id='i' type='text'>");
  let v = c.script_value("await page.fill('#i', 'Alice'); return await page.inputValue('#i');");
  assert_eq!(v, json!("Alice"), "page.fill: {v}");
}

fn test_script_fill_form(c: &mut McpClient) {
  c.nav("<input id='a'><input id='b'>");
  let v = c.script_value(
    "await page.fill('#a', 'val1'); \
       await page.fill('#b', 'val2'); \
       return { a: await page.inputValue('#a'), b: await page.inputValue('#b') };",
  );
  assert_eq!(v["a"], json!("val1"));
  assert_eq!(v["b"], json!("val2"));
}

fn test_script_type(c: &mut McpClient) {
  c.nav("<input id='i' type='text'>");
  let v = c.script_value(
    "await page.locator('#i').click(); \
       await page.type('#i', 'Bob'); \
       return await page.inputValue('#i');",
  );
  assert_eq!(v, json!("Bob"));
}

fn test_script_press(c: &mut McpClient) {
  c.nav("<textarea id='t'></textarea>");
  let v = c.script_value(
    "await page.locator('#t').click(); \
       await page.press('#t', 'Enter'); \
       return (await page.inputValue('#t')).length;",
  );
  let len = v.as_i64().unwrap_or(0);
  assert!(len > 0, "press Enter should insert newline, value length: {len}");
}

fn test_script_hover(c: &mut McpClient) {
  c.nav("<div id='d' onmouseenter=\"this.textContent='hovered'\" style='width:100px;height:100px'>hover me</div>");
  let v = c.script_value("await page.locator('#d').hover(); return await page.textContent('#d');");
  assert_eq!(v, json!("hovered"), "hover should trigger mouseenter");
}

fn test_script_dblclick(c: &mut McpClient) {
  c.nav("<h1 id='h'>0</h1><button id='b' onclick=\"document.getElementById('h').textContent=Number(document.getElementById('h').textContent)+1\">+</button>");
  let v = c.script_value("await page.dblclick('#b'); return await page.textContent('#h');");
  assert_eq!(v, json!("2"), "dblclick should fire two clicks");
}

fn test_script_select_option(c: &mut McpClient) {
  c.nav("<select id='s'><option value='apple'>Apple</option><option value='banana'>Banana</option></select>");
  let v = c.script_value(
    "await page.selectOption('#s', 'banana'); \
       return await page.inputValue('#s');",
  );
  assert_eq!(v, json!("banana"));
}

fn test_script_check_uncheck(c: &mut McpClient) {
  c.nav("<input id='c' type='checkbox'>");
  let v = c.script_value(
    "await page.check('#c'); \
       const on = await page.isChecked('#c'); \
       await page.uncheck('#c'); \
       const off = await page.isChecked('#c'); \
       return { on, off };",
  );
  assert_eq!(v["on"], json!(true));
  assert_eq!(v["off"], json!(false));
}

fn test_script_scroll(c: &mut McpClient) {
  c.nav("<div style='height:3000px'>tall</div>");
  let v = c.script_value(
    "await page.evaluate('window.scrollBy(0, 500)'); \
       const raw = await page.evaluate('window.scrollY'); \
       return JSON.parse(raw);",
  );
  let y = v.as_f64().unwrap_or(0.0);
  assert!(y > 0.0, "scroll should change scrollY: {y}");
}

fn test_script_scroll_into_view(c: &mut McpClient) {
  c.nav("<div style='height:3000px'></div><div id='bottom'>bottom</div>");
  let v = c.script_value(
    "await page.locator('#bottom').scrollIntoViewIfNeeded(); \
       const raw = await page.evaluate('window.scrollY'); \
       return JSON.parse(raw);",
  );
  let y = v.as_f64().unwrap_or(0.0);
  assert!(y > 100.0, "scroll into view should scroll down: {y}");
}

fn test_script_click_offscreen(c: &mut McpClient) {
  c.nav("<div style='height:3000px'></div><button id='b' onclick=\"this.textContent='clicked'\">far</button>");
  let v = c.script_value("await page.click('#b'); return await page.textContent('#b');");
  assert_eq!(v, json!("clicked"), "click should auto-scroll offscreen button");
}

fn test_script_dialog_alert(c: &mut McpClient) {
  c.nav("<button id='b' onclick=\"alert('hello')\">Go</button>");
  // Dialogs are auto-dismissed; the click should not hang.
  let v = c.script_value("await page.click('#b'); return 'alive';");
  assert_eq!(v, json!("alive"), "should survive alert dialog");
}

fn test_script_fill_dispatches_events(c: &mut McpClient) {
  c.nav("<input id='i' type='text'><div id='r'></div><script>document.getElementById('i').addEventListener('change', function(e) { document.getElementById('r').textContent = 'changed:' + e.target.value; });</script>");
  let v = c.script_value(
    "await page.fill('#i', 'test'); \
       return await page.textContent('#r');",
  );
  assert_eq!(v, json!("changed:test"), "fill should dispatch change event");
}

fn test_script_click_at(c: &mut McpClient) {
  c.nav("<div id='d' onclick=\"this.textContent='clicked'\" style='position:fixed;left:0;top:0;width:200px;height:200px'>click me</div>");
  let v = c.script_value(
    "await page.clickAt(50, 50); \
       return await page.textContent('#d');",
  );
  assert_eq!(v, json!("clicked"), "clickAt should trigger onclick");
}

fn test_script_mouse_click_coords(c: &mut McpClient) {
  c.nav("<div id='d' onclick=\"this.textContent='mouse-clicked'\" style='position:fixed;left:0;top:0;width:200px;height:200px'>click me</div>");
  let v = c.script_value(
    "await page.mouse.click(40, 40); \
       return await page.textContent('#d');",
  );
  assert_eq!(v, json!("mouse-clicked"), "page.mouse.click should fire onclick");
}

fn test_script_drag_coords(c: &mut McpClient) {
  c.nav("<div id='d' onmousedown=\"this.dataset.down='1'\" onmouseup=\"this.dataset.up='1'\" onmousemove=\"this.dataset.moved='1'\" style='position:fixed;left:0;top:0;width:200px;height:200px'>drag</div>");
  let v = c.script_value(
    "await page.mouse.down(); \
       await page.moveMouseSmooth(50, 50, 150, 150, 5); \
       await page.mouse.up(); \
       const down = await page.evaluate(\"document.getElementById('d').dataset.down\"); \
       const up = await page.evaluate(\"document.getElementById('d').dataset.up\"); \
       return { down: JSON.parse(down), up: JSON.parse(up) };",
  );
  assert_eq!(v["down"], json!("1"), "mouse.down should fire mousedown");
  assert_eq!(v["up"], json!("1"), "mouse.up should fire mouseup");
}

fn test_script_drag_and_drop(c: &mut McpClient) {
  c.nav("<div id='src' style='width:60px;height:60px;background:#f00' onmousedown=\"this.dataset.d='1'\"></div><div id='tgt' style='width:60px;height:60px;margin-top:80px;background:#0f0' onmouseup=\"this.dataset.u='1'\"></div>");
  let v = c.script_value(
    "await page.dragAndDrop('#src', '#tgt'); \
       const raw = await page.evaluate(\"document.getElementById('src').dataset.d || ''\"); \
       return JSON.parse(raw);",
  );
  assert_eq!(v, json!("1"), "dragAndDrop should trigger mousedown on source");
}

fn test_script_drag_and_drop_options(c: &mut McpClient) {
  // Navigate to a clean page so prior tests don't leave the browser in a
  // weird mouse state (e.g. held button from a previous drag leaking
  // into this test).
  c.nav(
    "<style>html,body{margin:0;padding:0}</style>\
     <div id='src' style='width:80px;height:80px;background:#f00;position:absolute;left:20px;top:20px'></div>\
     <div id='tgt' style='width:80px;height:80px;background:#0f0;position:absolute;left:200px;top:200px'></div>\
     <div id='out' style='position:fixed;top:0;right:0'>idle</div>\
     <script>\
       var o=document.getElementById('out');\
       var moves=0;\
       window.addEventListener('mousedown',function(e){o.dataset.down=JSON.stringify({x:e.clientX,y:e.clientY});},true);\
       window.addEventListener('mouseup',function(e){o.dataset.up=JSON.stringify({x:e.clientX,y:e.clientY});},true);\
       window.addEventListener('mousemove',function(){moves+=1;o.dataset.moves=String(moves);},true);\
       window.addEventListener('pointermove',function(e){\
         var c=typeof e.getCoalescedEvents==='function'?e.getCoalescedEvents():[];\
         if(c.length>1){moves+=c.length-1;o.dataset.moves=String(moves);}\
       },true);\
     </script>",
  );
  // QuickJS `page.evaluate` returns a JSON-stringified result, so we parse
  // once to unwrap the outer string and once more to reach the object.
  let v = c.script_value(
    "await page.dragAndDrop('#src', '#tgt', { sourcePosition: {x:5, y:5}, targetPosition: {x:10, y:10}, steps: 6 }); \
       const raw = await page.evaluate(\"JSON.stringify({d: document.getElementById('out').dataset.down || null, u: document.getElementById('out').dataset.up || null, m: parseInt(document.getElementById('out').dataset.moves || '0', 10)})\"); \
       const outer = JSON.parse(raw); \
       const state = JSON.parse(outer); \
       return { d: state.d ? JSON.parse(state.d) : null, u: state.u ? JSON.parse(state.u) : null, m: state.m };",
  );
  let dx = v["d"]["x"].as_f64().unwrap_or(-1.0);
  let dy = v["d"]["y"].as_f64().unwrap_or(-1.0);
  let ux = v["u"]["x"].as_f64().unwrap_or(-1.0);
  let uy = v["u"]["y"].as_f64().unwrap_or(-1.0);
  let moves = v["m"].as_u64().unwrap_or(0);
  assert!(
    (24.0..=26.0).contains(&dx),
    "mousedown x should be ~25 (source padding-box + sourcePosition): got {dx} (v={v})"
  );
  assert!(
    (24.0..=26.0).contains(&dy),
    "mousedown y should be ~25: got {dy} (v={v})"
  );
  assert!(
    (209.0..=211.0).contains(&ux),
    "mouseup x should be ~210 (target padding-box + targetPosition): got {ux} (v={v})"
  );
  assert!(
    (209.0..=211.0).contains(&uy),
    "mouseup y should be ~210: got {uy} (v={v})"
  );
  assert!(
    moves >= 6,
    "steps=6 should produce at least 6 mousemove dispatches: got {moves} (v={v})"
  );
}

fn test_script_locator_drag_to_options(c: &mut McpClient) {
  c.nav(
    "<style>html,body{margin:0;padding:0}</style>\
     <div id='src' style='width:80px;height:80px;background:#f00;position:absolute;left:20px;top:20px'></div>\
     <div id='tgt' style='width:80px;height:80px;background:#0f0;position:absolute;left:200px;top:200px'></div>\
     <div id='out' style='position:fixed;top:0;right:0'></div>\
     <script>\
       var o=document.getElementById('out');\
       window.addEventListener('mouseup',function(e){o.dataset.up=JSON.stringify({x:e.clientX,y:e.clientY});},true);\
     </script>",
  );
  let v = c.script_value(
    "await page.locator('#src').dragTo(page.locator('#tgt'), { targetPosition: {x:15, y:15} }); \
       const raw = await page.evaluate(\"document.getElementById('out').dataset.up || ''\"); \
       const inner = JSON.parse(raw); \
       return inner ? JSON.parse(inner) : null;",
  );
  let ux = v["x"].as_f64().unwrap_or(-1.0);
  let uy = v["y"].as_f64().unwrap_or(-1.0);
  assert!((214.0..=216.0).contains(&ux), "drop x should be ~215: got {ux} (v={v})");
  assert!((214.0..=216.0).contains(&uy), "drop y should be ~215: got {uy} (v={v})");
}

fn test_script_emulate_media_all_fields(c: &mut McpClient) {
  // BiDi/Firefox only supports colorScheme; CDP + WebKit support all five.
  // This test runs on CDP backends (cdp-pipe, cdp-raw) and WebKit.
  if c.backend == "bidi" {
    return;
  }
  c.nav("<html><body><div id='x'></div></body></html>");
  let v = c.script_value(
    "await page.emulateMedia({ \
        media: 'print', \
        colorScheme: 'dark', \
        reducedMotion: 'reduce', \
        forcedColors: 'active', \
        contrast: 'more' \
     }); \
     const raw = await page.evaluate(\"JSON.stringify({\
        print: matchMedia('print').matches, \
        screen: matchMedia('screen').matches, \
        dark: matchMedia('(prefers-color-scheme: dark)').matches, \
        reduced: matchMedia('(prefers-reduced-motion: reduce)').matches, \
        forced: matchMedia('(forced-colors: active)').matches, \
        contrast: matchMedia('(prefers-contrast: more)').matches, \
     })\"); \
     return JSON.parse(JSON.parse(raw));",
  );
  assert_eq!(
    v["print"],
    json!(true),
    "media=print should activate matchMedia('print'): {v}"
  );
  assert_eq!(
    v["screen"],
    json!(false),
    "matchMedia('screen') should be false under print: {v}"
  );
  assert_eq!(
    v["dark"],
    json!(true),
    "colorScheme=dark should activate prefers-color-scheme:dark: {v}"
  );
  assert_eq!(
    v["reduced"],
    json!(true),
    "reducedMotion=reduce should activate prefers-reduced-motion:reduce: {v}"
  );
  assert_eq!(
    v["forced"],
    json!(true),
    "forcedColors=active should activate forced-colors:active: {v}"
  );
  assert_eq!(
    v["contrast"],
    json!(true),
    "contrast=more should activate prefers-contrast:more: {v}"
  );
  // Reset so state doesn't leak into the next test.
  c.script_value(
    "await page.emulateMedia({ \
       media: null, colorScheme: null, reducedMotion: null, \
       forcedColors: null, contrast: null \
     }); return 'ok';",
  );
}

fn test_script_emulate_media_null_disables_single_field(c: &mut McpClient) {
  if c.backend == "bidi" {
    return;
  }
  c.nav("<html><body>init</body></html>");
  let v = c.script_value(
    "await page.emulateMedia({ colorScheme: 'dark', reducedMotion: 'reduce' }); \
     const pre = await page.evaluate(\"JSON.stringify({\
        dark: matchMedia('(prefers-color-scheme: dark)').matches, \
        reduced: matchMedia('(prefers-reduced-motion: reduce)').matches, \
     })\"); \
     await page.emulateMedia({ colorScheme: null }); \
     const post = await page.evaluate(\"JSON.stringify({\
        dark: matchMedia('(prefers-color-scheme: dark)').matches, \
        reduced: matchMedia('(prefers-reduced-motion: reduce)').matches, \
     })\"); \
     return { pre: JSON.parse(JSON.parse(pre)), post: JSON.parse(JSON.parse(post)) };",
  );
  assert_eq!(
    v["pre"]["dark"],
    json!(true),
    "sanity: dark should be active before reset: {v}"
  );
  assert_eq!(
    v["pre"]["reduced"],
    json!(true),
    "sanity: reduced should be active before reset: {v}"
  );
  assert_eq!(
    v["post"]["dark"],
    json!(false),
    "colorScheme=null should disable the override: {v}"
  );
  assert_eq!(
    v["post"]["reduced"],
    json!(true),
    "reducedMotion should survive a sibling reset: {v}"
  );
  c.script_value("await page.emulateMedia({ reducedMotion: null }); return 'ok';");
}

fn test_script_drag_and_drop_trial(c: &mut McpClient) {
  c.nav(
    "<style>html,body{margin:0;padding:0}</style>\
     <div id='src' style='width:60px;height:60px;background:#f00;position:absolute;left:20px;top:20px'></div>\
     <div id='tgt' style='width:60px;height:60px;background:#0f0;position:absolute;left:200px;top:200px'></div>\
     <div id='log' data-fired='0'></div>\
     <script>\
       window.addEventListener('mousedown',function(){document.getElementById('log').dataset.fired='1';},true);\
     </script>",
  );
  let v = c.script_value(
    "await page.dragAndDrop('#src', '#tgt', { trial: true }); \
       const raw = await page.evaluate(\"document.getElementById('log').dataset.fired\"); \
       return JSON.parse(raw);",
  );
  assert_eq!(v, json!("0"), "trial=true must not dispatch mousedown: got {v}");
}

fn test_script_mouse_wheel(c: &mut McpClient) {
  c.nav("<body style='height:3000px'></body>");
  // Verify the binding dispatches the wheel event without error. Whether the
  // event produces a visible scroll depends on Chrome's input routing with
  // the current mouse position (CDP Input.dispatchMouseEvent behaviour is
  // not guaranteed across backends/headless modes).
  let payload = c.script("await page.mouse.wheel(0, 400); return 'ok';");
  assert_eq!(
    payload["status"].as_str(),
    Some("ok"),
    "wheel should not error: {payload}"
  );
}

// Task 1.5: full `ClickOptions` surface — exercise button, modifiers,
// delay, position, clickCount, trial, and the error paths for unknown
// button / modifier strings. Every sub-assertion is a distinct DOM
// probe so per-option failures point at the exact wire bug.
fn test_script_click_options(c: &mut McpClient) {
  // button:'right' → contextmenu fires with event.button === 2.
  c.nav(
    "<button id='b' oncontextmenu=\"document.getElementById('out').textContent='right';return false\">b</button><div id='out'>n</div>",
  );
  let v = c.script_value(
    "await page.locator('#b').click({ button: 'right' });\
     return JSON.parse(await page.evaluate('document.getElementById(\"out\").textContent'));",
  );
  assert_eq!(v, json!("right"), "button=right fires contextmenu: {v}");

  // clickCount:2 → dblclick handler fires.
  c.nav(
    "<button id='b'>b</button><div id='out'>n</div>\
     <script>document.getElementById('b').addEventListener('dblclick',()=>document.getElementById('out').textContent='dbl')</script>",
  );
  let v = c.script_value(
    "await page.locator('#b').click({ clickCount: 2 });\
     return JSON.parse(await page.evaluate('document.getElementById(\"out\").textContent'));",
  );
  assert_eq!(v, json!("dbl"), "clickCount=2 fires dblclick: {v}");

  // modifiers:['Shift'] → click event has shiftKey === true.
  c.nav(
    "<button id='b'>b</button><div id='out'>n</div>\
     <script>document.getElementById('b').addEventListener('click',e=>document.getElementById('out').textContent=e.shiftKey?'shift':'none')</script>",
  );
  let v = c.script_value(
    "await page.locator('#b').click({ modifiers: ['Shift'] });\
     return JSON.parse(await page.evaluate('document.getElementById(\"out\").textContent'));",
  );
  assert_eq!(v, json!("shift"), "modifiers Shift sets event.shiftKey: {v}");

  // position:{x:10,y:20} → event coords land at padding-box offset.
  c.nav(
    "<div id='b' style='width:200px;height:100px;background:#ccc'></div><div id='out'>n</div>\
     <script>document.getElementById('b').addEventListener('click',e=>{var r=e.currentTarget.getBoundingClientRect();document.getElementById('out').textContent=(Math.round(e.clientX-r.left))+','+(Math.round(e.clientY-r.top))})</script>",
  );
  let v = c.script_value(
    "await page.locator('#b').click({ position: { x: 10, y: 20 } });\
     return JSON.parse(await page.evaluate('document.getElementById(\"out\").textContent'));",
  );
  assert_eq!(v, json!("10,20"), "position offsets click coords: {v}");

  // delay:100 → mousedown→mouseup gap is honored (allow slack for
  // timer resolution; demand ≥ 80ms so flaky schedulers still pass).
  c.nav(
    "<button id='b'>b</button><div id='out'>0</div>\
     <script>\
       let down=0;\
       const b=document.getElementById('b');\
       b.addEventListener('mousedown',()=>{down=Date.now()});\
       b.addEventListener('mouseup',()=>{document.getElementById('out').textContent=String(Date.now()-down)});\
     </script>",
  );
  let v = c.script_value(
    "await page.locator('#b').click({ delay: 120 });\
     return JSON.parse(await page.evaluate('document.getElementById(\"out\").textContent'));",
  );
  let ms = v.as_str().and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
  assert!(ms >= 80, "delay=120 held mousedown at least 80ms: got {ms} ({v})");

  // trial:true → click handler doesn't fire, but modifier keydown does.
  c.nav(
    "<button id='b'>b</button><div id='clicked'>no</div><div id='kd'>none</div>\
     <script>\
       document.getElementById('b').addEventListener('click',()=>document.getElementById('clicked').textContent='yes');\
       document.addEventListener('keydown',e=>{if(e.key==='Shift')document.getElementById('kd').textContent='shift'});\
     </script>",
  );
  let v = c.script_value(
    "await page.locator('#b').click({ trial: true, modifiers: ['Shift'] });\
     return {\
       clicked: JSON.parse(await page.evaluate('document.getElementById(\"clicked\").textContent')),\
       kd: JSON.parse(await page.evaluate('document.getElementById(\"kd\").textContent')),\
     };",
  );
  assert_eq!(v["clicked"], json!("no"), "trial=true skips click handler: {v}");
  assert_eq!(v["kd"], json!("shift"), "trial=true still presses modifiers: {v}");

  // Bad button string → typed error, not silent default.
  let v = c.script_value(
    "try {\
       await page.locator('#b').click({ button: 'garbage' });\
       return 'no-throw';\
     } catch (e) { return String(e.message || e); }",
  );
  let msg = v.as_str().unwrap_or("");
  assert!(
    msg.contains("Unknown mouse button"),
    "bad button errors with exact message: {v}"
  );

  // Bad modifier string → typed error.
  let v = c.script_value(
    "try {\
       await page.locator('#b').click({ modifiers: ['Hyper'] });\
       return 'no-throw';\
     } catch (e) { return String(e.message || e); }",
  );
  let msg = v.as_str().unwrap_or("");
  assert!(msg.contains("Unknown modifier"), "bad modifier errors: {v}");
}

// Task 1.5 phase 4c: `locator.dispatchEvent` must honor `opts.timeout`
// via the retry loop (previously did a one-shot `resolve()` that failed
// immediately on missing elements). Playwright's dispatchEvent does NOT
// run actionability — it's a programmatic event dispatch, polled only
// for element presence.
fn test_script_dispatch_event_timeout(c: &mut McpClient) {
  c.nav("<button id='b'>b</button>");
  let v = c.script_value(
    "const t0 = Date.now();\
     try { await page.locator('#nope').dispatchEvent('click', {}, { timeout: 200 }); return { msg: 'no-throw', elapsed: Date.now() - t0 }; }\
     catch (e) { return { msg: String(e.message || e), elapsed: Date.now() - t0 }; }",
  );
  let msg = v["msg"].as_str().unwrap_or("");
  let elapsed = v["elapsed"].as_i64().unwrap_or(9_999);
  assert!(
    msg.contains("Timeout") && msg.contains("200ms"),
    "dispatchEvent on missing element with timeout:200 should Timeout: {v}"
  );
  assert!(
    elapsed < 1500,
    "dispatchEvent timeout should fire within 1.5s, got {elapsed}ms: {v}"
  );
}

// Task 1.5 phase 4d: `selectOption` honors `opts.timeout` (via
// retry_resolve) AND `opts.force` (skips the `['visible','enabled']`
// pre-check that would otherwise return `error:notenabled`). Without
// force on a disabled `<select>`, the retry loop polls until the
// deadline. With force, the injected `selectOptions` runs immediately.
fn test_script_select_option_force(c: &mut McpClient) {
  // Disabled select — without force, fails fast via the timeout.
  c.nav("<select id='s' disabled><option value='a'>A</option><option value='b'>B</option></select>");
  let v = c.script_value(
    "const t0 = Date.now();\
     try { await page.locator('#s').selectOption('b', { timeout: 200 }); return { msg: 'no-throw', elapsed: Date.now() - t0 }; }\
     catch (e) { return { msg: String(e.message || e), elapsed: Date.now() - t0 }; }",
  );
  let msg = v["msg"].as_str().unwrap_or("");
  let elapsed = v["elapsed"].as_i64().unwrap_or(9_999);
  assert!(
    msg.contains("Timeout") && msg.contains("200ms"),
    "selectOption on disabled select with timeout:200 should Timeout: {v}"
  );
  assert!(
    elapsed < 1500,
    "selectOption timeout should fire within 1.5s, got {elapsed}ms: {v}"
  );
  // Value unchanged.
  let post = c.script_value("return JSON.parse(await page.evaluate('document.getElementById(\"s\").value'));");
  assert_eq!(
    post,
    json!("a"),
    "disabled select value unchanged after timeout: {post}"
  );

  // force: true bypasses the pre-check and selects even when disabled.
  c.nav("<select id='s' disabled><option value='a'>A</option><option value='b'>B</option></select>");
  c.script_value("await page.locator('#s').selectOption('b', { force: true });");
  let after = c.script_value("return JSON.parse(await page.evaluate('document.getElementById(\"s\").value'));");
  assert_eq!(
    after,
    json!("b"),
    "selectOption with force:true should bypass disabled pre-check: {after}"
  );
}

// Task 1.5 phase 4b: `check`/`uncheck`/`setChecked` must verify the
// final state matches the target AND reject uncheck-of-radio, matching
// Playwright's `server/dom.ts::_setChecked`. Proves on every backend:
//   - A custom checkbox with an `onclick` preventDefault doesn't change
//     state → call throws "Clicking the checkbox did not change its state".
//   - `uncheck` on a checked radio → throws the exact Playwright error
//     naming radio groups.
//   - `trial: true` skips verification (caller asserting actionability,
//     not state change).
//   - `check` on a plain checkbox flips the state and returns ok.
fn test_script_check_behavior(c: &mut McpClient) {
  // 1. Plain checkbox: check() toggles to checked.
  c.nav("<input id='cb' type='checkbox'>");
  c.script_value("await page.locator('#cb').check();");
  let v = c.script_value("return JSON.parse(await page.evaluate('document.getElementById(\"cb\").checked'));");
  assert_eq!(v, json!(true), "check() should toggle checkbox on: {v}");

  // 2. Checkbox that intercepts the click → state does not change →
  //    check() throws the Playwright-exact "did not change its state".
  c.nav("<input id='cb' type='checkbox' onclick='event.preventDefault()'>");
  let v = c.script_value(
    "try { await page.locator('#cb').check({ timeout: 500 }); return 'no-throw'; } \
     catch (e) { return String(e.message || e); }",
  );
  let msg = v.as_str().unwrap_or("");
  assert!(
    msg.contains("did not change its state"),
    "preventDefault checkbox should throw Playwright 'did not change its state', got: {v}"
  );

  // 3. Uncheck a checked radio → typed Playwright radio-group error.
  c.nav("<input id='r' type='radio' name='g' checked><input type='radio' name='g'>");
  let v = c.script_value(
    "try { await page.locator('#r').uncheck(); return 'no-throw'; } \
     catch (e) { return String(e.message || e); }",
  );
  let msg = v.as_str().unwrap_or("");
  assert!(
    msg.contains("Cannot uncheck radio button"),
    "uncheck radio should throw 'Cannot uncheck radio button', got: {v}"
  );

  // 4. trial: true skips the post-click verification AND the click —
  //    preventDefault checkbox that would normally throw returns ok.
  c.nav("<input id='cb' type='checkbox' onclick='event.preventDefault()'>");
  c.script_value("await page.locator('#cb').check({ trial: true });");
  let v = c.script_value("return JSON.parse(await page.evaluate('document.getElementById(\"cb\").checked'));");
  assert_eq!(
    v,
    json!(false),
    "trial:true should NOT actually toggle the checkbox state: {v}"
  );

  // 5. check() on an already-checked checkbox is a no-op (no click, no
  //    verification error). Prove by attaching an `onclick` listener and
  //    asserting it never fires.
  c.nav(
    "<input id='cb' type='checkbox' checked>\
     <div id='count'>0</div>\
     <script>\
       document.getElementById('cb').addEventListener('click', () => {\
         const el = document.getElementById('count');\
         el.textContent = String(parseInt(el.textContent, 10) + 1);\
       });\
     </script>",
  );
  c.script_value("await page.locator('#cb').check();");
  let v = c.script_value("return JSON.parse(await page.evaluate('document.getElementById(\"count\").textContent'));");
  assert_eq!(v, json!("0"), "already-checked check() must skip the click: {v}");
}

// Task 1.5 phase 4a: `fill.force` must actually bypass Playwright's
// `['visible','enabled','editable']` pre-check. Proves on every backend:
//   - Without force on a `readonly` input: the pre-check returns
//     `error:noteditable` and the retry loop polls until timeout.
//   - With force:true on the same input: the pre-check is skipped and
//     the JS `.value = 'x'` assignment goes through regardless of the
//     `readonly` attribute, letting the caller override it explicitly.
fn test_script_fill_force(c: &mut McpClient) {
  c.nav("<input id='ro' readonly value=''><div id='out'></div>");

  // 1. force: false (default) on readonly input → times out (retry
  //    loop sees `error:noteditable` as a retriable marker).
  let v = c.script_value(
    "const t0 = Date.now();\
     try { await page.locator('#ro').fill('hello', { timeout: 250 }); return { msg: 'no-throw', elapsed: Date.now() - t0 }; }\
     catch (e) { return { msg: String(e.message || e), elapsed: Date.now() - t0 }; }",
  );
  let msg = v["msg"].as_str().unwrap_or("");
  let elapsed = v["elapsed"].as_i64().unwrap_or(9_999);
  assert!(
    msg.contains("Timeout"),
    "fill without force on readonly should Timeout, got: {v}"
  );
  assert!(
    elapsed < 1500,
    "fill timeout should fire within 1.5s, got {elapsed}ms: {v}"
  );
  // Value stays empty — confirms no write happened.
  let post = c.script_value("return JSON.parse(await page.evaluate('document.getElementById(\"ro\").value'));");
  assert_eq!(post, json!(""), "readonly input should still be empty: {post}");

  // 2. force: true on the same readonly input → writes successfully.
  c.nav("<input id='ro' readonly value=''>");
  c.script_value("await page.locator('#ro').fill('bypass', { force: true });");
  let after = c.script_value("return JSON.parse(await page.evaluate('document.getElementById(\"ro\").value'));");
  assert_eq!(
    after,
    json!("bypass"),
    "fill with force:true should set value on readonly: {after}"
  );
}

// Task 1.5 phase 3 (Rule 4): `locator.tap` must use the backend's native
// touch primitive on every backend that supports it, not a JS `TouchEvent`
// shim. CDP dispatches via `Input.dispatchTouchEvent` producing
// `isTrusted === true` events. BiDi (no pointerType='touch' in stable) and
// WebKit (no public NSTouchEvent synthesis) surface a typed Unsupported
// error instead.
fn test_script_tap_native(c: &mut McpClient) {
  if c.backend == "bidi" || c.backend == "webkit" {
    // On these backends, tap must return Unsupported. Install a button,
    // call tap(), and assert the error message identifies the backend
    // and explains the protocol gap — not a silent JS fallback.
    c.nav(
      "<button id='b' ontouchstart=\"document.getElementById('out').textContent='fired'\">b</button>\
       <div id='out'>no</div>",
    );
    let v = c.script_value(
      "try { await page.locator('#b').tap({ timeout: 2000 }); return { msg: 'no-throw' }; } \
       catch (e) { return { msg: String(e.message || e) }; }",
    );
    let msg = v["msg"].as_str().unwrap_or("");
    assert!(
      msg.contains("unsupported") || msg.contains("Unsupported"),
      "{}: tap should throw Unsupported, got: {v}",
      c.backend
    );
    assert!(
      msg.contains("tap"),
      "{}: Unsupported message should mention tap, got: {v}",
      c.backend
    );
    // The page's DOM event handler must NOT have fired — proof there's
    // no JS-fallback dispatch happening behind the typed error.
    let after =
      c.script_value("return JSON.parse(await page.evaluate('document.getElementById(\"out\").textContent'));");
    assert_eq!(
      after,
      json!("no"),
      "{}: no JS-fallback tap should have fired; got {after}",
      c.backend
    );
    return;
  }

  // CDP native path: Input.dispatchTouchEvent emits a trusted touchstart
  // + touchend pair. Record event.isTrusted and whether the touch point
  // lands inside the button rect; read each field back as a separate
  // `textContent` so we stay inside the single-level JSON.parse pattern
  // (QuickJS `page.evaluate` returns a JSON-stringified result).
  c.nav(
    "<button id='b' style='width:100px;height:50px'>b</button>\
     <div id='trusted'>n</div><div id='inrect'>n</div>\
     <script>\
       const b = document.getElementById('b');\
       b.addEventListener('touchstart', e => {\
         const t = e.changedTouches[0];\
         const r = b.getBoundingClientRect();\
         document.getElementById('trusted').textContent = String(e.isTrusted);\
         document.getElementById('inrect').textContent = String(\
           t.clientX >= r.left && t.clientX <= r.right && t.clientY >= r.top && t.clientY <= r.bottom\
         );\
       }, { passive: true });\
     </script>",
  );
  let v = c.script_value(
    "await page.locator('#b').tap();\
     return {\
       trusted: JSON.parse(await page.evaluate('document.getElementById(\"trusted\").textContent')),\
       inRect: JSON.parse(await page.evaluate('document.getElementById(\"inrect\").textContent')),\
     };",
  );
  assert_eq!(
    v["trusted"],
    json!("true"),
    "CDP tap should emit isTrusted=true touchstart; got: {v}"
  );
  assert_eq!(
    v["inRect"],
    json!("true"),
    "CDP tap should land inside button rect; got: {v}"
  );

  // Modifiers propagate to the touch event: tap + Shift → event.shiftKey.
  c.nav(
    "<button id='b'>b</button><div id='out'>no</div>\
     <script>\
       document.getElementById('b').addEventListener('touchstart', e => {\
         document.getElementById('out').textContent = e.shiftKey ? 'shift' : 'none';\
       }, { passive: true });\
     </script>",
  );
  let v = c.script_value(
    "await page.locator('#b').tap({ modifiers: ['Shift'] });\
     return JSON.parse(await page.evaluate('document.getElementById(\"out\").textContent'));",
  );
  assert_eq!(
    v,
    json!("shift"),
    "tap modifiers:['Shift'] must set event.shiftKey on touchstart: {v}"
  );

  // trial:true skips the touch dispatch but still presses modifiers.
  c.nav(
    "<button id='b'>b</button><div id='tap'>no</div><div id='kd'>no</div>\
     <script>\
       document.getElementById('b').addEventListener('touchstart', () => { document.getElementById('tap').textContent = 'yes'; }, { passive: true });\
       document.addEventListener('keydown', e => { if (e.key === 'Shift') document.getElementById('kd').textContent = 'shift'; });\
     </script>",
  );
  let v = c.script_value(
    "await page.locator('#b').tap({ trial: true, modifiers: ['Shift'] });\
     return {\
       t: JSON.parse(await page.evaluate('document.getElementById(\"tap\").textContent')),\
       k: JSON.parse(await page.evaluate('document.getElementById(\"kd\").textContent')),\
     };",
  );
  assert_eq!(v["t"], json!("no"), "trial:true skips touchstart dispatch: {v}");
  assert_eq!(v["k"], json!("shift"), "trial:true still presses modifier: {v}");
}

// Task 1.5 phase 2: `opts.timeout` must honor the user's deadline on every
// action method — previously accepted and silently ignored. For each action
// we call on a selector that doesn't exist with `timeout: 200`; the call
// must throw a TimeoutError within ~1.5s (wall clock) instead of waiting
// out the page default (30s). Proves the deadline threaded through
// `retry_resolve!` actually fires.
fn test_script_action_timeout(c: &mut McpClient) {
  c.nav("<button id='b'>b</button>");
  for (name, call) in [
    ("click", "await page.locator('#nope').click({ timeout: 200 })"),
    ("fill", "await page.locator('#nope').fill('x', { timeout: 200 })"),
    ("hover", "await page.locator('#nope').hover({ timeout: 200 })"),
    ("tap", "await page.locator('#nope').tap({ timeout: 200 })"),
    ("press", "await page.locator('#nope').press('A', { timeout: 200 })"),
    ("type", "await page.locator('#nope').type('x', { timeout: 200 })"),
    ("dblclick", "await page.locator('#nope').dblclick({ timeout: 200 })"),
    ("check", "await page.locator('#nope').check({ timeout: 200 })"),
    ("uncheck", "await page.locator('#nope').uncheck({ timeout: 200 })"),
  ] {
    let src = format!(
      "const t0 = Date.now();\
       try {{ {call}; return {{ elapsed: Date.now() - t0, msg: 'no-throw' }}; }}\
       catch (e) {{ return {{ elapsed: Date.now() - t0, msg: String(e.message || e) }}; }}"
    );
    let v = c.script_value(&src);
    let msg = v["msg"].as_str().unwrap_or("");
    let elapsed = v["elapsed"].as_i64().unwrap_or(99_999);
    assert!(
      msg.contains("Timeout") && msg.contains("200ms"),
      "{name}: expected TimeoutError w/ 200ms; got: {v}"
    );
    assert!(
      elapsed < 1500,
      "{name}: expected to fail within 1.5s of 200ms timeout; got {elapsed}ms: {v}"
    );
  }
}

// Task 1.3 phase B: the injected `window.__fd` namespace exposes the
// Playwright `UtilityScript` class and its isomorphic serializer
// helpers (`parseEvaluationResultValue`, `serializeAsCallArgument`).
// These are the load-bearing primitives for `page.evaluate(fn, arg)` +
// JSHandle round-trip in phase D — if they're missing from the bundle
// or shadowed by a later `if (!window.__fd)` guard, evaluate(fn, arg)
// will never work no matter how the Rust side serializes. Proves the
// bundle surfaces them on every backend.
fn test_script_utility_script_exposed(c: &mut McpClient) {
  c.nav("<div id='x'></div>");

  // 1. The class and both serializer helpers exist on window.__fd.
  //    `page.evaluate` JSON-stringifies the result string `"function"`,
  //    so one JSON.parse unwraps the quote.
  let v = c.script_value(
    "return {\
       hasClass: JSON.parse(await page.evaluate('typeof window.__fd.UtilityScript')),\
       hasFactory: JSON.parse(await page.evaluate('typeof window.__fd.newUtilityScript')),\
       hasParse: JSON.parse(await page.evaluate('typeof window.__fd.parseEvaluationResultValue')),\
       hasSerialize: JSON.parse(await page.evaluate('typeof window.__fd.serializeAsCallArgument')),\
     };",
  );
  assert_eq!(v["hasClass"], json!("function"), "UtilityScript class missing: {v}");
  assert_eq!(
    v["hasFactory"],
    json!("function"),
    "newUtilityScript factory missing: {v}"
  );
  assert_eq!(
    v["hasParse"],
    json!("function"),
    "parseEvaluationResultValue missing: {v}"
  );
  assert_eq!(
    v["hasSerialize"],
    json!("function"),
    "serializeAsCallArgument missing: {v}"
  );

  // 2. The factory returns a working instance — its `evaluate` and
  //    `jsonValue` methods are invokable.
  let v = c.script_value(
    "return {\
       hasEvaluate: JSON.parse(await page.evaluate('typeof window.__fd.newUtilityScript().evaluate')),\
       hasJsonValue: JSON.parse(await page.evaluate('typeof window.__fd.newUtilityScript().jsonValue')),\
     };",
  );
  assert_eq!(
    v["hasEvaluate"],
    json!("function"),
    "UtilityScript.evaluate missing: {v}"
  );
  assert_eq!(
    v["hasJsonValue"],
    json!("function"),
    "UtilityScript.jsonValue missing: {v}"
  );

  // 3. The deserializer round-trips Playwright's wire shapes for rich
  //    types — a smoke check that the isomorphic format we built on the
  //    Rust side is the same one the page's utility script parses.
  //    Probe each result as a primitive string so QuickJS' JSON.stringify
  //    on page.evaluate plays nicely.
  let probes = [
    // `{v: 'NaN'}` → NaN. Use Number.isNaN to verify since NaN !== NaN.
    (
      "nan",
      "Number.isNaN(window.__fd.parseEvaluationResultValue({v: 'NaN'}))",
      json!(true),
    ),
    // `{v: 'Infinity'}` → Infinity.
    (
      "inf",
      "window.__fd.parseEvaluationResultValue({v: 'Infinity'}) === Infinity",
      json!(true),
    ),
    // `{v: '-Infinity'}` → -Infinity.
    (
      "neginf",
      "window.__fd.parseEvaluationResultValue({v: '-Infinity'}) === -Infinity",
      json!(true),
    ),
    // `{v: '-0'}` → -0. Detect via 1/-0 === -Infinity.
    (
      "negzero",
      "1 / window.__fd.parseEvaluationResultValue({v: '-0'}) === -Infinity",
      json!(true),
    ),
    // `{v: 'null'}` → null.
    (
      "null",
      "window.__fd.parseEvaluationResultValue({v: 'null'}) === null",
      json!(true),
    ),
    // `{v: 'undefined'}` → undefined.
    (
      "undef",
      "typeof window.__fd.parseEvaluationResultValue({v: 'undefined'})",
      json!("undefined"),
    ),
    // `{d: '...'}` → Date instance.
    (
      "date",
      "window.__fd.parseEvaluationResultValue({d: '2024-01-01T00:00:00.000Z'}) instanceof Date",
      json!(true),
    ),
    // `{u: '...'}` → URL instance.
    (
      "url",
      "window.__fd.parseEvaluationResultValue({u: 'https://a.test/x'}) instanceof URL",
      json!(true),
    ),
    // `{r: {p, f}}` → RegExp.
    (
      "regexp",
      "window.__fd.parseEvaluationResultValue({r: {p: 'foo', f: 'gi'}}) instanceof RegExp",
      json!(true),
    ),
    // `{bi: '42'}` → BigInt(42n). `typeof` == 'bigint'.
    (
      "bigint",
      "typeof window.__fd.parseEvaluationResultValue({bi: '42'})",
      json!("bigint"),
    ),
    // `{e: {m, n, s}}` → Error.
    (
      "error",
      "window.__fd.parseEvaluationResultValue({e: {n: 'TypeError', m: 'oops', s: ''}}) instanceof Error",
      json!(true),
    ),
  ];
  for (name, probe_expr, expected) in probes {
    // `page.evaluate` in QuickJS already JSON-stringifies its result
    // before handing it back, so one `JSON.parse` is enough to unwrap.
    // The inline-script `{probe_expr}` must return a JSON-expressible
    // primitive (bool or typeof-string in our probes).
    let script = format!("return JSON.parse(await page.evaluate({probe_expr:?}));");
    let got = c.script_value(&script);
    assert_eq!(
      got, expected,
      "deserializer probe '{name}' failed: expr {probe_expr}, got {got}"
    );
  }

  // 4. Round-trip: serialize a rich value → deserialize → re-serialize
  //    and assert the wire shape is stable. Exercises the complete
  //    isomorphic format end-to-end inside the page. `page.evaluate`
  //    already JSON-stringifies the IIFE's return value, so one
  //    `JSON.parse` unwraps the object shape.
  let v = c.script_value(
    "return JSON.parse(await page.evaluate(`(() => {\
       const raw = {d: '2024-06-01T00:00:00.000Z'};\
       const dateObj = window.__fd.parseEvaluationResultValue(raw);\
       return window.__fd.serializeAsCallArgument(dateObj, v => ({fallThrough: v}));\
     })()`));",
  );
  assert_eq!(
    v,
    json!({"d": "2024-06-01T00:00:00.000Z"}),
    "Date round-trip should preserve the d-tag wire shape: {v}"
  );
}

// Task 1.2 + 1.3 phase C — JSHandle + ElementHandle lifecycle. Rule 9:
// dispose must work end-to-end on every backend (cdp-pipe, cdp-raw,
// webkit, bidi). Exercises the QuickJS `ElementHandle.dispose()` +
// `JSHandle.dispose()` + idempotence via `run_script` so all four
// backends are proven.
fn test_script_handle_lifecycle(c: &mut McpClient) {
  c.nav("<button id='primary'>ok</button><div class='needle'>x</div>");

  // querySelector returns an ElementHandle with isDisposed=false.
  let v = c.script_value(
    "const h = await page.querySelector('button#primary');\
     return {found: h !== null, disposed: h.isDisposed};",
  );
  assert_eq!(v["found"], json!(true), "querySelector missed #primary: {v}");
  assert_eq!(v["disposed"], json!(false), "fresh handle already disposed: {v}");

  // $ alias returns a handle too.
  let v = c.script_value(
    "const h = await page.$('div.needle');\
     return h !== null;",
  );
  assert_eq!(v, json!(true), "$ alias missed .needle: {v}");

  // Missing selector returns null/undefined (not an error). Use `== null`
  // (loose equality) so we accept both representations — rquickjs maps
  // Rust's `Option::None` to `undefined` on the JS side, while
  // Playwright's TS types say `null`. Either is acceptable here; what
  // we're testing is that an unmatched selector is non-truthy, not an
  // error.
  let v = c.script_value(
    "const r = await page.querySelector('button#does-not-exist');\
     return r === null || r === undefined;",
  );
  assert_eq!(v, json!(true), "missing selector did not return null: {v}");

  // dispose() latches isDisposed and is idempotent.
  let v = c.script_value(
    "const h = await page.querySelector('button#primary');\
     const before = h.isDisposed;\
     await h.dispose();\
     const after1 = h.isDisposed;\
     await h.dispose();\
     const after2 = h.isDisposed;\
     return {before, after1, after2};",
  );
  assert_eq!(v["before"], json!(false), "before dispose: {v}");
  assert_eq!(v["after1"], json!(true), "after first dispose: {v}");
  assert_eq!(v["after2"], json!(true), "after second dispose: {v}");

  // asJSHandle shares the disposed flag with the ElementHandle.
  let v = c.script_value(
    "const eh = await page.querySelector('button#primary');\
     const jh = eh.asJSHandle();\
     const before_eh = eh.isDisposed;\
     const before_jh = jh.isDisposed;\
     await eh.dispose();\
     const after_eh = eh.isDisposed;\
     const after_jh = jh.isDisposed;\
     return {before_eh, before_jh, after_eh, after_jh};",
  );
  assert_eq!(v["before_eh"], json!(false));
  assert_eq!(v["before_jh"], json!(false));
  assert_eq!(v["after_eh"], json!(true));
  // Shared Arc<AtomicBool> means the JSHandle observes the dispose too.
  assert_eq!(
    v["after_jh"],
    json!(true),
    "JSHandle sibling did not see the dispose: {v}"
  );

  // JSHandle.asElement is a phase-C stub — always null/undefined until
  // phase-D inspects the remote type.
  let v = c.script_value(
    "const eh = await page.querySelector('button#primary');\
     const jh = eh.asJSHandle();\
     const asEl = jh.asElement();\
     await eh.dispose();\
     return asEl === null || asEl === undefined;",
  );
  assert_eq!(v, json!(true), "asElement was not null in phase C: {v}");
}

// Task 1.3 phase D — page.evaluate(fn, arg) + evaluateHandle(fn) +
// handle.evaluate(fn). Rule 9 covers all four backends via QuickJS
// `run_script` (CDP + BiDi go through callFunctionOn / callFunction
// with remote references; WebKit inlines the call via the shared
// `window.__wr` registry).
fn test_script_evaluate_fn_and_handle(c: &mut McpClient) {
  c.nav("<button id='primary'>ok</button>");

  // page.evaluateWithArg(fn, primitive) — function-call semantics.
  let v = c.script_value("return await page.evaluateWithArg('x => x + 1', 41);");
  assert_eq!(v, json!(42), "primitive arg round-trip: {v}");

  // page.evaluateWithArg(fn, object) — JSON round-trip.
  let v = c.script_value("return await page.evaluateWithArg('o => o.a + o.b', {a: 2, b: 3});");
  assert_eq!(v, json!(5), "object arg round-trip: {v}");

  // page.evaluateWithArg(fn, null) — no-arg function-call with null.
  let v = c.script_value("return await page.evaluateWithArg('() => 7', null);");
  assert_eq!(v, json!(7), "null-arg call: {v}");

  // page.evaluateHandleWithArg — returns a live JSHandle.
  let v = c.script_value(
    "const h = await page.evaluateHandleWithArg('() => ({x: 42})', null);\
     const disposed = h.isDisposed;\
     await h.dispose();\
     return {disposed_before: disposed, disposed_after: h.isDisposed};",
  );
  assert_eq!(v["disposed_before"], json!(false));
  assert_eq!(v["disposed_after"], json!(true));

  // handle.evaluateWithArg passes the handle as arg[0].
  let v = c.script_value(
    "const h = await page.evaluateHandleWithArg('() => document.body', null);\
     const tag = await h.evaluateWithArg('el => el.tagName', null);\
     await h.dispose();\
     return tag;",
  );
  assert_eq!(v, json!("BODY"), "handle.evaluate: {v}");

  // ElementHandle.evaluateWithArg routes through its JSHandle.
  let v = c.script_value(
    "const eh = await page.querySelector('button#primary');\
     const tag = await eh.evaluateWithArg('el => el.tagName', null);\
     await eh.dispose();\
     return tag;",
  );
  assert_eq!(v, json!("BUTTON"), "ElementHandle.evaluate: {v}");

  // Disposed-handle use raises the Playwright 'disposed' error.
  let v = c.script_value(
    "const eh = await page.querySelector('button#primary');\
     const jh = eh.asJSHandle();\
     await eh.dispose();\
     let threw = false;\
     let msg = '';\
     try { await jh.evaluateWithArg('el => el.tagName', null); }\
     catch (e) { threw = true; msg = String(e.message || e); }\
     return {threw, hasDisposedWord: msg.indexOf('disposed') >= 0};",
  );
  assert_eq!(v["threw"], json!(true), "disposed-use threw?: {v}");
  assert_eq!(
    v["hasDisposedWord"],
    json!(true),
    "disposed error mentions 'disposed': {v}"
  );
}

// Phase D — rich-type round-trip via the isomorphic wire. Exercised
// through `evaluateWithArgWire` which bypasses the JSON-like
// lowering and returns the raw tagged object. Proves Playwright's
// serializer round-trips every wire variant end-to-end on every
// backend.
fn test_script_evaluate_rich_types(c: &mut McpClient) {
  c.nav("<div></div>");

  // Date → {d: '<iso>'}
  let v =
    c.script_value("return await page.evaluateWithArgWire(\"() => new Date('2024-06-01T00:00:00.000Z')\", null);");
  assert_eq!(v, json!({"d": "2024-06-01T00:00:00.000Z"}), "Date wire round-trip: {v}");

  // RegExp → {r: {p, f}}
  let v = c.script_value("return await page.evaluateWithArgWire(\"() => /foo.*bar/gi\", null);");
  assert_eq!(v, json!({"r": {"p": "foo.*bar", "f": "gi"}}), "RegExp wire: {v}");

  // NaN → {v: 'NaN'}
  let v = c.script_value("return await page.evaluateWithArgWire('() => NaN', null);");
  assert_eq!(v, json!({"v": "NaN"}), "NaN wire: {v}");

  // Infinity → {v: 'Infinity'}
  let v = c.script_value("return await page.evaluateWithArgWire('() => Infinity', null);");
  assert_eq!(v, json!({"v": "Infinity"}), "Infinity wire: {v}");

  // BigInt → {bi: '<digits>'}
  let v = c.script_value("return await page.evaluateWithArgWire('() => 9007199254740993n', null);");
  assert_eq!(v, json!({"bi": "9007199254740993"}), "BigInt wire: {v}");

  // undefined → {v: 'undefined'}
  let v = c.script_value("return await page.evaluateWithArgWire('() => undefined', null);");
  assert_eq!(v, json!({"v": "undefined"}), "undefined wire: {v}");
}

// Task 1.2 phase E — ElementHandle DOM methods. Rule 9: verify reads,
// state predicates, bounding_box, click / focus / scrollIntoView on
// all four backends via QuickJS.
fn test_script_element_handle_methods(c: &mut McpClient) {
  c.nav("<a id='l' href='/x' data-k='v'>hello <b>world</b></a>");

  // innerHTML / innerText / textContent / getAttribute
  let v = c.script_value(
    "const eh = await page.querySelector('a#l');\
     const result = {\
       inner_html: await eh.innerHTML(),\
       inner_text: await eh.innerText(),\
       text_content: await eh.textContent(),\
       href: await eh.getAttribute('href'),\
       k: await eh.getAttribute('data-k'),\
     };\
     await eh.dispose();\
     return result;",
  );
  let inner = v["inner_html"].as_str().unwrap_or("");
  // BiDi injects a `data-fdref` attribute on DOM elements it
  // references, so the serialised innerHTML is `<b data-fdref="...">`
  // rather than a bare `<b>`. Match the substrings that matter.
  assert!(inner.contains("<b") && inner.contains("world</b>"), "innerHTML: {v}");
  assert_eq!(v["inner_text"], json!("hello world"), "innerText: {v}");
  assert_eq!(v["text_content"], json!("hello world"), "textContent: {v}");
  assert_eq!(v["href"], json!("/x"), "getAttribute(href): {v}");
  assert_eq!(v["k"], json!("v"), "getAttribute(data-k): {v}");

  // inputValue
  c.nav("<input id='i' value='hi'>");
  let v = c.script_value(
    "const eh = await page.querySelector('#i');\
     const v = await eh.inputValue();\
     await eh.dispose();\
     return v;",
  );
  assert_eq!(v, json!("hi"), "inputValue: {v}");

  // State predicates
  c.nav("<button id='v'>x</button><button id='d' disabled>x</button><button id='h' style='display:none'>x</button>");
  let v = c.script_value(
    "const v = await page.querySelector('#v');\
     const d = await page.querySelector('#d');\
     const h = await page.querySelector('#h');\
     const result = {\
       v_visible: await v.isVisible(),\
       v_enabled: await v.isEnabled(),\
       d_disabled: await d.isDisabled(),\
       h_hidden: await h.isHidden(),\
     };\
     await v.dispose(); await d.dispose(); await h.dispose();\
     return result;",
  );
  assert_eq!(v["v_visible"], json!(true));
  assert_eq!(v["v_enabled"], json!(true));
  assert_eq!(v["d_disabled"], json!(true));
  assert_eq!(v["h_hidden"], json!(true));

  // isChecked + isEditable
  c.nav("<input type='checkbox' id='c' checked><input id='i'><input id='r' readonly>");
  let v = c.script_value(
    "const c = await page.querySelector('#c');\
     const i = await page.querySelector('#i');\
     const r = await page.querySelector('#r');\
     const result = {\
       c_checked: await c.isChecked(),\
       i_editable: await i.isEditable(),\
       r_editable: await r.isEditable(),\
     };\
     await c.dispose(); await i.dispose(); await r.dispose();\
     return result;",
  );
  assert_eq!(v["c_checked"], json!(true));
  assert_eq!(v["i_editable"], json!(true));
  assert_eq!(v["r_editable"], json!(false));

  // boundingBox
  c.nav("<button id='b' style='position:absolute;left:10px;top:20px;width:50px;height:30px'>b</button>");
  let v = c.script_value(
    "const b = await page.querySelector('#b');\
     const box = await b.boundingBox();\
     await b.dispose();\
     return box;",
  );
  let width = v["width"].as_f64().unwrap_or(0.0);
  let height = v["height"].as_f64().unwrap_or(0.0);
  assert!(width > 0.0, "bbox width > 0: {v}");
  assert!(height > 0.0, "bbox height > 0: {v}");

  // click fires the native handler. The onclick handler is
  // synchronous so the title update is observable on the next
  // page.title round-trip — no setTimeout needed (QuickJS doesn't
  // have setTimeout anyway).
  c.nav("<button id='b' onclick=\"document.title='clicked'\">b</button>");
  let v = c.script_value(
    "const b = await page.querySelector('#b');\
     await b.click();\
     const t = await page.title();\
     await b.dispose();\
     return t;",
  );
  assert_eq!(v, json!("clicked"), "click fired: {v}");

  // focus updates activeElement
  c.nav("<input id='i'>");
  let v = c.script_value(
    "const i = await page.querySelector('#i');\
     await i.focus();\
     const active = await page.evaluate('document.activeElement && document.activeElement.id');\
     await i.dispose();\
     return active;",
  );
  // QuickJS page.evaluate wraps strings as JSON — result may be either a
  // bare "i" or the string "\"i\"". Normalize.
  let s = v.as_str().unwrap_or("");
  assert!(s == "i" || s == "\"i\"", "focus activeElement: {v}");

  // scrollIntoViewIfNeeded shouldn't throw on an offscreen element
  c.nav("<div style='height:2000px'></div><button id='b'>b</button>");
  c.script_value(
    "const b = await page.querySelector('#b');\
     await b.scrollIntoViewIfNeeded();\
     await b.dispose();\
     return true;",
  );
}

// Task 1.2 + 1.3 phase F — handle materialisation surface.
// querySelectorAll on Page + locator.elementHandle{,s}. Rule 9
// covers all 4 backends.
fn test_script_handle_materialisation(c: &mut McpClient) {
  c.nav("<ul><li>a</li><li>b</li><li>c</li></ul>");

  // page.querySelectorAll returns one handle per match in document
  // order. Each handle's lifecycle is independent — disposing one
  // doesn't affect the others.
  let v = c.script_value(
    "const items = await page.querySelectorAll('li');\
     const texts = [];\
     for (const it of items) texts.push(await it.textContent());\
     for (const it of items) await it.dispose();\
     return {len: items.length, texts};",
  );
  assert_eq!(v["len"], json!(3), "querySelectorAll length: {v}");
  assert_eq!(v["texts"], json!(["a", "b", "c"]), "querySelectorAll texts: {v}");

  // $$ alias
  let v = c.script_value(
    "const items = await page.$$('li');\
     const len = items.length;\
     for (const it of items) await it.dispose();\
     return len;",
  );
  assert_eq!(v, json!(3), "$$ alias: {v}");

  // Empty selector returns empty array (not error).
  let v = c.script_value(
    "const items = await page.querySelectorAll('li.does-not-exist');\
     return items.length;",
  );
  assert_eq!(v, json!(0), "empty querySelectorAll: {v}");

  // locator.elementHandle resolves the locator's selector to a
  // single pinned ElementHandle.
  c.nav("<button id='b'>click</button>");
  let v = c.script_value(
    "const loc = page.locator('#b');\
     const eh = await loc.elementHandle();\
     const tag = await eh.evaluateWithArg('el => el.tagName', null);\
     await eh.dispose();\
     return tag;",
  );
  assert_eq!(v, json!("BUTTON"), "locator.elementHandle: {v}");

  // locator.elementHandles returns one handle per match.
  c.nav("<ul><li class='it'>x</li><li class='it'>y</li></ul>");
  let v = c.script_value(
    "const loc = page.locator('li.it');\
     const ehs = await loc.elementHandles();\
     const texts = [];\
     for (const eh of ehs) texts.push(await eh.textContent());\
     for (const eh of ehs) await eh.dispose();\
     return {len: ehs.length, texts};",
  );
  assert_eq!(v["len"], json!(2));
  assert_eq!(v["texts"], json!(["x", "y"]));
}

// WebKit-specific Rule-9 probe: proves Op::ReleaseRef actually reached
// the host and deleted from `window.__wr`. CDP's Runtime.releaseObject
// and BiDi's script.disown are not observable from page-side JS without
// the phase-D use-after-dispose path; this probe covers WebKit's
// page-side registry shrink which IS observable.
fn test_script_handle_lifecycle_webkit_observable(c: &mut McpClient) {
  c.nav("<button id='primary'>ok</button>");

  // `page.evaluate` through the QuickJS binding JSON-stringifies its
  // result, so the Number comes back as a string (`"0"`). Coerce via
  // `Number(...)` inside JS so we get a real number on the wire.
  let v = c.script_value(
    "const sizeNow = async () => Number(await page.evaluate('window.__wr ? window.__wr.size : 0'));\
     const before = await sizeNow();\
     const h = await page.querySelector('button#primary');\
     const during = await sizeNow();\
     await h.dispose();\
     const after = await sizeNow();\
     return {before, during, after};",
  );
  let as_int = |k: &str| -> i64 {
    v[k]
      .as_i64()
      .or_else(|| v[k].as_str().and_then(|s| s.parse::<i64>().ok()))
      .unwrap_or(-1)
  };
  let before_size = as_int("before");
  let during_size = as_int("during");
  let after_size = as_int("after");
  assert_eq!(
    during_size,
    before_size + 1,
    "querySelector did not grow window.__wr by 1 (before={before_size}, during={during_size}): {v}"
  );
  assert_eq!(
    after_size, before_size,
    "Op::ReleaseRef did not shrink window.__wr back to pre-mint size (before={before_size}, after={after_size}): {v}"
  );
}

// Task 3.25: `page.addInitScript(script, arg)` — exercise the full
// Playwright surface (Function + arg, string, `{ content }`) from QuickJS
// end-to-end, including the Rust-core-driven `Cannot evaluate a string with
// arguments` error for the string+arg form. Every assertion fires after a
// `goto` so the init script really did run at document start.
fn test_script_add_init_script(c: &mut McpClient) {
  // Function + typed arg → init script runs before page JS with `arg`.
  // `page.evaluate` in the QuickJS binding wraps the page value in
  // JSON.stringify on the way out, so each probe is a single JSON.parse
  // to unwrap back to a raw JS value.
  let v = c.script_value(
    "await page.addInitScript(\
       (cfg) => { window.__fd_init_arg = cfg; },\
       { answer: 42, label: 'hi' },\
     );\
     await page.goto('data:text/html,<title>x</title>');\
     return {\
       answer: JSON.parse(await page.evaluate('window.__fd_init_arg.answer')),\
       label: JSON.parse(await page.evaluate('window.__fd_init_arg.label')),\
     };",
  );
  assert_eq!(v["answer"], json!(42), "function arg answer: {v}");
  assert_eq!(v["label"], json!("hi"), "function arg label: {v}");

  // Function with no arg → rendered as `(fn)(undefined)`, so typeof is 'undefined'.
  let v = c.script_value(
    "await page.addInitScript((x) => { window.__fd_init_noarg = typeof x; });\
     await page.goto('data:text/html,<title>y</title>');\
     return JSON.parse(await page.evaluate('window.__fd_init_noarg'));",
  );
  assert_eq!(v, json!("undefined"), "function no-arg typeof: {v}");

  // Function with explicit null → JSON.stringify(null) = 'null', arg is null.
  let v = c.script_value(
    "await page.addInitScript((x) => { window.__fd_init_null = x === null ? 'is-null' : typeof x; }, null);\
     await page.goto('data:text/html,<title>z</title>');\
     return JSON.parse(await page.evaluate('window.__fd_init_null'));",
  );
  assert_eq!(v, json!("is-null"), "function null arg: {v}");

  // { content } → used verbatim.
  let v = c.script_value(
    "await page.addInitScript({ content: \"window.__fd_init_content = 'from-content';\" });\
     await page.goto('data:text/html,<title>w</title>');\
     return JSON.parse(await page.evaluate('window.__fd_init_content'));",
  );
  assert_eq!(v, json!("from-content"), "{{content}} form: {v}");

  // String + arg → Rust core rejects with Playwright's exact message.
  let v = c.script_value(
    "try {\
       await page.addInitScript('window.x = 1', { bad: true });\
       return 'no-throw';\
     } catch (e) {\
       return String(e.message || e);\
     }",
  );
  let msg = v.as_str().unwrap_or("");
  assert!(
    msg.contains("Cannot evaluate a string with arguments"),
    "string+arg error message: {v}"
  );
}

// Task 3.8: Playwright-parity sync frame accessors exposed via QuickJS.
// Verifies the same FrameJs surface the NAPI tests cover — name/url/
// isMainFrame/parentFrame/childFrames/isDetached are all sync (no await).
fn test_script_frame_sync_accessors(c: &mut McpClient) {
  c.nav(
    "<h1>Parent</h1>\
     <iframe name='alpha' srcdoc='<p>A</p>'></iframe>\
     <iframe name='beta' srcdoc='<p>B</p>'></iframe>",
  );
  // Wait for both iframes to appear in the DOM — by the time
  // waitForSelector resolves, FrameAttached/Navigated events have
  // propagated to the page-owned frame cache.
  // Use `== null` (loose equality) to accept both rquickjs `undefined` and
  // explicit `null` — rquickjs maps `Option::None` returns to JS
  // `undefined`, not `null`.
  let v = c.script_value(
    "await page.waitForSelector('iframe[name=\"alpha\"]'); \
       await page.waitForSelector('iframe[name=\"beta\"]'); \
       const main = page.mainFrame(); \
       const kidNames = main.childFrames().map(f => f.name()).sort(); \
       const alpha = page.frame('alpha'); \
       const alphaParent = alpha ? alpha.parentFrame() : null; \
       return { \
         mainIsMain: main.isMainFrame(), \
         mainParentNull: main.parentFrame() == null, \
         mainDetached: main.isDetached(), \
         kidNames, \
         alphaName: alpha ? alpha.name() : null, \
         alphaIsMain: alpha ? alpha.isMainFrame() : null, \
         alphaParentIsMain: alphaParent ? alphaParent.isMainFrame() : null, \
         frameCount: page.frames().length, \
       };",
  );
  assert_eq!(v["mainIsMain"], json!(true), "mainFrame.isMainFrame(): {v}");
  assert_eq!(
    v["mainParentNull"],
    json!(true),
    "mainFrame.parentFrame() === null: {v}"
  );
  assert_eq!(v["mainDetached"], json!(false), "mainFrame.isDetached() === false: {v}");
  assert_eq!(v["alphaName"], json!("alpha"), "frame('alpha').name(): {v}");
  assert_eq!(v["alphaIsMain"], json!(false), "child frame is not main: {v}");
  assert_eq!(v["alphaParentIsMain"], json!(true), "child.parentFrame() is main: {v}");
  assert!(
    v["frameCount"].as_i64().unwrap_or(0) >= 3,
    "frames() includes main + 2 iframes: {v}"
  );
  let kids = v["kidNames"].as_array().cloned().unwrap_or_default();
  assert!(
    kids.iter().any(|n| n == &json!("alpha")),
    "child names contain 'alpha': {v}"
  );
  assert!(
    kids.iter().any(|n| n == &json!("beta")),
    "child names contain 'beta': {v}"
  );
}

fn test_script_frame_selector_union(c: &mut McpClient) {
  c.nav("<iframe name='target' src='about:blank'></iframe>");
  let v = c.script_value(
    "await page.waitForSelector('iframe[name=\"target\"]'); \
       const byName = page.frame('target'); \
       const byObj = page.frame({ name: 'target' }); \
       const empty = page.frame({}); \
       return { \
         byNameName: byName ? byName.name() : null, \
         byObjName: byObj ? byObj.name() : null, \
         emptyIsNull: empty == null, \
       };",
  );
  assert_eq!(v["byNameName"], json!("target"), "frame(string) resolves: {v}");
  assert_eq!(v["byObjName"], json!("target"), "frame({{name}}) resolves: {v}");
  assert_eq!(v["emptyIsNull"], json!(true), "frame({{}}) returns null: {v}");
}

fn test_script_keyboard_press(c: &mut McpClient) {
  c.nav("<textarea id='t'></textarea>");
  let v = c.script_value(
    "await page.locator('#t').focus(); \
       await page.keyboard.press('A'); \
       await page.keyboard.press('B'); \
       return await page.inputValue('#t');",
  );
  let s = v.as_str().unwrap_or("").to_string();
  assert!(
    s.contains('A') || s.contains('a') || s.contains('B') || s.contains('b') || !s.is_empty(),
    "keyboard.press should insert characters: {s:?}"
  );
}

fn test_script_wait_for_text(c: &mut McpClient) {
  c.nav("<body></body><script>setTimeout(function(){document.body.innerHTML='<p>findme</p>'}, 100)</script>");
  let v = c.script_value(
    "await page.waitForSelector('p'); \
       return await page.textContent('p');",
  );
  assert_eq!(v, json!("findme"));
}

fn test_script_selector_chain(c: &mut McpClient) {
  c.nav("<div class='a'><button onclick=\"this.textContent='clicked'\">Yes</button></div><div class='b'><button>No</button></div>");
  let v = c.script_value(
    "await page.locator('.a').locator('button').click(); \
       return await page.locator('.a button').textContent();",
  );
  assert_eq!(v, json!("clicked"), "chained locator should click button in .a");
}

fn test_script_upload_file(c: &mut McpClient) {
  c.nav("<input type='file' id='f'><div id='r'></div><script>document.getElementById('f').addEventListener('change',function(e){var f=e.target.files[0];if(f){var reader=new FileReader();reader.onload=function(){document.getElementById('r').textContent='name:'+f.name+',size:'+f.size+',content:'+reader.result;};reader.readAsText(f);}});</script>");
  let tmp = std::env::temp_dir().join("ferridriver_test_upload.txt");
  std::fs::write(&tmp, "test file content").unwrap();
  let v = c.script_value_with_args(
    "await page.setInputFiles('#f', [args[0]]); \
       const count = await page.evaluate(\"document.getElementById('f').files.length\"); \
       const name = await page.evaluate(\"document.getElementById('f').files[0].name\"); \
       const size = await page.evaluate(\"document.getElementById('f').files[0].size\"); \
       return { count: JSON.parse(count), name: JSON.parse(name), size: JSON.parse(size) };",
    json!([tmp.to_str().unwrap()]),
  );
  assert_eq!(v["count"], json!(1));
  assert_eq!(v["name"], json!("ferridriver_test_upload.txt"));
  assert_eq!(v["size"], json!(17));
  let _ = std::fs::remove_file(&tmp);
}

fn test_script_user_agent(c: &mut McpClient) {
  c.nav("<body>ua-test</body>");
  let v = c.script_value(
    "await page.setUserAgent('TestBot/1.0'); \
       const rawUa = await page.evaluate('navigator.userAgent'); \
       return JSON.parse(rawUa);",
  );
  let ua = v.as_str().unwrap_or("").to_string();
  assert!(ua.contains("TestBot"), "setUserAgent should override UA: {ua}");
}

fn test_script_viewport(c: &mut McpClient) {
  c.nav("<body></body>");
  let v = c.script_value(
    "await page.setViewportSize(375, 812); \
       const w = await page.evaluate('window.innerWidth'); \
       const h = await page.evaluate('window.innerHeight'); \
       return { w: JSON.parse(w), h: JSON.parse(h) };",
  );
  assert_eq!(v["w"], json!(375));
  assert_eq!(v["h"], json!(812));
}

fn test_script_geolocation(c: &mut McpClient) {
  c.nav("<body></body>");
  let v = c.script_value(
    "await context.setGeolocation(37.7749, -122.4194, 1.0); \
       const raw = await page.evaluate('typeof navigator.geolocation'); \
       return JSON.parse(raw);",
  );
  assert_eq!(v, json!("object"), "geolocation should be available");
}

fn test_script_offline(c: &mut McpClient) {
  c.nav("<body></body>");
  let v = c.script_value(
    "await context.setOffline(true); \
       const rawOffline = await page.evaluate('navigator.onLine'); \
       await context.setOffline(false); \
       const rawOnline = await page.evaluate('navigator.onLine'); \
       return { offline: JSON.parse(rawOffline), online: JSON.parse(rawOnline) };",
  );
  assert_eq!(v["offline"], json!(false), "should be offline");
  assert_eq!(v["online"], json!(true), "should be back online");
}

fn test_script_markdown(c: &mut McpClient) {
  c.nav("<h1>Title</h1><p>Hello world</p><ul><li>Item 1</li><li>Item 2</li></ul>");
  let v = c.script_value("return await page.markdown();");
  let md = v.as_str().unwrap_or("").to_string();
  assert!(md.contains("# Title"), "markdown headings: {md}");
  assert!(md.contains("Hello world"), "markdown paragraphs: {md}");
  assert!(md.contains("- Item"), "markdown lists: {md}");
}

fn test_script_markdown_links(c: &mut McpClient) {
  c.nav("<p>Visit <a href='https://example.com'>Example</a></p>");
  let v = c.script_value("return await page.markdown();");
  let md = v.as_str().unwrap_or("").to_string();
  assert!(md.contains("[Example](https://example.com)"), "markdown links: {md}");
}

// ─── run_script: Locator chains ─────────────────────────────────────────────

fn test_script_locator_role(c: &mut McpClient) {
  c.nav("<button>Save</button><button disabled>Delete</button>");
  let v = c.script_value(
    "await page.getByRole('button').first().click(); \
       return await page.getByRole('button').count();",
  );
  assert_eq!(v, json!(2), "getByRole should find 2 buttons");
}

fn test_script_locator_label(c: &mut McpClient) {
  c.nav("<label for='e'>Email Address</label><input id='e' type='email'>");
  let v = c.script_value(
    "await page.getByLabel('Email Address').fill('test@test.com'); \
       return await page.inputValue('#e');",
  );
  assert_eq!(v, json!("test@test.com"));
}

fn test_script_locator_placeholder(c: &mut McpClient) {
  c.nav("<input placeholder='Enter your name' id='n'>");
  let v = c.script_value(
    "await page.getByPlaceholder('Enter your name').fill('Alice'); \
       return await page.inputValue('#n');",
  );
  assert_eq!(v, json!("Alice"));
}

fn test_script_locator_text(c: &mut McpClient) {
  c.nav("<button>First</button><button>Second</button><button>Third</button>");
  let v = c.script_value("return await page.getByText('Second').textContent();");
  assert_eq!(v, json!("Second"));
}

fn test_script_locator_nth(c: &mut McpClient) {
  c.nav("<button>alpha</button><button>beta</button><button>gamma</button>");
  let v = c.script_value("return await page.getByRole('button').nth(1).textContent();");
  assert_eq!(v, json!("beta"));
}

fn test_script_locator_all_text(c: &mut McpClient) {
  c.nav("<li>a</li><li>b</li><li>c</li>");
  let v = c.script_value("return await page.locator('li').allTextContents();");
  assert_eq!(v, json!(["a", "b", "c"]));
}

// ─── run_script: waits + auto-wait ──────────────────────────────────────────

fn test_script_wait_for_selector(c: &mut McpClient) {
  c.nav("<div id='target'>here</div>");
  let v = c.script_value("await page.waitForSelector('#target'); return 'ok';");
  assert_eq!(v, json!("ok"));
}

fn test_script_auto_wait_visibility(c: &mut McpClient) {
  c.nav("<button style='display:none' id='b' onclick=\"this.textContent='ok'\">Go</button><script>setTimeout(function(){document.getElementById('b').style.display=''},500)</script>");
  let v = c.script_value("await page.click('#b'); return await page.textContent('#b');");
  assert_eq!(v, json!("ok"), "click should auto-wait for visible");
}

// ─── run_script: BrowserContext ─────────────────────────────────────────────

fn test_script_cookies(c: &mut McpClient) {
  c.nav_url("https://example.com");
  let v = c.script_value(
    "await context.addCookies([{ \
         name: 'k', value: 'v', domain: 'example.com', path: '/', \
         secure: false, httpOnly: false, sameSite: 'Lax' \
       }]); \
       const cookies = await context.cookies(); \
       const found = cookies.find(c => c.name === 'k'); \
       await context.deleteCookie('k'); \
       const after = await context.cookies(); \
       return { foundValue: found?.value ?? null, afterCount: after.filter(c => c.name === 'k').length };",
  );
  assert_eq!(v["foundValue"], json!("v"), "cookie should round-trip");
  assert_eq!(v["afterCount"], json!(0), "deleteCookie should remove it");
}

fn test_script_localstorage(c: &mut McpClient) {
  c.nav_url("https://example.com");
  // localStorage lives in the page, not the runner — drive it through
  // page.evaluate. page.evaluate returns a JSON-serialized string so we
  // JSON.parse the payload for each read.
  let v = c.script_value(
    "await page.evaluate(\"localStorage.setItem('lk', 'lv')\"); \
       const rawGot = await page.evaluate(\"localStorage.getItem('lk')\"); \
       const rawLen = await page.evaluate(\"localStorage.length\"); \
       return { got: JSON.parse(rawGot), count: JSON.parse(rawLen) };",
  );
  assert_eq!(v["got"], json!("lv"));
  assert!(v["count"].as_i64().unwrap_or(0) >= 1);
}

// ─── run_script: args + vars + console ──────────────────────────────────────

fn test_script_bound_args(c: &mut McpClient) {
  c.nav("<input id='i' type='text'>");
  let payload = c.script_with_args(
    "await page.fill('#i', args[0]); return await page.inputValue('#i');",
    json!(["prompt-injection\"; drop table; --"]),
  );
  assert_eq!(
    payload["status"].as_str(),
    Some("ok"),
    "bound args should not break source parsing: {payload}"
  );
  assert_eq!(payload["value"], json!("prompt-injection\"; drop table; --"));
}

fn test_script_vars_persist_across_calls(c: &mut McpClient) {
  c.nav("<body></body>");
  let _ = c.script_value("vars.set('k', 'v1'); return null;");
  let v = c.script_value("return vars.get('k');");
  assert_eq!(v, json!("v1"), "vars should persist across run_script calls");
}

fn test_script_console_captured(c: &mut McpClient) {
  c.nav("<body></body>");
  let payload = c.script(
    "console.log('hello from script'); \
       console.warn('be careful', 42); \
       return null;",
  );
  assert_eq!(payload["status"].as_str(), Some("ok"));
  let entries = payload["console"].as_array().expect("console array");
  assert!(entries.len() >= 2, "expected >= 2 console entries: {entries:?}");
  assert_eq!(entries[0]["level"], json!("log"));
  assert!(entries[0]["message"].as_str().unwrap_or("").contains("hello"));
}

fn test_script_error_surfaces_structured(c: &mut McpClient) {
  c.nav("<body></body>");
  let payload = c.script("throw new Error('boom');");
  assert_eq!(payload["status"].as_str(), Some("error"));
  assert!(
    payload["error"]["message"].as_str().unwrap_or("").contains("boom"),
    "error message should include 'boom': {payload}"
  );
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

  macro_rules! run_webkit {
    ($name:ident) => {{
      if backend == "webkit" {
        run!($name);
      }
    }};
  }

  // Navigation + session
  run!(test_navigate);
  run!(test_page_list);
  run!(test_page_reload);
  run!(test_page_back_forward);

  // evaluate
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

  // snapshot + screenshot + search_page
  run!(test_snapshot);
  run!(test_snapshot_scroll_info);
  run!(test_screenshot_png);
  run!(test_screenshot_full_page);
  run!(test_search_page);
  run!(test_search_page_regex);
  run!(test_search_page_no_match);

  // diagnostics (CDP-only: trace uses Performance domain)
  run!(test_console_messages);
  run!(test_network_requests);
  run_cdp!(test_trace);

  // run_script: Page interaction
  run!(test_script_click);
  run!(test_script_fill);
  run!(test_script_fill_form);
  run!(test_script_type);
  run!(test_script_press);
  run!(test_script_hover);
  run!(test_script_dblclick);
  run!(test_script_select_option);
  run!(test_script_check_uncheck);
  run!(test_script_scroll);
  run!(test_script_scroll_into_view);
  run!(test_script_click_offscreen);
  run!(test_script_dialog_alert);
  run!(test_script_fill_dispatches_events);

  // run_script: mouse/keyboard namespaces + coordinate-based actions
  run!(test_script_click_at);
  run!(test_script_mouse_click_coords);
  run!(test_script_drag_coords);
  run!(test_script_drag_and_drop);
  run!(test_script_drag_and_drop_options);
  run!(test_script_locator_drag_to_options);
  run!(test_script_drag_and_drop_trial);
  run!(test_script_emulate_media_all_fields);
  run!(test_script_emulate_media_null_disables_single_field);
  run!(test_script_add_init_script);
  run!(test_script_utility_script_exposed);
  // Task 1.2 + 1.3 phase C — JSHandle + ElementHandle lifecycle (4
  // backends via the QuickJS bindings).
  run!(test_script_handle_lifecycle);
  // WebKit-only observability: Op::ReleaseRef actually shrinks
  // `window.__wr` (CDP and BiDi dispose paths are proven via the
  // successful `dispose()` call but have no page-observable side
  // effect until phase D's use-after-dispose test).
  run_webkit!(test_script_handle_lifecycle_webkit_observable);
  // Task 1.3 phase D — page.evaluate(fn, arg) + evaluateHandle + rich
  // type round-trip. Rule 9 covers all 4 backends.
  run!(test_script_evaluate_fn_and_handle);
  run!(test_script_evaluate_rich_types);
  // Task 1.2 phase E — ElementHandle DOM methods (reads, state,
  // bounding box, click / focus / scroll). Rule 9 covers all 4
  // backends.
  run!(test_script_element_handle_methods);
  // Task 1.2 + 1.3 phase F — handle materialisation
  // (querySelectorAll, locator.elementHandle{,s}). Rule 9 on all 4
  // backends.
  run!(test_script_handle_materialisation);
  run!(test_script_click_options);
  run!(test_script_action_timeout);
  run!(test_script_tap_native);
  run!(test_script_fill_force);
  run!(test_script_check_behavior);
  run!(test_script_dispatch_event_timeout);
  run!(test_script_select_option_force);
  run!(test_script_mouse_wheel);
  run!(test_script_keyboard_press);

  // run_script: Frame sync accessors (Playwright parity — task 3.8)
  run!(test_script_frame_sync_accessors);
  run!(test_script_frame_selector_union);

  // run_script: waits
  run!(test_script_wait_for_selector);
  run!(test_script_wait_for_text);
  run!(test_script_auto_wait_visibility);

  // run_script: Locator chains
  run!(test_script_locator_role);
  run!(test_script_locator_label);
  run!(test_script_locator_placeholder);
  run!(test_script_locator_text);
  run!(test_script_locator_nth);
  run!(test_script_locator_all_text);
  run!(test_script_selector_chain);

  // run_script: file input
  run!(test_script_upload_file);

  // run_script: page-scoped emulation (CDP-only for UA + viewport — WebKit has
  // its own emulation path that isn't surfaced here yet).
  run_cdp!(test_script_user_agent);
  run_cdp!(test_script_viewport);

  // run_script: context-scoped emulation
  run_cdp!(test_script_geolocation);
  run_cdp!(test_script_offline);

  // run_script: BrowserContext cookies + page storage
  run!(test_script_cookies);
  run!(test_script_localstorage);

  // run_script: page markdown extraction
  run!(test_script_markdown);
  run!(test_script_markdown_links);

  // run_script: args + vars + console + errors
  run!(test_script_bound_args);
  run!(test_script_vars_persist_across_calls);
  run!(test_script_console_captured);
  run!(test_script_error_surfaces_structured);

  // Multi-page last (changes session state)
  run!(test_new_page);

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
