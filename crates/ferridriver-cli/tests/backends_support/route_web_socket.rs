//! Integration tests for `page.routeWebSocket` / `context.routeWebSocket`
//! through QuickJS `run_script`, on every backend.
//!
//! The QuickJS engine has no upstream WebSocket server in-process, so these
//! exercise the fully-mocked path (handler `onMessage` + `send`, page never
//! reaches a real server) — no real `ws://` endpoint is required. The
//! `connectToServer()` passthrough path is covered by the NAPI `bun test`
//! against a real Bun WebSocket server.
//!
//! The page runs on a real `http://` origin, so on WebKit the navigation
//! crosses a process boundary and swaps the target session — this exercises
//! both the binding-channel replay
//! (`backend/webkit/events::handle_provisional_target_created`) and the
//! main-world-anchored driver→page dispatch (`WsRouteState::dispatch` uses
//! `call_utility_evaluate`, matching the context the socket was created in,
//! like Playwright's `frame.evaluateExpression`).
//!
//! The reply is observed with the idiomatic Playwright single-await shape: one
//! `page.evaluate` returns a page-side promise resolved by the driver→page WS
//! dispatch while the script execute is parked on that await. This is the
//! regression shape for the engine's single-owner VM event loop
//! (`ferridriver-script::vm`): without it, the schedular wake for the
//! resolved evaluate is lost and the execute hangs.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::needless_pass_by_value)]

use super::client::McpClient;

/// Navigate to the http page, create a mocked socket inside a single awaited
/// `page.evaluate`, and return the echoed reply the route handler produced
/// via `onMessage` + `send`. The handler must already be installed.
fn run_mocked_echo(c: &mut McpClient, page_url: &str, ws_url: &str) -> serde_json::Value {
  c.script_value_with_args(
    r"
    const [pageUrl, wsUrl] = args;
    await page.goto(pageUrl);
    return await page.evaluate((u) => new Promise((resolve) => {
      const ws = new WebSocket(u);
      ws.onopen = () => ws.send('hi');
      ws.onmessage = (e) => resolve(e.data);
    }), wsUrl);
    ",
    serde_json::json!([page_url, ws_url]),
  )
}

/// `page.routeWebSocket` fully-mocked path: the handler sets `onMessage` to
/// echo back a prefixed reply; the page never reaches a real server.
/// Exercises onCreate dispatch, ensureOpened, page→driver message, and the
/// driver→page `send` (through the interpreter-thread WS pump) resolving a
/// page-side promise the script is awaiting.
pub fn test_page_route_web_socket(c: &mut McpClient) {
  let port = super::spawn_html_server();
  c.script_value(
    r"
    await page.routeWebSocket('ws://ferri.invalid/mock', (ws) => {
      ws.onMessage((m) => ws.send('mocked:' + m));
    });
    return true;
    ",
  );
  let got = run_mocked_echo(c, &format!("http://127.0.0.1:{port}/p"), "ws://ferri.invalid/mock");
  assert_eq!(
    got.as_str(),
    Some("mocked:hi"),
    "page.routeWebSocket mock should echo via onMessage/send into a single awaited evaluate"
  );
}

/// `context.routeWebSocket` fully-mocked path: same echo handler, but
/// registered at the context level so it applies to the context's page.
/// Proves the context-level fan-out reaches the same WS mock + pump.
pub fn test_context_route_web_socket(c: &mut McpClient) {
  let port = super::spawn_html_server();
  c.script_value(
    r"
    await context.routeWebSocket('ws://ferri.invalid/ctxmock', (ws) => {
      ws.onMessage((m) => ws.send('ctx:' + m));
    });
    return true;
    ",
  );
  let got = run_mocked_echo(c, &format!("http://127.0.0.1:{port}/cp"), "ws://ferri.invalid/ctxmock");
  assert_eq!(
    got.as_str(),
    Some("ctx:hi"),
    "context.routeWebSocket mock should echo via onMessage/send into a single awaited evaluate"
  );
}

/// A socket created INSIDE a same-origin iframe: the `onCreate` binding
/// call carries the iframe as its `BindingSource.frame`, and every
/// driver→page dispatch (`ensureOpened`, `send`) must evaluate in THAT
/// frame — the iframe realm has its own `WebSocket` mock and
/// `idToWebSocket` map, so a main-frame dispatch silently strands the
/// socket. Mirrors Playwright's `source.frame.evaluateExpression`
/// anchoring in `webSocketRouteDispatcher.ts`. The echo is observed via
/// a single awaited `frame.evaluate`.
pub fn test_page_route_web_socket_in_iframe(c: &mut McpClient) {
  let port = super::spawn_html_server();
  c.script_value(
    r"
    await page.routeWebSocket('ws://ferri.invalid/frame-mock', (ws) => {
      ws.onMessage((m) => ws.send('frame:' + m));
    });
    return true;
    ",
  );
  let got = c.script_value_with_args(
    r"
    const [pageUrl, wsUrl] = args;
    await page.goto(pageUrl);
    await page.waitForSelector('iframe');
    let frame = null;
    for (let i = 0; i < 50; i++) {
      const fs = page.frames();
      if (fs.length > 1) { frame = fs[1]; break; }
      await page.waitForTimeout(100);
    }
    if (!frame) throw new Error('iframe never appeared in page.frames()');
    return await frame.evaluate((u) => new Promise((resolve) => {
      const ws = new WebSocket(u);
      ws.onopen = () => ws.send('hi');
      ws.onmessage = (e) => resolve(e.data);
    }), wsUrl);
    ",
    serde_json::json!([
      format!("http://127.0.0.1:{port}/iframe"),
      "ws://ferri.invalid/frame-mock"
    ]),
  );
  assert_eq!(
    got.as_str(),
    Some("frame:hi"),
    "routeWebSocket must intercept a socket created inside an iframe and dispatch the mocked reply back into that frame"
  );
}

pub fn register(set: &mut super::super::TestSet<'_>) {
  set.run(
    "backends_support::route_web_socket::test_page_route_web_socket",
    test_page_route_web_socket,
  );
  set.run(
    "backends_support::route_web_socket::test_context_route_web_socket",
    test_context_route_web_socket,
  );
  set.run(
    "backends_support::route_web_socket::test_page_route_web_socket_in_iframe",
    test_page_route_web_socket_in_iframe,
  );
}
