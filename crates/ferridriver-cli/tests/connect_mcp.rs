#![allow(
  clippy::too_many_lines,
  clippy::unwrap_used,
  clippy::expect_used,
  clippy::needless_pass_by_value,
  clippy::redundant_closure_for_method_calls,
  clippy::uninlined_format_args
)]
//! MCP-level test: connect to running Chrome + page select.
//! Reproduces the exact flow Claude Code uses via MCP stdio.
//!
//! Run: `cargo test -p ferridriver-cli --test connect_mcp -- --nocapture`

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

static GLOBAL_ID: AtomicU64 = AtomicU64::new(1);

struct McpClient {
  child: Child,
  reader: BufReader<std::process::ChildStdout>,
  stdin: std::process::ChildStdin,
}

impl McpClient {
  fn new() -> Self {
    let binary = std::env::var("FERRIDRIVER_BIN")
      .unwrap_or_else(|_| format!("{}/../../target/debug/ferridriver", env!("CARGO_MANIFEST_DIR")));
    eprintln!("[mcp] launching binary: {binary}");
    let mut child = Command::new(&binary)
      .arg("mcp")
      .arg("--backend")
      .arg("cdp-raw")
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .stderr(Stdio::inherit()) // inherit stderr so we see debug logs!
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
    let mut line = String::new();
    self.reader.read_line(&mut line).expect("read stdout");
    serde_json::from_str(line.trim()).unwrap_or_else(|e| panic!("JSON parse: {e}\nRaw: {line}"))
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
}

impl Drop for McpClient {
  fn drop(&mut self) {
    let _ = self.child.kill();
    let _ = self.child.wait();
  }
}

fn extract_text(resp: &Value) -> String {
  resp
    .pointer("/result/content/0/text")
    .and_then(|v| v.as_str())
    .unwrap_or("<no text>")
    .to_string()
}

#[test]
fn mcp_connect_and_select() {
  let t = Instant::now();
  let mut client = McpClient::new();
  eprintln!("\n[test] {:?} MCP server ready", t.elapsed());

  // Step 1: connect (auto_discover)
  eprintln!("\n[test] {:?} === calling connect(auto_discover) ===", t.elapsed());
  let t1 = Instant::now();
  let resp = client.call_tool("connect", json!({"auto_discover": true}));
  let text = extract_text(&resp);
  eprintln!("[test] {:?} connect returned ({:?}):", t.elapsed(), t1.elapsed());
  // Print first 500 chars to see the result
  eprintln!("{}", &text[..text.len().min(500)]);

  // Step 2: page list
  eprintln!("\n[test] {:?} === calling page(list) ===", t.elapsed());
  let t2 = Instant::now();
  let resp = client.call_tool("page", json!({"action": "list"}));
  let text = extract_text(&resp);
  eprintln!("[test] {:?} page list returned ({:?}):", t.elapsed(), t2.elapsed());
  eprintln!("{}", &text[..text.len().min(1000)]);

  // Step 3: page select on each page
  // Find how many pages from the list output
  let page_count = text.matches("Page ").count();
  eprintln!("\n[test] {:?} found {page_count} pages, selecting each...", t.elapsed());

  for idx in 0..page_count {
    eprintln!("\n[test] {:?} === page(select, {idx}) ===", t.elapsed());
    let t3 = Instant::now();
    let resp = client.call_tool("page", json!({"action": "select", "page_index": idx}));
    let text = extract_text(&resp);
    let elapsed = t3.elapsed();
    // Print first line + timing
    let first_line = text.lines().next().unwrap_or("<empty>");
    eprintln!(
      "[test] {:?} select {idx} returned ({elapsed:?}): {first_line}",
      t.elapsed()
    );

    if elapsed.as_secs() > 10 {
      eprintln!("[test] WARNING: page {idx} took over 10s!");
    }
  }

  // Step 4: Find the WhatsApp page and switch to it
  eprintln!("\n[test] {:?} === finding WhatsApp page ===", t.elapsed());
  let resp = client.call_tool("page", json!({"action": "list"}));
  let list_text = extract_text(&resp);
  eprintln!("[test] page list:\n{}", &list_text[..list_text.len().min(500)]);
  let whatsapp_idx = list_text
    .lines()
    .find(|l| l.contains("web.whatsapp.com"))
    .and_then(|l| {
      // Extract page index from "  Page N: ..." or "  Page N (active): ..."
      let trimmed = l.trim();
      let after_page = trimmed.strip_prefix("Page ")?;
      let idx_str = after_page.split(|c: char| !c.is_ascii_digit()).next()?;
      idx_str.parse::<usize>().ok()
    });
  if let Some(wa_idx) = whatsapp_idx {
    eprintln!("[test] {:?} WhatsApp is page {wa_idx}, selecting...", t.elapsed());
    let resp = client.call_tool("page", json!({"action": "select", "page_index": wa_idx}));
    let snap = extract_text(&resp);
    eprintln!("[test] {:?} selected WhatsApp ({} chars)", t.elapsed(), snap.len());

    // Step 5: Check if snapshot includes chat list with collapsing
    let has_collapsing = snap.contains("... (") && snap.contains("more ");
    let row_count = snap.matches("row ").count();
    eprintln!(
      "[test] snapshot: {} rows visible, collapsing={}",
      row_count, has_collapsing
    );

    // Step 6: Click on first chat (should be 7bibty or whatever is at top)
    eprintln!("\n[test] {:?} === click first chat (e22) ===", t.elapsed());
    let t4 = Instant::now();
    let resp = client.call_tool("click", json!({"ref": "e22"}));
    let _click_text = extract_text(&resp);
    eprintln!("[test] {:?} click returned ({:?})", t.elapsed(), t4.elapsed());

    // Wait for chat to open
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Step 7: Take fresh snapshot — does the message input appear now?
    eprintln!("\n[test] {:?} === snapshot after opening chat ===", t.elapsed());
    let t5 = Instant::now();
    let resp = client.call_tool("snapshot", json!({}));
    let snap2 = extract_text(&resp);
    let has_message_input = snap2.contains("Type a message") || snap2.contains("textbox \"Type a message\"");
    let textbox_count = snap2.matches("textbox").count();
    eprintln!(
      "[test] {:?} snapshot ({:?}): {} chars, {} textboxes, has_message_input={}",
      t.elapsed(),
      t5.elapsed(),
      snap2.len(),
      textbox_count,
      has_message_input
    );

    if has_message_input {
      eprintln!("[test] SUCCESS: message input visible in snapshot!");
    } else {
      eprintln!("[test] PROBLEM: message input NOT in snapshot — collapsing may need tuning");
      // Print tail to see what's there
      let tail_start = snap2.len().saturating_sub(500);
      let safe_start = snap2.ceil_char_boundary(tail_start);
      eprintln!("[test] snapshot tail: {}", &snap2[safe_start..]);
    }

    // Step 8: Test evaluate on WhatsApp
    eprintln!("\n[test] {:?} === evaluate on WhatsApp ===", t.elapsed());
    let t6 = Instant::now();
    let resp = client.call_tool("evaluate", json!({"expression": "document.title"}));
    let eval_text = extract_text(&resp);
    eprintln!(
      "[test] {:?} evaluate returned ({:?}): {}",
      t.elapsed(),
      t6.elapsed(),
      eval_text
    );

    // Step 9: Test type_text (type into focused element)
    eprintln!("\n[test] {:?} === type_text test ===", t.elapsed());
    let t7 = Instant::now();
    // First find and click the message input ref if visible
    if has_message_input {
      // Find the ref for the message input textbox
      if let Some(ref_str) = snap2
        .lines()
        .find(|l| l.contains("Type a message"))
        .and_then(|l| l.split("[ref=").nth(1))
        .and_then(|s| s.split(']').next())
      {
        eprintln!("[test] clicking message input ref={ref_str}");
        client.call_tool("click", json!({"ref": ref_str}));
        std::thread::sleep(std::time::Duration::from_millis(500));

        // Check what element has focus before typing
        let resp = client.call_tool("evaluate", json!({
          "expression": "JSON.stringify({tag: document.activeElement?.tagName, ce: document.activeElement?.isContentEditable, role: document.activeElement?.getAttribute('role'), tab: document.activeElement?.dataset?.tab})"
        }));
        let focus_info = extract_text(&resp);
        eprintln!("[test] active element before typing: {focus_info}");

        // Try type_text
        let resp = client.call_tool("type_text", json!({"text": "hello test"}));
        let type_result = extract_text(&resp);
        eprintln!(
          "[test] {:?} type_text returned ({:?}): {}",
          t.elapsed(),
          t7.elapsed(),
          &type_result[..type_result.len().min(100)]
        );
        std::thread::sleep(std::time::Duration::from_millis(300));

        // Verify: check what's actually in the input now
        let resp = client.call_tool("evaluate", json!({
          "expression": "JSON.stringify({text: document.activeElement?.textContent, value: document.activeElement?.value, inner: document.activeElement?.innerText})"
        }));
        let after_type = extract_text(&resp);
        eprintln!("[test] content after type_text: {after_type}");

        // Take snapshot to see if text appeared
        let resp = client.call_tool("snapshot", json!({}));
        let snap3 = extract_text(&resp);
        let has_hello = snap3.contains("hello test");
        eprintln!("[test] snapshot contains 'hello test': {has_hello}");

        if !has_hello {
          eprintln!("[test] PROBLEM: type_text did NOT produce visible text!");
          // Try press_key approach character by character
          eprintln!("[test] trying press_key char by char...");
          for ch in "ABC".chars() {
            client.call_tool("press_key", json!({"key": ch.to_string()}));
          }
          std::thread::sleep(std::time::Duration::from_millis(300));
          let resp = client.call_tool(
            "evaluate",
            json!({
              "expression": "document.activeElement?.textContent"
            }),
          );
          let after_keys = extract_text(&resp);
          eprintln!("[test] content after press_key: {after_keys}");
        }

        // Clear whatever we typed
        for _ in 0..20 {
          client.call_tool("press_key", json!({"key": "Backspace"}));
        }
      }
    } else {
      eprintln!("[test] skipping type_text — message input not visible in snapshot");
    }
  } else {
    eprintln!("[test] WhatsApp not found in page list, skipping WhatsApp tests");
  }

  eprintln!("\n[test] {:?} done", t.elapsed());
}
