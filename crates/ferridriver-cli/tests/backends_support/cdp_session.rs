//! Rule-9 integration tests for the raw `CDPSession` surface
//! (`browserContext.newCDPSession(page)` / `browser.newBrowserCDPSession()`)
//! through QuickJS `run_script`. Chromium backends get a live session
//! (send + events + detach); WebKit/BiDi must reject with the typed
//! Unsupported error.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::needless_pass_by_value)]

use super::client::McpClient;

fn is_chromium(backend: &str) -> bool {
  backend.starts_with("cdp")
}

/// `newCDPSession(page).send()` executes raw protocol commands, protocol
/// events reach listeners, and `detach()` ends the session (further
/// sends reject). Non-Chromium backends reject creation.
pub fn test_cdp_session_page(c: &mut McpClient) {
  c.nav("<body>cdp</body>");
  if !is_chromium(&c.backend) {
    let v = c.script_value(
      r"
      let msg = '';
      try { await context.newCDPSession(page); } catch (e) { msg = String(e); }
      return { msg };
      ",
    );
    assert!(
      v["msg"].as_str().unwrap_or("").contains("Chromium"),
      "newCDPSession must reject with the typed Unsupported on {}: {v}",
      c.backend
    );
    return;
  }
  let v = c.script_value(
    r"
    const session = await context.newCDPSession(page);
    const evalResult = await session.send('Runtime.evaluate', { expression: '6 * 7', returnByValue: true });

    await session.send('Page.enable');
    const loadFired = new Promise((resolve) => {
      session.once('Page.loadEventFired', (params) => resolve(typeof params.timestamp === 'number'));
    });
    await page.goto('data:text/html,<title>cdp-session</title>');
    const eventOk = await loadFired;

    await session.detach();
    let sendAfterDetach = '';
    try { await session.send('Runtime.evaluate', { expression: '1' }); } catch (e) { sendAfterDetach = String(e); }
    let doubleDetach = '';
    try { await session.detach(); } catch (e) { doubleDetach = String(e); }
    return {
      value: evalResult.result.value,
      eventOk,
      sendAfterDetach,
      doubleDetach,
    };
    ",
  );
  assert_eq!(
    v["value"].as_i64(),
    Some(42),
    "raw Runtime.evaluate must return the protocol result: {v}"
  );
  assert_eq!(
    v["eventOk"].as_bool(),
    Some(true),
    "Page.loadEventFired must reach the session listener with params: {v}"
  );
  assert!(
    v["sendAfterDetach"].as_str().unwrap_or("").contains("detached"),
    "send after detach must reject: {v}"
  );
  assert!(
    v["doubleDetach"].as_str().unwrap_or("").contains("detached"),
    "double detach must reject like Playwright: {v}"
  );
}

/// `browser.newBrowserCDPSession()` attaches to the browser target;
/// browser-domain commands work. Non-Chromium backends reject.
pub fn test_cdp_session_browser(c: &mut McpClient) {
  if !is_chromium(&c.backend) {
    let v = c.script_value(
      r"
      let msg = '';
      try { await browser.newBrowserCDPSession(); } catch (e) { msg = String(e); }
      return { msg };
      ",
    );
    assert!(
      v["msg"].as_str().unwrap_or("").contains("Chromium"),
      "newBrowserCDPSession must reject with the typed Unsupported on {}: {v}",
      c.backend
    );
    return;
  }
  let v = c.script_value(
    r"
    const session = await browser.newBrowserCDPSession();
    const version = await session.send('Browser.getVersion');
    await session.detach();
    return { product: String(version.product || '') };
    ",
  );
  assert!(
    v["product"].as_str().unwrap_or("").contains("Chrome"),
    "Browser.getVersion via the raw session must return the product: {v}"
  );
}

pub fn register(set: &mut super::super::TestSet<'_>) {
  set.run(
    "backends_support::cdp_session::test_cdp_session_page",
    test_cdp_session_page,
  );
  set.run(
    "backends_support::cdp_session::test_cdp_session_browser",
    test_cdp_session_browser,
  );
}
