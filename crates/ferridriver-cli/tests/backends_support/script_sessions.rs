#![allow(
  clippy::too_many_lines,
  clippy::doc_markdown,
  clippy::uninlined_format_args,
  clippy::single_char_pattern,
  clippy::unwrap_used,
  clippy::expect_used
)]
//! run_script: args + vars + console session tests, extracted from backends.rs.

use serde_json::json;

use super::client::McpClient;

pub fn test_script_bound_args(c: &mut McpClient) {
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

pub fn test_script_vars_persist_across_calls(c: &mut McpClient) {
  c.nav("<body></body>");
  let _ = c.script_value("vars.set('k', 'v1'); return null;");
  let v = c.script_value("return vars.get('k');");
  assert_eq!(v, json!("v1"), "vars should persist across run_script calls");
}

pub fn test_script_globals_persist_across_calls(c: &mut McpClient) {
  c.nav("<body></body>");
  let _ = c.script_value("globalThis.counter = 41; return null;");
  // The user's headline contract: `globalThis` state carries forward
  // across separate run_script calls in the same session.
  let v = c.script_value("return ++globalThis.counter;");
  assert_eq!(v, json!(42), "globalThis state must persist across run_script calls");
}

pub fn test_script_throw_keeps_session_state(c: &mut McpClient) {
  c.nav("<body></body>");
  let _ = c.script_value("globalThis.keep = 'alive'; return null;");
  let err = c.script("throw new Error('boom');");
  assert_eq!(err["status"].as_str(), Some("error"), "expected error: {err}");
  // A plain JS throw must NOT poison the session — state survives.
  let v = c.script_value("return globalThis.keep;");
  assert_eq!(v, json!("alive"), "plain throw must not drop session state");
}

pub fn test_script_session_recovers_after_timeout(c: &mut McpClient) {
  c.nav("<body></body>");
  let _ = c.script_value("globalThis.keep = 'alive'; return 'ok';");
  // Force a poisoning timeout (interrupt halts the interpreter mid-run).
  let timed = c.script_with_timeout("while (true) { /* spin */ }", 500);
  assert_eq!(
    timed["status"].as_str(),
    Some("error"),
    "expected timeout error: {timed}"
  );
  // Next call must transparently get a fresh VM and just work.
  let v = c.script_value("return 1 + 1;");
  assert_eq!(v, json!(2), "session must recover after a poisoning timeout");
  // The rebuilt VM correctly discarded the poisoned VM's state.
  let gone = c.script_value("return typeof globalThis.keep;");
  assert_eq!(gone, json!("undefined"), "rebuilt VM must not carry poisoned state");
}

pub fn test_script_timers_and_web_globals(c: &mut McpClient) {
  c.nav("<body></body>");
  // setTimeout/await (was unsupported before rquickjs-extra-timers),
  // plus URL + TextEncoder/btoa wired at Session::create.
  let v = c.script_value(
    "const t = await new Promise((r) => setTimeout(() => r('tick'), 20)); \
     const host = new URL('https://ex.com/p?x=1').host; \
     return { t, host, b64: btoa('hi'), bytes: new TextEncoder().encode('ok').length };",
  );
  assert_eq!(v["t"], json!("tick"), "setTimeout/await resolved: {v}");
  assert_eq!(v["host"], json!("ex.com"), "URL parsed: {v}");
  assert_eq!(v["b64"], json!("aGk="), "btoa: {v}");
  assert_eq!(v["bytes"], json!(2), "TextEncoder: {v}");
}

pub fn test_script_console_captured(c: &mut McpClient) {
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

pub fn test_script_error_surfaces_structured(c: &mut McpClient) {
  c.nav("<body></body>");
  let payload = c.script("throw new Error('boom');");
  assert_eq!(payload["status"].as_str(), Some("error"));
  assert!(
    payload["error"]["message"].as_str().unwrap_or("").contains("boom"),
    "error message should include 'boom': {payload}"
  );
}

pub fn register(set: &mut crate::TestSet<'_>) {
  set.run(
    "backends_support::script_sessions::test_script_bound_args",
    test_script_bound_args,
  );
  set.run(
    "backends_support::script_sessions::test_script_vars_persist_across_calls",
    test_script_vars_persist_across_calls,
  );
  set.run(
    "backends_support::script_sessions::test_script_globals_persist_across_calls",
    test_script_globals_persist_across_calls,
  );
  set.run(
    "backends_support::script_sessions::test_script_throw_keeps_session_state",
    test_script_throw_keeps_session_state,
  );
  set.run(
    "backends_support::script_sessions::test_script_session_recovers_after_timeout",
    test_script_session_recovers_after_timeout,
  );
  set.run(
    "backends_support::script_sessions::test_script_timers_and_web_globals",
    test_script_timers_and_web_globals,
  );
  set.run(
    "backends_support::script_sessions::test_script_console_captured",
    test_script_console_captured,
  );
  set.run(
    "backends_support::script_sessions::test_script_error_surfaces_structured",
    test_script_error_surfaces_structured,
  );
}
