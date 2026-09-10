export const scripts = {
  "dotted_tool_names_are_projected_as_namespaces": [
    "return { flat: await tools['acme.login']({ user: 'a' }), nested: await tools.acme.login({ user: 'b' }), tools: await tools.acme.login({ user: 'c' }), ferridriver: await ferridriver.tools.acme.login({ user: 'd' }), global: await acme.login({ user: 'e' }) };"
  ],
  "typescript_plugin_with_local_import_bundles_and_runs": [
    "return await tools['ts']({ n: 7 });"
  ],
  "allow_net_capability_is_enforced_on_the_request_binding": [
    "return await tools['net']({ url: 'http://blocked.test/' });",
    "return await tools['net']({ url: 'http://127.0.0.1:1/' });"
  ],
  "allow_net_capability_is_enforced_on_the_handler_fetch": [
    "return await tools['netf']({ url: 'http://blocked.test/' });",
    "return await tools['netf']({ url: 'http://127.0.0.1:1/' });"
  ],
  "fetch_net_policy_does_not_leak_between_concurrent_tools": [
    "const [a, b] = await Promise.all([ tools['restricted']({ url: 'http://blocked.test/' }), tools['open']({ url: 'http://127.0.0.1:1/' }) ]); return { restricted: a, open: b };"
  ],
  "extension_branches_on_ferridriver_host_flag": [
    "return await tools['mcpOnly']({});",
    "return typeof tools['mcpOnly'];"
  ],
  "per_tool_timeout_ms_is_enforced_for_every_caller": [
    "try { await tools['slow'](); return 'resolved'; } catch (e) { return String(e); }",
    "return await tools['fast']();"
  ],
  "plugin_top_level_await_registers_tools_in_session": [
    "return await tools['late']();"
  ],
  "broken_plugin_is_skipped_without_killing_the_session": [
    "return await tools['good']();"
  ],
  "allow_net_capability_attenuates_the_handler_request_not_the_global": [
    "return await tools['netg']({ url: 'http://blocked.test/' });",
    "return await tools['netg']({ url: 'http://blocked.test/', via: 'global' });",
    "return await tools['netg']({ url: 'http://127.0.0.1:1/' });",
    "try { await globalThis.request.get('http://blocked.test/'); return 'reached'; } catch (e) { return String(e.message || e); }"
  ],
  "allow_net_follows_the_capability_into_a_timer_callback": [
    "return await tools['nett']({ url: 'http://blocked.test/' });",
    "return await new Promise((resolve) => setTimeout(async () => { try { await fetch('http://127.0.0.1:1/'); resolve('reached'); } catch (e) { resolve('err:' + String(e.message || e)); } }, 10));"
  ],
  "extraction_environment_matches_session_for_top_level_globals": [
    "return await tools['ambient']();"
  ],
  "execute_tool_invokes_natively_and_reports_missing_tools": [
    "return await tools['demo']({ x: 2 });"
  ]
};

export const plugins = {
  "allow_net_capability_is_enforced_on_the_request_binding": "defineTool({ name: 'net', allow: { net: ['127.0.0.1'] }, handler: async ({ args, request }) => { await request.get(args.url); return 'ok'; } });",
  "allow_net_capability_is_enforced_on_the_handler_fetch": "defineTool({ name: 'netf', allow: { net: ['127.0.0.1'] }, handler: async ({ args, fetch }) => { const r = await fetch(args.url); return r.status; } });",
  "fetch_net_policy_does_not_leak_between_concurrent_tools": "defineTool({ name: 'restricted', allow: { net: ['127.0.0.1'] }, handler: async ({ args, fetch }) => { try { await fetch(args.url); return 'reached'; } catch (e) { return 'denied:' + String(e.message || e); } } }); defineTool({ name: 'open', handler: async ({ args, fetch }) => { try { await fetch(args.url); return 'reached'; } catch (e) { return 'err:' + String(e.message || e); } } });",
  "extension_branches_on_ferridriver_host_flag": "if (ferridriver.host === 'mcp') { defineTool({ name: 'mcpOnly', handler: async () => 'tool-ran' }); } if (ferridriver.host === 'bdd') { Given('a step', () => {}); }",
  "duplicate_tool_name_is_rejected_at_load": "defineTool({ name: 'dup', handler: async () => 1 });\ndefineTool({ name: 'dup', handler: async () => 2 });\n",
  "per_tool_timeout_ms_is_enforced_for_every_caller": "defineTool({ name: 'slow', timeoutMs: 50, handler: async () => { await new Promise(r => setTimeout(r, 400)); return 'late'; } });\ndefineTool({ name: 'fast', timeoutMs: 5000, handler: async () => 'quick' });\n",
  "allow_net_follows_the_capability_into_a_timer_callback": "defineTool({ name: 'nett', allow: { net: ['127.0.0.1'] }, handler: ({ args, fetch }) => new Promise((resolve) => { setTimeout(async () => { try { await fetch(args.url); resolve('reached'); } catch (e) { resolve('denied:' + String(e.message || e)); } }, 10); }) });",
  "extraction_environment_matches_session_for_top_level_globals": "console.log('extension booting');\nconst enc = new TextEncoder().encode('hi');\nconst id = crypto.randomUUID();\nexpect(enc.length).toBe(2);\nawait new Promise((r) => setTimeout(r, 5));\ndefineTool({ name: 'ambient', handler: async () => ({ len: enc.length, hasId: id.length > 0 }) });\n",
  "execute_tool_propagates_handler_failures_and_timeouts": "defineTool({ name: 'boom', handler: async () => { throw new Error('handler exploded'); } });\ndefineTool({ name: 'slow', timeoutMs: 50, handler: async () => { await new Promise(r => setTimeout(r, 400)); return 'late'; } });\n",
  "DEMO_PLUGIN": "defineTool({ name: 'demo', handler: async ({ args }) => { globalThis.__n = (globalThis.__n || 0) + 1; return { n: globalThis.__n, got: args }; } });",
  "NAMESPACED_PLUGIN": "defineTool({ name: 'acme.login', handler: async ({ args }) => ({ ok: true, user: args.user }) });",
  "NET_GLOBAL": "defineTool({ name: 'netg', allow: { net: ['127.0.0.1'] }, handler: async ({ args, request }) => { if (args.via === 'global') { await globalThis.request.get(args.url); return 'ok'; } await request.get(args.url); return 'ok'; } });"
};
