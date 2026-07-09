//! MCP-boundary end-to-end tests for the extension system: a real
//! server launched with a config that loads an extension file, spoken
//! to over the actual MCP protocol.
//!
//! Two halves:
//! - `mcp_surface`: promoted-tool metadata on the wire (`tools/list`
//!   `title` / `annotations` / `inputSchema` / `outputSchema`),
//!   `tools/call` success with `structuredContent`, input-schema
//!   rejection, output-schema violation as a tool error, the reserved
//!   `session` routing key coexisting with `additionalProperties:
//!   false`, and the `ferridriver_extensions` introspection payload.
//! - `capability_follows_registrar`: the page-callback half of the
//!   "capability follows the registrar" invariant — a net-restricted
//!   tool registers a `page.on` listener (sync dispatch), a `page.route`
//!   handler (async dispatch, off the synchronous call), and a
//!   `page.exposeFunction` binding, each triggered after the tool
//!   returned; each must still be denied the un-declared host, while a
//!   top-level-registered callback stays unrestricted. Every page
//!   callback funnels through the same `SavedCallback::save_with_net` +
//!   `bracket_net` path (`routeWebSocket`/`startScreencast` included);
//!   these three cover the sync-listener, async-continuation, and
//!   binding dispatch shapes deterministically. The timer/microtask
//!   rows live browser-free in
//!   `ferridriver-script/tests/extension_policy.rs`.
//!
//! Chromium-only (`cdp-pipe`): the invariant under test is VM-side
//! policy bracketing, not backend protocol behaviour.

use serde_json::json;

use super::client::{McpClient, extract_text, is_error};

const EXTENSION_SRC: &str = r"
defineTool({
  name: 'typed_greet',
  title: 'Typed Greeter',
  description: 'Greets a user',
  exposeAsTool: true,
  annotations: { readOnlyHint: true, openWorldHint: false },
  inputSchema: {
    type: 'object',
    properties: { user: { type: 'string' } },
    required: ['user'],
    additionalProperties: false,
  },
  outputSchema: {
    type: 'object',
    properties: { greeting: { type: 'string' } },
    required: ['greeting'],
    additionalProperties: false,
  },
  handler: async ({ args }) => ({ greeting: 'hi ' + args.user }),
});

defineTool({
  name: 'bad_output',
  exposeAsTool: true,
  outputSchema: { type: 'object', properties: { ok: { type: 'boolean' } }, required: ['ok'] },
  handler: async () => ({ ok: 'not-a-boolean' }),
});

defineTool({
  name: 'cap_register',
  allow: { net: ['127.0.0.1'] },
  // Every registration CALL is made on the handler's synchronous prefix
  // (before the returned promise is awaited), so each one's sync net
  // snapshot runs on this tool's grant. The returned promises are then
  // awaited SEQUENTIALLY — the calls captured the grant already, and
  // sequential awaits avoid racing the per-document binding-channel
  // bootstrap that backs exposeFunction. The callbacks fire later,
  // cross-task — they must keep the captured grant.
  handler: ({ page }) => {
    globalThis.__cap = {};
    const probe = async (k) => {
      try {
        await fetch('http://blocked.test/');
        globalThis.__cap[k] = 'ALLOWED';
      } catch (e) {
        globalThis.__cap[k] = String((e && e.message) || e);
      }
    };
    page.on('console', (m) => { if (m.text() === 'cap-console') probe('pageOn'); });
    const pRoute = page.route('**/cap-route**', async (r) => {
      await probe('route');
      await r.fulfill({ status: 200, body: 'ok' });
    });
    const pExpose = page.exposeFunction('__capProbe', async () => { await probe('exposeFn'); return 1; });
    return (async () => {
      await pRoute;
      await pExpose;
      return 'registered';
    })();
  },
});
";

/// Write the extension file + a config whose `extensions` list points
/// at it (absolute path, so the server's cwd doesn't matter).
fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
  let dir = tempfile::tempdir().expect("tempdir");
  let ext = dir.path().join("ext.js");
  std::fs::write(&ext, EXTENSION_SRC).expect("write extension");
  let config = dir.path().join("ferridriver.toml");
  std::fs::write(
    &config,
    format!(
      "extensions = [{}]\n",
      serde_json::to_string(&ext.display().to_string()).expect("json path")
    ),
  )
  .expect("write config");
  (dir, config)
}

pub fn run() {
  let (_dir, config) = fixture();
  let mut c = McpClient::with_config("cdp-pipe", &config);
  mcp_surface(&mut c);
  capability_follows_registrar(&mut c);
}

fn mcp_surface(c: &mut McpClient) {
  let list = c.send_request("tools/list", json!({}));
  let tools = list["result"]["tools"].as_array().expect("tools array");
  let find = |name: &str| tools.iter().find(|t| t["name"] == name);

  let typed = find("typed_greet").expect("typed_greet promoted");
  assert_eq!(typed["title"], "Typed Greeter", "title on the wire: {typed}");
  assert_eq!(typed["annotations"]["readOnlyHint"], true, "annotations: {typed}");
  assert_eq!(typed["annotations"]["openWorldHint"], false, "annotations: {typed}");
  assert_eq!(typed["inputSchema"]["required"][0], "user", "inputSchema: {typed}");
  assert_eq!(
    typed["outputSchema"]["required"][0], "greeting",
    "outputSchema: {typed}"
  );
  assert!(find("bad_output").is_some(), "bad_output promoted");
  assert!(
    find("cap_register").is_none(),
    "exposeAsTool: false must stay out of tools/list"
  );

  // Success: structuredContent carries the conforming result. The
  // reserved `session` routing key must coexist with the tool's
  // `additionalProperties: false` schema.
  let good = c.call_tool("typed_greet", json!({ "user": "bob", "session": "default" }));
  assert!(!is_error(&good), "conforming call must succeed: {good}");
  assert_eq!(
    good["result"]["structuredContent"]["greeting"], "hi bob",
    "structuredContent must carry the validated result: {good}"
  );
  assert!(
    extract_text(&good).contains("hi bob"),
    "text payload still present: {good}"
  );

  // Input-schema rejection happens before the handler runs.
  let missing = c.call_tool("typed_greet", json!({}));
  assert!(is_error(&missing), "missing required arg must be a tool error");
  assert!(
    extract_text(&missing).contains("invalid arguments"),
    "input violation names the problem: {missing}"
  );
  let extra = c.call_tool("typed_greet", json!({ "user": "bob", "extra": 1 }));
  assert!(is_error(&extra), "additionalProperties: false must reject extras");

  // Output-schema violation is the author's bug, surfaced as a tool error.
  let bad = c.call_tool("bad_output", json!({}));
  assert!(is_error(&bad), "non-conforming output must be a tool error: {bad}");
  assert!(
    extract_text(&bad).contains("outputSchema"),
    "output violation names the contract: {bad}"
  );

  // Introspection sees all three tools from the loaded file.
  let intro = c.call_tool("ferridriver_extensions", json!({}));
  let payload: serde_json::Value = serde_json::from_str(&extract_text(&intro)).expect("introspection JSON");
  assert_eq!(payload["count"], 3, "all three tools listed: {payload}");
  assert_eq!(payload["files"][0]["tools"][0]["name"], "typed_greet");
  assert_eq!(payload["errors"], json!([]));
  assert_eq!(payload["warnings"], json!([]));
}

fn capability_follows_registrar(c: &mut McpClient) {
  // A stable, already-loaded document: the binding channel that backs
  // `exposeFunction` bootstraps per-document, so registering onto a
  // freshly-navigating page races it. `data:` is enough — the probes
  // hit `fetch`/`WebSocket` to `*.invalid` hosts the tool intercepts,
  // not the page's origin.
  c.nav("<!doctype html><title>cap</title><body>cap</body>");

  let reg_payload = c.script("return await tools['cap_register']();");
  assert_eq!(
    reg_payload["status"].as_str(),
    Some("ok"),
    "cap_register must register callbacks: {reg_payload}"
  );
  assert_eq!(reg_payload["value"].as_str(), Some("registered"));

  // Control: the same probe registered OUTSIDE any tool must stay
  // unrestricted — proves the denials below come from the captured
  // grant, not a VM-wide policy.
  c.script_value(
    r"
    await page.exposeFunction('__ctlProbe', async () => {
      try { await fetch('http://blocked.test/'); globalThis.__cap.control = 'ALLOWED'; }
      catch (e) { globalThis.__cap.control = String((e && e.message) || e); }
      return 1;
    });
    return true;
    ",
  );

  let payload = c.script_with_timeout(
    r"
    await page.evaluate(`console.log('cap-console')`);
    await page.evaluate(`fetch('http://ferri.invalid/cap-route').catch(() => {})`);
    await page.evaluate('window.__capProbe()');
    await page.evaluate('window.__ctlProbe()');
    const want = ['pageOn', 'route', 'exposeFn', 'control'];
    const deadline = Date.now() + 20000;
    while (Date.now() < deadline && want.some((k) => !(k in globalThis.__cap))) {
      await new Promise((r) => setTimeout(r, 100));
    }
    return globalThis.__cap;
    ",
    45_000,
  );
  assert_eq!(payload["status"], "ok", "trigger script must run clean: {payload}");
  let cap = &payload["value"];

  for key in ["pageOn", "route", "exposeFn"] {
    let msg = cap[key]
      .as_str()
      .unwrap_or_else(|| panic!("`{key}` never fired: {cap}"));
    assert!(
      msg.contains("not in allow.net") && msg.contains("blocked.test"),
      "`{key}` callback must keep the registrar's allow.net, got: {msg}"
    );
  }
  let control = cap["control"].as_str().expect("control probe fired");
  assert!(
    !control.contains("allow.net"),
    "a top-level callback must stay unrestricted (expected a plain network error), got: {control}"
  );
}
