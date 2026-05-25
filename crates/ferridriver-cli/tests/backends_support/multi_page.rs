#![allow(
  clippy::too_many_lines,
  clippy::doc_markdown,
  clippy::uninlined_format_args,
  clippy::single_char_pattern,
  clippy::unwrap_used,
  clippy::expect_used
)]
//! Multi-page session tests, extracted from backends.rs.

use serde_json::json;

use super::client::{McpClient, is_error, ok};

pub fn test_new_page(c: &mut McpClient) {
  // Originally relied on a prior test having already opened Page 0
  // (the giant run_all_tests ran navigation first). The split moved
  // this into its own #[test] with a fresh McpClient — bootstrap a
  // first page explicitly so the `page new` below produces Page 1.
  c.nav("<body>seed</body>");

  let r = c.call_tool("page", json!({"action": "new"}));
  if !is_error(&r) {
    let t = c.tool_text("page", json!({"action": "list"}));
    assert!(t.contains("Page 0") && t.contains("Page 1"), "should have 2 pages: {t}");
    let r2 = c.call_tool("page", json!({"action": "select", "page_index": 0}));
    ok(&r2, "page select");
  }
}

pub fn register(set: &mut crate::TestSet<'_>) {
  set.run("backends_support::multi_page::test_new_page", test_new_page);
}
