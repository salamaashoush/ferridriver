//! Rule-9 integration tests for `WebError` / `pageerror` / `weberror`
//! as first-class event handles accessible via
//! `page.waitForEvent('pageerror')` (Playwright:
//! `Promise<Error>` — native JS `Error`) and
//! `context.waitForEvent('weberror')` (Playwright:
//! `Promise<WebError>` — live `WebError` class with
//! `error()` → native `Error`).
//!
//! Per-backend expectations:
//! * cdp-pipe / cdp-raw — full round-trip through
//!   `Runtime.exceptionThrown`. `name` comes from the exception's
//!   description prefix (or `preview.name` override), `message` is the
//!   post-`': '` remainder, `stack` is the full
//!   `description + callFrames` string.
//! * bidi — `log.entryAdded` with `type: 'javascript'` + `level: 'error'`.
//!   `name` / `message` come from splitting `text` at `': '`; `stack`
//!   is `text` followed by one `    at <func> (<url>:<line+1>:<col+1>)`
//!   line per stack frame.
//! * webkit — `window.addEventListener('error', …)` injected via the
//!   host-side userScript posts `"<name>: <message>\n<stack>"` through
//!   the existing `fdConsole` IPC with `level: 'pageerror'`. The Rust
//!   drain routes to `PageEvent::PageError` and recovers the
//!   structured shape.

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

/// `page.waitForEvent('pageerror')` resolves to a **native JS `Error`**
/// (not a wrapper class). Assertions use `instanceof Error` + direct
/// `.name` / `.message` / `.stack` property access on the raw value.
///
/// Polls for the specific error identifier rather than asserting the
/// first event — Firefox BiDi emits a spurious cross-origin
/// `"Permission denied"` error at page init that would otherwise land
/// first. Playwright's own BiDi consumers hit the same quirk; polling
/// is the robust stance (matches their waitForEvent + predicate
/// option).
pub fn test_page_error_is_native_error(c: &mut McpClient) {
  let html = "<!doctype html><html><body><h1>wait-pageerror</h1></body></html>";
  let url = format!("data:text/html,{}", urlencoding(html));
  let script = format!(
    r"
    await page.goto({url});
    await page.evaluate(() => {{
      setTimeout(() => {{
        const e = new Error('boom');
        window.dispatchEvent(new ErrorEvent('error', {{ error: e, message: e.message }}));
        throw e;
      }}, 10);
    }});
    const deadline = Date.now() + 5000;
    let match = null;
    while (Date.now() < deadline) {{
      const remaining = deadline - Date.now();
      if (remaining <= 0) break;
      const err = await page.waitForEvent('pageerror', remaining);
      // Playwright parity: `err` is a native JS Error, not a wrapper.
      if (err && err.message && err.message.indexOf('boom') !== -1) {{
        match = {{
          isError: err instanceof Error,
          name: err.name,
          message: err.message,
          stackIsString: typeof err.stack === 'string',
        }};
        break;
      }}
    }}
    return match;
  ",
    url = serde_json::to_string(&url).unwrap()
  );
  let v = c.script_value(&script);
  assert!(!v.is_null(), "expected a pageerror with 'boom' message: {v}");
  assert_eq!(
    v["isError"].as_bool(),
    Some(true),
    "page.waitForEvent('pageerror') should resolve to `instanceof Error`: {v}"
  );
  assert_eq!(
    v["name"].as_str(),
    Some("Error"),
    "pageerror name should be 'Error': {v}"
  );
  assert!(
    v["message"].as_str().unwrap_or("").contains("boom"),
    "pageerror message should contain 'boom': {v}"
  );
  assert_eq!(
    v["stackIsString"].as_bool(),
    Some(true),
    "pageerror stack must be a string (possibly empty on synthesised dispatches): {v}"
  );
}

/// `context.waitForEvent('weberror')` resolves to a live `WebError`
/// class instance with `error()` returning a native JS `Error`.
/// Exercises the per-page → per-context bridge installed by
/// `BrowserState::register_opened_page`.
pub fn test_context_weberror_is_webbed_error_class(c: &mut McpClient) {
  let html = "<!doctype html><html><body><h1>wait-weberror</h1></body></html>";
  let url = format!("data:text/html,{}", urlencoding(html));
  let script = format!(
    r"
    await page.goto({url});
    await page.evaluate(() => {{
      setTimeout(() => {{
        const e = new Error('ctx-forwarded');
        window.dispatchEvent(new ErrorEvent('error', {{ error: e, message: e.message }}));
        throw e;
      }}, 10);
    }});
    const deadline = Date.now() + 5000;
    let match = null;
    while (Date.now() < deadline) {{
      const remaining = deadline - Date.now();
      if (remaining <= 0) break;
      const webErr = await context.waitForEvent('weberror', remaining);
      // `webErr` is a WebError class instance — call .error() to
      // retrieve the native JS Error.
      const err = webErr && typeof webErr.error === 'function' ? webErr.error() : null;
      if (err && err.message && err.message.indexOf('ctx-forwarded') !== -1) {{
        match = {{
          webErrorHasErrorMethod: typeof webErr.error === 'function',
          errorIsError: err instanceof Error,
          name: err.name,
          message: err.message,
        }};
        break;
      }}
    }}
    return match;
  ",
    url = serde_json::to_string(&url).unwrap()
  );
  let v = c.script_value(&script);
  assert!(!v.is_null(), "expected a weberror with 'ctx-forwarded' message: {v}");
  assert_eq!(
    v["webErrorHasErrorMethod"].as_bool(),
    Some(true),
    "context.waitForEvent('weberror') should resolve to a class with `.error()`: {v}"
  );
  assert_eq!(
    v["errorIsError"].as_bool(),
    Some(true),
    "webError.error() should return a native JS Error: {v}"
  );
  assert_eq!(
    v["name"].as_str(),
    Some("Error"),
    "webError.error().name should be 'Error': {v}"
  );
  assert!(
    v["message"].as_str().unwrap_or("").contains("ctx-forwarded"),
    "webError.error().message should contain 'ctx-forwarded': {v}"
  );
}

/// `webError.location()` (Playwright 1.60) returns a `{ url, line, column }`
/// captured from the error's top stack frame. Before this landed,
/// `location` was undefined.
pub fn test_web_error_location(c: &mut McpClient) {
  c.nav("<body>weberror</body>");
  let v = c.script_value(
    r"
    const [werr] = await Promise.all([
      context.waitForEvent('weberror', 5000),
      page.evaluate(() => { setTimeout(() => { throw new Error('boom-loc'); }, 10); }),
    ]);
    const loc = werr.location();
    return {
      name: werr.error().name,
      message: werr.error().message,
      url: loc.url,
      lineType: typeof loc.line,
      columnType: typeof loc.column,
      urlType: typeof loc.url,
    };
    ",
  );
  assert_eq!(v["message"].as_str(), Some("boom-loc"), "{v}");
  assert_eq!(v["name"].as_str(), Some("Error"), "{v}");
  assert_eq!(
    v["lineType"].as_str(),
    Some("number"),
    "location.line must be numeric: {v}"
  );
  assert_eq!(
    v["columnType"].as_str(),
    Some("number"),
    "location.column must be numeric: {v}"
  );
  assert_eq!(
    v["urlType"].as_str(),
    Some("string"),
    "location.url must be a string: {v}"
  );
}
