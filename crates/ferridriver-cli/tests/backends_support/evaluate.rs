#![allow(
  clippy::too_many_lines,
  clippy::doc_markdown,
  clippy::uninlined_format_args,
  clippy::single_char_pattern,
  clippy::unwrap_used,
  clippy::expect_used
)]
//! evaluate (page-side JS one-liners) tests, extracted from backends.rs.

use serde_json::json;

use super::client::{McpClient, is_error};

pub fn test_evaluate_number(c: &mut McpClient) {
  c.nav("<body></body>");
  let t = c.tool_text("evaluate", json!({"expression": "1 + 1"}));
  assert!(t.contains("2"), "evaluate 1+1: {t}");
}

pub fn test_evaluate_string(c: &mut McpClient) {
  c.nav("<body></body>");
  let t = c.tool_text("evaluate", json!({"expression": "'hello'"}));
  assert!(t.contains("hello"), "evaluate string: {t}");
}

pub fn test_evaluate_dom(c: &mut McpClient) {
  c.nav("<h1>Test</h1>");
  let t = c.tool_text(
    "evaluate",
    json!({"expression": "document.querySelector('h1').textContent"}),
  );
  assert!(t.contains("Test"), "evaluate dom: {t}");
}

pub fn test_evaluate_promise(c: &mut McpClient) {
  c.nav("<body></body>");
  let t = c.tool_text("evaluate", json!({"expression": "Promise.resolve(42)"}));
  assert!(t.contains("42"), "evaluate promise: {t}");
}

pub fn test_evaluate_boolean(c: &mut McpClient) {
  c.nav("<body></body>");
  let t = c.tool_text("evaluate", json!({"expression": "true"}));
  assert!(t.contains("true"), "evaluate bool: {t}");
}

pub fn test_evaluate_array(c: &mut McpClient) {
  c.nav("<body></body>");
  let t = c.tool_text("evaluate", json!({"expression": "JSON.stringify([1,2,3])"}));
  assert!(t.contains("1") && t.contains("3"), "evaluate array: {t}");
}

pub fn test_evaluate_object(c: &mut McpClient) {
  c.nav("<body></body>");
  let t = c.tool_text("evaluate", json!({"expression": "({a: 1, b: true})"}));
  assert!(t.contains("a") && t.contains("1"), "evaluate object: {t}");
}

pub fn test_evaluate_null(c: &mut McpClient) {
  c.nav("<body></body>");
  let t = c.tool_text("evaluate", json!({"expression": "null"}));
  assert!(t.contains("null") || t.contains("undefined"), "evaluate null: {t}");
}

pub fn test_evaluate_error(c: &mut McpClient) {
  c.nav("<body></body>");
  let r = c.call_tool("evaluate", json!({"expression": "thisFunctionDoesNotExist()"}));
  assert!(is_error(&r), "should be error");
}

pub fn test_evaluate_syntax_error(c: &mut McpClient) {
  c.nav("<body></body>");
  let r = c.call_tool("evaluate", json!({"expression": "function{"}));
  assert!(is_error(&r), "syntax error should fail");
}

pub fn test_evaluate_large_payload(c: &mut McpClient) {
  c.nav("<body></body>");
  let t = c.tool_text(
    "evaluate",
    json!({"expression": "JSON.stringify(Array(1000).fill('x'))"}),
  );
  assert!(t.len() > 1000, "large payload: {}", t.len());
}

pub fn register(set: &mut crate::TestSet<'_>) {
  set.run("backends_support::evaluate::test_evaluate_number", test_evaluate_number);
  set.run("backends_support::evaluate::test_evaluate_string", test_evaluate_string);
  set.run("backends_support::evaluate::test_evaluate_dom", test_evaluate_dom);
  set.run(
    "backends_support::evaluate::test_evaluate_promise",
    test_evaluate_promise,
  );
  set.run(
    "backends_support::evaluate::test_evaluate_boolean",
    test_evaluate_boolean,
  );
  set.run("backends_support::evaluate::test_evaluate_array", test_evaluate_array);
  set.run("backends_support::evaluate::test_evaluate_object", test_evaluate_object);
  set.run("backends_support::evaluate::test_evaluate_null", test_evaluate_null);
  set.run("backends_support::evaluate::test_evaluate_error", test_evaluate_error);
  set.run(
    "backends_support::evaluate::test_evaluate_syntax_error",
    test_evaluate_syntax_error,
  );
  set.run(
    "backends_support::evaluate::test_evaluate_large_payload",
    test_evaluate_large_payload,
  );
}
