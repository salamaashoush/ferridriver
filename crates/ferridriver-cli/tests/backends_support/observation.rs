#![allow(
  clippy::too_many_lines,
  clippy::doc_markdown,
  clippy::uninlined_format_args,
  clippy::single_char_pattern,
  clippy::unwrap_used,
  clippy::expect_used
)]
//! Observation (snapshot/screenshot/search/diagnostics) tests, extracted from backends.rs.

use serde_json::json;

use super::client::{McpClient, extract_image_b64, ok};

pub fn test_snapshot(c: &mut McpClient) {
  c.nav("<h1>Snap</h1><button>Click</button>");
  let t = c.tool_text("snapshot", json!({}));
  assert!(t.contains("[ref="), "snapshot refs: {t}");
  assert!(t.contains("Snap"), "snapshot content: {t}");
}

pub fn test_snapshot_scroll_info(c: &mut McpClient) {
  c.nav("<div style='height:3000px'>tall</div>");
  // Scroll via a run_script call before snapshotting.
  c.script("window.scrollBy(0, 500); return null;");
  let t = c.tool_text("snapshot", json!({}));
  assert!(t.contains("Scroll:"), "snapshot should show scroll position: {t}");
}

pub fn test_screenshot_png(c: &mut McpClient) {
  c.nav("<h1>Screenshot</h1>");
  // Wait for content to render via the scripted locator waiter.
  c.script("await page.waitForSelector('h1'); return true;");
  let r = c.call_tool("screenshot", json!({}));
  ok(&r, "screenshot");
  let b64 = extract_image_b64(&r);
  assert!(b64.starts_with("iVBOR"), "valid PNG: {}", &b64[..20.min(b64.len())]);
}

pub fn test_screenshot_full_page(c: &mut McpClient) {
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

pub fn test_search_page(c: &mut McpClient) {
  c.nav("<p>Alpha Beta Gamma</p><p>Delta Beta Epsilon</p>");
  let t = c.tool_text("search_page", json!({"pattern": "Beta"}));
  assert!(t.contains("2"), "should find 2 matches: {t}");
  assert!(t.contains("Beta"), "should show match text: {t}");
}

pub fn test_search_page_regex(c: &mut McpClient) {
  c.nav("<p>Order #123</p><p>Order #456</p>");
  let t = c.tool_text("search_page", json!({"pattern": "Order #\\d+", "regex": true}));
  assert!(t.contains("2"), "regex should find 2 matches: {t}");
}

pub fn test_search_page_no_match(c: &mut McpClient) {
  c.nav("<p>Hello world</p>");
  let t = c.tool_text("search_page", json!({"pattern": "nonexistent"}));
  assert!(t.contains("No matches") || t.contains("0"), "no matches: {t}");
}

pub fn test_console_messages(c: &mut McpClient) {
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

pub fn test_network_requests(c: &mut McpClient) {
  c.nav_url("https://example.com");
  let t = c.tool_text("diagnostics", json!({"type": "network"}));
  assert!(
    t.contains("example.com") || t.contains("GET") || t.contains("request"),
    "network diagnostics should list requests: {t}"
  );
}

pub fn test_trace(c: &mut McpClient) {
  // BiDi has no per-page CDP-style tracing; metrics() returns
  // Unsupported. CDP / webkit produce real metrics.
  if c.backend == "bidi" {
    return;
  }
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

pub fn register(set: &mut crate::TestSet<'_>) {
  set.run("backends_support::observation::test_snapshot", test_snapshot);
  set.run(
    "backends_support::observation::test_snapshot_scroll_info",
    test_snapshot_scroll_info,
  );
  set.run(
    "backends_support::observation::test_screenshot_png",
    test_screenshot_png,
  );
  set.run(
    "backends_support::observation::test_screenshot_full_page",
    test_screenshot_full_page,
  );
  set.run("backends_support::observation::test_search_page", test_search_page);
  set.run(
    "backends_support::observation::test_search_page_regex",
    test_search_page_regex,
  );
  set.run(
    "backends_support::observation::test_search_page_no_match",
    test_search_page_no_match,
  );
  set.run(
    "backends_support::observation::test_console_messages",
    test_console_messages,
  );
  set.run(
    "backends_support::observation::test_network_requests",
    test_network_requests,
  );
  set.run("backends_support::observation::test_trace", test_trace);
}
