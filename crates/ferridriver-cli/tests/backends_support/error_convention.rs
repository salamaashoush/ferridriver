#![allow(clippy::unwrap_used, clippy::expect_used)]
//! How failures reach the client.
//!
//! MCP splits errors in two: a JSON-RPC `error` means the request could
//! not be processed (unknown tool, arguments the schema rejects) and is
//! for the host; a result with `isError: true` means the tool ran and
//! failed, and is for the model, which is the only party that can react
//! by retrying, re-snapshotting, or choosing another selector.
//!
//! Everything a ferridriver tool can fail at is the second kind. These
//! tests pin that down, because the failure mode is silent: a browser
//! error delivered as JSON-RPC `-32603` tells the host the server
//! malfunctioned, and hosts that end a turn on protocol errors deny the
//! model the chance to recover from a timeout it could have retried.

use serde_json::json;

use super::client::{McpClient, extract_text, is_error};

/// A tool result that reports failure the MCP way: no JSON-RPC `error`
/// member, `isError: true`, and a message the model can read.
fn assert_tool_error(resp: &serde_json::Value, ctx: &str) {
  assert!(
    resp.get("error").is_none(),
    "{ctx}: a failed operation must not be a JSON-RPC error (the model never sees those): {resp}"
  );
  assert_eq!(
    resp["result"]["isError"].as_bool(),
    Some(true),
    "{ctx}: expected isError: true: {resp}"
  );
  assert!(
    !extract_text(resp).trim().is_empty(),
    "{ctx}: a tool error must carry a message: {resp}"
  );
}

/// A browser action that cannot complete — navigating to a host that
/// does not resolve — is an execution error, not a protocol error.
pub fn test_failed_navigation_is_a_tool_error(c: &mut McpClient) {
  let resp = c.call_tool("navigate", json!({ "url": "http://ferridriver.invalid.example/nope" }));
  assert_tool_error(&resp, "navigate to an unresolvable host");
}

/// Bad arguments that the declared schema still accepts (an action name
/// the handler does not know, a missing conditional field) are the
/// model's to fix, so they come back as tool errors too.
pub fn test_bad_arguments_are_tool_errors(c: &mut McpClient) {
  let unknown = c.call_tool("page", json!({ "action": "definitely-not-an-action" }));
  assert_tool_error(&unknown, "page with an unknown action");
  assert!(
    extract_text(&unknown).contains("close_browser"),
    "the message should list the valid actions: {unknown}"
  );

  let missing = c.call_tool("page", json!({ "action": "select" }));
  assert_tool_error(&missing, "page(select) without page_index");

  let out_of_range = c.call_tool("page", json!({ "action": "select", "page_index": 99 }));
  assert_tool_error(&out_of_range, "page(select) with an out-of-range index");
}

/// A thrown script is a failed operation: `isError` says so, and the
/// structured payload still carries `status` / `error` for callers that
/// parse it. Both halves matter — the flag is what a client checks, the
/// payload is what existing callers already read.
pub fn test_thrown_script_sets_is_error_and_keeps_payload(c: &mut McpClient) {
  c.nav("<h1>err</h1>");
  let resp = c.call_tool("run_script", json!({ "source": "throw new Error('boom');" }));
  assert_tool_error(&resp, "run_script that throws");

  // `extract_text` reads the first block only; the structured payload
  // is the second one, so join them.
  let text = resp["result"]["content"]
    .as_array()
    .map(|blocks| {
      blocks
        .iter()
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
    })
    .unwrap_or_default();
  assert!(
    text.contains("\"status\": \"error\""),
    "the payload still reports status: error: {text}"
  );
  assert!(text.contains("boom"), "the payload still carries the message: {text}");
  assert!(
    text.contains("[runtime_error]"),
    "the human summary block survives: {text}"
  );
}

/// The other side of the contract: a script that completes is a plain
/// success, and an operation whose answer is simply "nothing" is not an
/// error at all.
pub fn test_success_paths_are_not_flagged(c: &mut McpClient) {
  c.nav("<h1>fine</h1>");

  let good = c.call_tool("run_script", json!({ "source": "return 1 + 1;" }));
  assert!(!is_error(&good), "a script that returns must not be an error: {good}");

  // No matches is a legitimate result, not a failure.
  let empty = c.call_tool("search_page", json!({ "pattern": "nothing-matches-this-string-xyzzy" }));
  assert!(
    !is_error(&empty),
    "an empty result set is a success, not an error: {empty}"
  );
}

/// A tool that does not exist cannot be processed at all, so it stays a
/// JSON-RPC error — the half of the split that must NOT move.
pub fn test_unknown_tool_stays_a_protocol_error(c: &mut McpClient) {
  let resp = c.call_tool("no_such_tool_at_all", json!({}));
  assert!(
    resp.get("error").is_some(),
    "an unknown tool is a protocol error, not a tool result: {resp}"
  );
}

/// The server keeps working after each failure — a tool error must not
/// leave the session wedged.
pub fn test_session_survives_tool_errors(c: &mut McpClient) {
  c.nav("<h1>after</h1>");
  let text = c.tool_text("evaluate", json!({ "expression": "document.title" }));
  assert!(!text.is_empty(), "session still usable after the error cases");
}

pub fn register(set: &mut crate::TestSet<'_>) {
  set.run(
    "backends_support::error_convention::test_failed_navigation_is_a_tool_error",
    test_failed_navigation_is_a_tool_error,
  );
  set.run(
    "backends_support::error_convention::test_bad_arguments_are_tool_errors",
    test_bad_arguments_are_tool_errors,
  );
  set.run(
    "backends_support::error_convention::test_thrown_script_sets_is_error_and_keeps_payload",
    test_thrown_script_sets_is_error_and_keeps_payload,
  );
  set.run(
    "backends_support::error_convention::test_success_paths_are_not_flagged",
    test_success_paths_are_not_flagged,
  );
  set.run(
    "backends_support::error_convention::test_unknown_tool_stays_a_protocol_error",
    test_unknown_tool_stays_a_protocol_error,
  );
  set.run(
    "backends_support::error_convention::test_session_survives_tool_errors",
    test_session_survives_tool_errors,
  );
}
