//! Rule-9 integration tests for `WebError` as a first-class event
//! handle accessible via `page.waitForEvent('pageerror')` and
//! `context.waitForEvent('weberror')`.
//!
//! Per-backend expectations:
//! * cdp-pipe / cdp-raw — full round-trip through
//!   `Runtime.exceptionThrown`. `name` comes from the exception's
//!   description prefix (or `preview.name` override), `message` is the
//!   post-`': '` remainder, `stack` is the full `description + callFrames`
//!   string.
//! * bidi — `log.entryAdded` with `type: 'javascript'` + `level: 'error'`.
//!   `name` / `message` come from splitting `text` at `': '`; `stack` is
//!   `text` followed by one `    at <func> (<url>:<line+1>:<col+1>)` line
//!   per stack frame.
//! * webkit — `window.addEventListener('error', …)` injected via the
//!   host-side userScript posts `"<name>: <message>\n<stack>"` through
//!   the existing `fdConsole` IPC with `level: 'pageerror'`. The Rust
//!   drain routes to `PageEvent::PageError` and recovers the structured
//!   shape. `stack` is whatever `error.stack` reported by the engine (may
//!   be empty for `Error` thrown from inline scripts).

#![allow(
  clippy::too_many_lines,
  clippy::doc_markdown,
  clippy::uninlined_format_args,
  clippy::unwrap_used,
  clippy::expect_used,
  clippy::needless_pass_by_value
)]

use super::client::McpClient;

fn urlencoding(s: &str) -> String {
  s.replace(' ', "%20").replace('#', "%23").replace('"', "%22")
}

/// Navigate to an inline HTML page, then dispatch an error from a
/// *second* microtask so the `pageerror` listener is registered before
/// the error fires. Drains any spurious BiDi/Firefox startup errors
/// that land first (e.g. cross-origin warnings from injected scripts)
/// by looping until we see our boom.
///
/// Using `dispatchEvent(new ErrorEvent('error', { error: new Error('boom') }))`
/// rather than `throw` from inline `<script>` because:
/// * CDP `Runtime.exceptionThrown` surfaces both equally.
/// * BiDi Firefox sometimes emits a spurious
///   `"Permission denied to access property 'length'"` from its own
///   cross-origin guards before the user error; polling for the
///   correct payload is more robust than asserting the first event.
/// * WebKit only catches via `window.addEventListener('error', …)`
///   which `ErrorEvent` dispatch propagates directly.
pub fn test_page_error_sync_throw(c: &mut McpClient) {
  let html = "<!doctype html><html><body><h1>wait-pageerror</h1></body></html>";
  let url = format!("data:text/html,{}", urlencoding(html));
  let script = format!(
    r"
    await page.goto({url});
    // Poll up to 5s for a 'pageerror' event whose message === 'boom'.
    // Drains any unrelated errors backends may emit (BiDi Firefox
    // sometimes reports a cross-origin permission error first).
    const deadline = Date.now() + 5000;
    // Kick off the throw via an in-page listener so the backend's
    // `Runtime.exceptionThrown` / `log.entryAdded` / host 'error'
    // handler fires on the next microtask.
    await page.evaluate(() => {{
      setTimeout(() => {{
        const e = new Error('boom');
        window.dispatchEvent(new ErrorEvent('error', {{ error: e, message: e.message }}));
        throw e;
      }}, 10);
    }});
    let match = null;
    while (Date.now() < deadline) {{
      const remaining = deadline - Date.now();
      if (remaining <= 0) break;
      const err = await page.waitForEvent('pageerror', remaining);
      const d = err.error();
      if (d.message && d.message.indexOf('boom') !== -1) {{
        match = d;
        break;
      }}
    }}
    return match ? {{
      name: match.name,
      message: match.message,
      stackNonEmpty: (match.stack || '').length > 0,
    }} : null;
  ",
    url = serde_json::to_string(&url).unwrap()
  );
  let v = c.script_value(&script);
  assert!(!v.is_null(), "expected a pageerror with 'boom' message: {v}");
  // Name parity across engines: Chrome reports `'Error'`, Firefox
  // reports `'Error'`, WebKit via host listener recovers `'Error'`
  // from the payload. The first `: '-separated prefix.
  assert_eq!(
    v["name"].as_str(),
    Some("Error"),
    "pageerror name should be 'Error': {v}"
  );
  assert!(
    v["message"].as_str().unwrap_or("").contains("boom"),
    "pageerror message should contain 'boom': {v}"
  );
  assert!(v["stackNonEmpty"].is_boolean(), "stackNonEmpty must exist: {v}");
}

/// Context-level `'weberror'` listener observes the same error via the
/// per-page → per-context bridge installed by
/// `BrowserState::register_opened_page`. This exercises the fan-out
/// added alongside §2.13.
///
/// Same polling strategy as [`test_page_error_sync_throw`] — some
/// backends emit spurious errors before the user error.
pub fn test_context_weberror_forwarding(c: &mut McpClient) {
  let html = "<!doctype html><html><body><h1>wait-weberror</h1></body></html>";
  let url = format!("data:text/html,{}", urlencoding(html));
  let script = format!(
    r"
    await page.goto({url});
    const deadline = Date.now() + 5000;
    await page.evaluate(() => {{
      setTimeout(() => {{
        const e = new Error('ctx-forwarded');
        window.dispatchEvent(new ErrorEvent('error', {{ error: e, message: e.message }}));
        throw e;
      }}, 10);
    }});
    let match = null;
    while (Date.now() < deadline) {{
      const remaining = deadline - Date.now();
      if (remaining <= 0) break;
      const err = await context.waitForEvent('weberror', remaining);
      const d = err.error();
      if (d.message && d.message.indexOf('ctx-forwarded') !== -1) {{
        match = d;
        break;
      }}
    }}
    return match ? {{ name: match.name, message: match.message }} : null;
  ",
    url = serde_json::to_string(&url).unwrap()
  );
  let v = c.script_value(&script);
  assert!(!v.is_null(), "expected a weberror with 'ctx-forwarded' message: {v}");
  assert_eq!(
    v["name"].as_str(),
    Some("Error"),
    "weberror name should be 'Error': {v}"
  );
  assert!(
    v["message"].as_str().unwrap_or("").contains("ctx-forwarded"),
    "weberror message should contain 'ctx-forwarded': {v}"
  );
}
