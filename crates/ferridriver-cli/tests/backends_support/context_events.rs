//! Rule-9 integration tests for context/browser lifecycle-observation
//! events (Playwright 1.60) through QuickJS `run_script`, on every backend:
//! - `browserContext.on('framenavigated' | 'frameattached' | 'pageload' |
//!   'pageclose')` — page-level events mirrored up to the context.
//! - `browser.on('context')` — fired when a new context is created.
//!
//! Each test waits for the event and asserts it resolves with the right live
//! handle (Frame / Page / BrowserContext), so it only passes once the
//! page→context (and browser) event bridge actually forwards.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::needless_pass_by_value)]

use super::client::McpClient;

/// Context `'framenavigated'` mirror event resolves with a `Frame` for
/// the navigated main frame. Only fires once the page→context bridge
/// forwards the page-level frame event.
pub fn test_context_framenavigated(c: &mut McpClient) {
  c.nav("<body>ctx-frame</body>");
  let v = c.script_value(
    r"
    const [frame] = await Promise.all([
      context.waitForEvent('framenavigated', 5000),
      page.goto('data:text/html,<title>navmark</title>'),
    ]);
    return { url: frame.url(), hasUrlFn: typeof frame.url === 'function' };
    ",
  );
  assert_eq!(v["hasUrlFn"].as_bool(), Some(true), "must resolve a Frame: {v}");
  let url = v["url"].as_str().unwrap_or_default();
  assert!(
    url.starts_with("data:") && url.contains("navmark"),
    "frame.url() wrong: {url}"
  );
}

/// Context `'frameattached'` resolves with the newly-attached child
/// `Frame` after an iframe is appended.
pub fn test_context_frameattached(c: &mut McpClient) {
  c.nav("<body><div id=host></div></body>");
  let v = c.script_value(
    r"
    const [frame] = await Promise.all([
      context.waitForEvent('frameattached', 5000),
      page.evaluate(() => {
        const f = document.createElement('iframe');
        f.src = 'data:text/html,<p>child</p>';
        document.getElementById('host').appendChild(f);
      }),
    ]);
    return { hasUrlFn: typeof frame.url === 'function', isMain: frame === page.mainFrame() };
    ",
  );
  assert_eq!(
    v["hasUrlFn"].as_bool(),
    Some(true),
    "frameattached must resolve a Frame: {v}"
  );
}

/// Context `'pageload'` resolves with the `Page` that fired `load`.
pub fn test_context_pageload(c: &mut McpClient) {
  c.nav("<body>ctx-load</body>");
  let v = c.script_value(
    r"
    const [p] = await Promise.all([
      context.waitForEvent('pageload', 5000),
      page.goto('data:text/html,<title>loadmark</title>'),
    ]);
    return { url: p.url(), hasUrlFn: typeof p.url === 'function' };
    ",
  );
  assert_eq!(v["hasUrlFn"].as_bool(), Some(true), "pageload must resolve a Page: {v}");
  assert!(
    v["url"].as_str().unwrap_or_default().contains("loadmark"),
    "page.url() wrong: {v}"
  );
}

/// Context `'pageclose'` resolves with the closed `Page`.
pub fn test_context_pageclose(c: &mut McpClient) {
  c.nav("<body>ctx-close</body>");
  let v = c.script_value(
    r"
    const newPage = await context.newPage();
    const [closed] = await Promise.all([
      context.waitForEvent('pageclose', 5000),
      newPage.close(),
    ]);
    return { isClosed: closed.isClosed(), hasIsClosedFn: typeof closed.isClosed === 'function' };
    ",
  );
  assert_eq!(
    v["hasIsClosedFn"].as_bool(),
    Some(true),
    "pageclose must resolve a Page: {v}"
  );
  assert_eq!(
    v["isClosed"].as_bool(),
    Some(true),
    "closed page should report isClosed: {v}"
  );
}

/// `browser.on('context')` (via waitForEvent) fires when a new context
/// is created, resolving with the live `BrowserContext`. Times out (and
/// fails) if the event is never delivered.
pub fn test_browser_context_event(c: &mut McpClient) {
  c.nav("<body>browser-ctx-event</body>");
  let v = c.script_value(
    r"
    const [bcx] = await Promise.all([
      browser.waitForEvent('context', 5000),
      browser.newContext(),
    ]);
    return {
      hasNewPageFn: typeof bcx.newPage === 'function',
      hasCookiesFn: typeof bcx.cookies === 'function',
    };
    ",
  );
  assert_eq!(
    v["hasNewPageFn"].as_bool(),
    Some(true),
    "context event must resolve a BrowserContext: {v}"
  );
  assert_eq!(v["hasCookiesFn"].as_bool(), Some(true), "{v}");
}

pub fn register(set: &mut super::super::TestSet<'_>) {
  set.run(
    "backends_support::context_events::test_context_framenavigated",
    test_context_framenavigated,
  );
  set.run(
    "backends_support::context_events::test_context_frameattached",
    test_context_frameattached,
  );
  set.run(
    "backends_support::context_events::test_context_pageload",
    test_context_pageload,
  );
  set.run(
    "backends_support::context_events::test_context_pageclose",
    test_context_pageclose,
  );
  set.run(
    "backends_support::context_events::test_browser_context_event",
    test_browser_context_event,
  );
}
