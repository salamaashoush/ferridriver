//! Rule-9 integration tests for `ConsoleMessage` as a first-class
//! event handle accessible via `page.waitForEvent('console')`.
//!
//! Per-backend expectations:
//! * cdp-pipe / cdp-raw — full round-trip through
//!   `Runtime.consoleAPICalled` with `args` as `JSHandle`s and
//!   `location` from `stackTrace.callFrames[0]`.
//! * bidi — `log.entryAdded` with `type: 'console'`. Args land as
//!   `JSHandle`s via the BiDi handle builder; `location` from
//!   `stackTrace.callFrames[0]` with Playwright's fallback
//!   `{ '', 1, 1 }` when the stack is empty.
//! * webkit — stock `WKWebView` host only surfaces `(level, text)`
//!   through our IPC. `args` is empty and `location` is the default
//!   `{ '', 0, 0 }`. The test asserts type/text round-trip but
//!   documents the gap for args/location.

#![allow(
  clippy::too_many_lines,
  clippy::doc_markdown,
  clippy::uninlined_format_args,
  clippy::unwrap_used,
  clippy::expect_used,
  clippy::needless_pass_by_value
)]

use super::client::McpClient;

/// Page HTML that doesn't emit console messages on its own — tests
/// trigger the console call via `evaluate` so we can observe args +
/// text shape precisely.
const BLANK_HTML: &str = "<!doctype html><html><body><h1>x</h1></body></html>";

fn urlencoding(s: &str) -> String {
  s.replace(' ', "%20").replace('#', "%23").replace('"', "%22")
}

/// `console.log('hello', 42)` — two primitive args. On CDP / BiDi we
/// observe `args.length === 2`, `type === 'log'`, `text === 'hello 42'`.
/// On WebKit `args.length === 0` (documented gap) and `text` is the
/// host-computed preview string.
pub fn test_console_message_primitives(c: &mut McpClient) {
  c.nav_url(&format!("data:text/html,{}", urlencoding(BLANK_HTML)));
  let script = r#"
    const waiter = page.waitForEvent("console", 5000);
    await page.evaluate(() => console.log("hello", 42));
    const msg = await waiter;
    return {
      type: msg.type(),
      text: msg.text(),
      argsLen: msg.args().length,
    };
  "#;
  let v = c.script_value(script);
  assert_eq!(v["type"].as_str(), Some("log"), "type should be 'log': {v}");
  let text = v["text"].as_str().unwrap_or("");
  assert!(
    text.contains("hello") && text.contains("42"),
    "text should include both primitive args: {v}"
  );
  assert_eq!(v["argsLen"].as_u64(), Some(2), "should report 2 args: {v}");
}

/// `console.warn` -> `type() === 'warning'` on BiDi (which reports
/// `method: 'warn'` and we remap per Playwright parity). CDP reports
/// `'warning'` natively in `Runtime.consoleAPICalled.type`.
pub fn test_console_message_warn_maps_to_warning(c: &mut McpClient) {
  c.nav_url(&format!("data:text/html,{}", urlencoding(BLANK_HTML)));
  let script = r#"
    const waiter = page.waitForEvent("console", 5000);
    await page.evaluate(() => console.warn("careful"));
    const msg = await waiter;
    return { type: msg.type(), text: msg.text() };
  "#;
  let v = c.script_value(script);
  assert_eq!(
    v["type"].as_str(),
    Some("warning"),
    "console.warn should surface as type 'warning' (Playwright parity): {v}"
  );
  assert!(
    v["text"].as_str().unwrap_or("").contains("careful"),
    "warn text should include the payload: {v}"
  );
}

/// `console.error` — preserves the error type label. All four backends
/// should round-trip the type string consistently.
pub fn test_console_message_error_type(c: &mut McpClient) {
  c.nav_url(&format!("data:text/html,{}", urlencoding(BLANK_HTML)));
  let script = r#"
    const waiter = page.waitForEvent("console", 5000);
    await page.evaluate(() => console.error("boom"));
    const msg = await waiter;
    return { type: msg.type(), text: msg.text() };
  "#;
  let v = c.script_value(script);
  assert_eq!(
    v["type"].as_str(),
    Some("error"),
    "console.error should surface as type 'error': {v}"
  );
  assert!(
    v["text"].as_str().unwrap_or("").contains("boom"),
    "error text should include the payload: {v}"
  );
}

/// `location()` surfaces the `{ url, lineNumber, columnNumber }`
/// shape on all backends. CDP populates the struct from
/// `Runtime.StackTrace.callFrames[0]`, BiDi from `log.entryAdded`'s
/// own stackTrace, WebKit leaves the defaults in place (no IPC
/// payload for frames — documented Section B gap). Console calls
/// issued via `Runtime.evaluate` / `page.evaluate` don't always carry
/// a user-script URL (the devtools eval context is nameless), so the
/// check is shape-only: every field exists and is the right type.
pub fn test_console_message_location_shape(c: &mut McpClient) {
  c.nav_url(&format!("data:text/html,{}", urlencoding(BLANK_HTML)));
  let script = r#"
    const waiter = page.waitForEvent("console", 5000);
    await page.evaluate(() => console.log("loc-check"));
    const msg = await waiter;
    const loc = msg.location();
    return {
      url: loc.url,
      line: loc.lineNumber,
      column: loc.columnNumber,
    };
  "#;
  let v = c.script_value(script);
  assert!(
    v.get("url").is_some() && v.get("line").is_some() && v.get("column").is_some(),
    "location object should have url/line/column: {v}"
  );
  assert!(v["url"].is_string(), "url must be string: {v}");
  assert!(v["line"].is_number(), "line must be number: {v}");
  assert!(v["column"].is_number(), "column must be number: {v}");
}
