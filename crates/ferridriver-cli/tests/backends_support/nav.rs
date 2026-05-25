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

pub fn register(set: &mut crate::TestSet<'_>) {
  set.run("backends_support::nav::test_navigate", test_navigate);
  set.run("backends_support::nav::test_page_list", test_page_list);
  set.run("backends_support::nav::test_page_reload", test_page_reload);
  set.run("backends_support::nav::test_page_back_forward", test_page_back_forward);
}
