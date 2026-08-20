//! What an extension handler receives.
//!
//! A handler used to get a strict subset of a plain `run_script`'s
//! capabilities: `{ args, page, context, request, commands, signal }`
//! and nothing else. The gaps forced real workarounds in shipped
//! extensions:
//!
//! - no `session`, so a tool took the environment as an argument and
//!   could silently disagree with the browser it was handed;
//! - no `vars`, so cross-call state lived in module-level variables that
//!   vanish when the session VM is reaped;
//! - no `settings`, so deployment values were smuggled through tool
//!   arguments or an allow-listed environment variable;
//! - no `artifacts`/`fs`/`sidecars`, all of which a script could reach.

use serde_json::json;

use super::client::McpClient;

const EXTENSION_SRC: &str = r"
defineTool({
  name: 'ctxprobe.surface',
  description: 'Reports the handler context surface',
  exposeAsTool: true,
  inputSchema: { type: 'object', properties: {} },
  async handler(ctx) {
    return {
      // Session identity, split the same way the server routes it.
      sessionKey: ctx.session ? ctx.session.key : null,
      instance: ctx.session ? ctx.session.instance : null,
      contextName: ctx.session ? ctx.session.context : null,
      // Operator-supplied settings for this extension.
      settingsEnv: ctx.settings ? ctx.settings.env : null,
      settingsOrigin: ctx.settings ? ctx.settings.origin : null,
      // Capabilities that must be present, not just truthy names.
      hasVars: typeof ctx.vars?.set === 'function',
      hasFs: typeof ctx.fs?.readFileSync === 'function' && typeof ctx.fs?.promises?.readFile === 'function',
      hasArtifacts: typeof ctx.artifacts?.writeBytes === 'function',
      hasSidecars: typeof ctx.sidecars?.connect === 'function',
      hasLog: typeof ctx.log === 'function',
      logLevels: ['error', 'warn', 'info', 'debug', 'trace'].filter((l) => typeof ctx.log?.[l] === 'function'),
      // `log.enabled(level)` reflects the operator's tracing filter, so
      // a handler can skip building a payload nobody will record.
      hasEnabled: typeof ctx.log?.enabled === 'function',
      errorEnabled: ctx.log?.enabled('error'),
      bogusLevelEnabled: ctx.log?.enabled('nonsense'),
      hasCommands: typeof ctx.commands?.run === 'function',
      hasPage: typeof ctx.page?.goto === 'function',
    };
  },
});

defineTool({
  name: 'ctxprobe.remember',
  description: 'Writes a value through the durable session store',
  exposeAsTool: true,
  inputSchema: { type: 'object', properties: { value: { type: 'string' } } },
  async handler({ args, vars, log }) {
    log(`remembering ${args.value}`);
    log.debug('debug detail', { value: args.value });
    log.warn('warn detail');
    vars.set('ctxprobe', args.value);
    return { stored: args.value };
  },
});

defineTool({
  name: 'ctxprobe.recall',
  description: 'Reads back what a previous call stored',
  exposeAsTool: true,
  inputSchema: { type: 'object', properties: {} },
  async handler({ vars }) {
    return { recalled: vars.get('ctxprobe') ?? null };
  },
});
";

fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
  let dir = tempfile::tempdir().expect("tempdir");
  let ext = dir.path().join("ctxprobe.js");
  std::fs::write(&ext, EXTENSION_SRC).expect("write extension");
  let config = dir.path().join("ferridriver.toml");
  std::fs::write(
    &config,
    format!(
      "[extensions]\n\
       paths = [{}]\n\
       \n\
       [extensions.settings.ctxprobe]\n\
       env = \"staging\"\n\
       origin = \"https://example.test\"\n\
       \n\
       [mcp.browser]\n\
       headless = true\n\
       \n\
       [mcp.browser.instances.staging]\n\
       args = []\n",
      serde_json::to_string(&ext.display().to_string()).expect("json path")
    ),
  )
  .expect("write config");
  (dir, config)
}

pub fn run() {
  let (_dir, config) = fixture();
  let mut c = McpClient::with_config("cdp-pipe", &config);

  context_carries_session_settings_and_capabilities(&mut c);
  vars_survive_across_calls(&mut c);
}

fn context_carries_session_settings_and_capabilities(c: &mut McpClient) {
  let res = c.call_tool("ctxprobe.surface", json!({"session": "staging:admin"}));
  assert_ne!(res["result"]["isError"], true, "surface probe failed: {res}");
  let out = &res["result"]["structuredContent"];
  let out = if out.is_null() {
    // No outputSchema declared, so read the text payload.
    let text = res["result"]["content"]
      .as_array()
      .and_then(|c| c.last())
      .and_then(|c| c["text"].as_str())
      .unwrap_or_default();
    serde_json::from_str::<serde_json::Value>(text).unwrap_or_else(|e| panic!("payload {text:?}: {e}"))
  } else {
    out.clone()
  };
  let value = out.get("value").unwrap_or(&out);

  assert_eq!(value["sessionKey"], "staging:admin", "session key: {value}");
  assert_eq!(value["instance"], "staging", "instance half: {value}");
  assert_eq!(value["contextName"], "admin", "context half: {value}");
  assert_eq!(value["settingsEnv"], "staging", "settings block: {value}");
  assert_eq!(value["settingsOrigin"], "https://example.test", "settings: {value}");

  assert_eq!(
    value["logLevels"],
    json!(["error", "warn", "info", "debug", "trace"]),
    "log must expose every tracing level, not just info: {value}"
  );
  assert_eq!(value["errorEnabled"], true, "error level is on by default: {value}");
  assert_eq!(
    value["bogusLevelEnabled"], false,
    "an unknown level must not report itself enabled: {value}"
  );

  for key in [
    "hasVars",
    "hasEnabled",
    "hasFs",
    "hasArtifacts",
    "hasSidecars",
    "hasLog",
    "hasCommands",
    "hasPage",
  ] {
    assert_eq!(value[key], true, "{key} must be present: {value}");
  }
}

/// Durable state must live in `vars`, which survives a VM rebuild —
/// unlike the module-level variables extensions were forced to use.
fn vars_survive_across_calls(c: &mut McpClient) {
  let res = c.call_tool(
    "ctxprobe.remember",
    json!({"value": "abc123", "session": "staging:admin"}),
  );
  assert_ne!(res["result"]["isError"], true, "remember: {res}");

  let res = c.call_tool("ctxprobe.recall", json!({"session": "staging:admin"}));
  assert_ne!(res["result"]["isError"], true, "recall: {res}");
  let text = serde_json::to_string(&res["result"]).expect("json");
  assert!(text.contains("abc123"), "value must survive the call boundary: {text}");
}
