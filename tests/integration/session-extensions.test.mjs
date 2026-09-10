import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { fixtureServer, observation, runtimeProbe } from './support.mjs';
import { plugins, scripts } from './fixtures/session-extensions.mjs';

async function execute(files, calls, { host = 'script', httpClient = false, entries = Object.keys(files) } = {}) {
  const { results } = await runtimeProbe([
    { op: 'configure-context', host },
    { op: 'compile-extensions', groups: entries.map(entry => [entry]), httpClient },
    ...calls.flatMap(call => [typeof call === 'string' ? { op: 'execute-script', source: call } : call,
      { op: 'session-state' }]),
  ], files);
  observation(results[0]);
  const compiled = observation(results[1]);
  assert.deepEqual(compiled.failures, []);
  assert.equal(compiled.compiled.length, entries.length);
  for (const extension of compiled.compiled) assert.ok(extension.bytecodeLength > 0);
  return { compiled: compiled.compiled, calls: calls.map((_, i) => ({
    result: observation(results[2 + i * 2]), ...observation(results[3 + i * 2]),
  })) };
}

async function scenario(name, options = {}) {
  return execute({ 'extension.ts': plugins[name] }, scripts[name], options);
}

function value(call) {
  assert.equal(call.result.status, 'ok', JSON.stringify(call));
  assert.equal(call.poisoned, false);
  return call.result.value;
}

function error(call) {
  assert.equal(call.result.status, 'error', JSON.stringify(call));
  return call.result.error.message;
}

function denied(call) {
  assert.match(error(call), /permission denied/);
  assert.match(error(call), /blocked\.test/);
}

function allowed(call) {
  if (call.result.status === 'error') assert.doesNotMatch(error(call), /permission denied/);
  else value(call);
}

test('session extensions project dotted tools into every supported namespace', async () => {
  const { calls } = await execute({ 'extension.ts': plugins.NAMESPACED_PLUGIN },
    scripts.dotted_tool_names_are_projected_as_namespaces);
  assert.deepEqual(value(calls[0]), Object.fromEntries(
    ['flat', 'nested', 'tools', 'ferridriver', 'global'].map((key, i) => [key, { ok: true, user: 'abcde'[i] }])));
});

test('session extensions bundle local TypeScript imports before installation', async () => {
  const { calls } = await execute({
    'helper.ts': 'export const tag = (n: number): string => `t${n}`;\n',
    'extension.ts': `import { tag } from './helper';
      interface In { n: number }
      defineTool({ name: 'ts', exposeAsMcpTool: true,
        async handler({ args }: { args: In }) { return { tag: tag(args.n) }; } });`,
  }, scripts.typescript_plugin_with_local_import_bundles_and_runs, { entries: ['extension.ts'] });
  assert.deepEqual(value(calls[0]), { tag: 't7' });
});

for (const name of ['allow_net_capability_is_enforced_on_the_request_binding',
  'allow_net_capability_is_enforced_on_the_handler_fetch']) {
  test(`session extensions ${name.replaceAll('_', ' ')}`, async () => {
    await fixtureServer(async base => {
      const tool = name.endsWith('request_binding') ? 'net' : 'netf';
      const { calls } = await execute({ 'extension.ts': plugins[name] }, [...scripts[name],
        `return await tools['${tool}']({ url: ${JSON.stringify(`${base}/fx/landed`)} });`,
      ], { httpClient: true });
      denied(calls[0]);
      allowed(calls[1]);
      assert.equal(value(calls[2]), tool === 'net' ? 'ok' : 200);
    });
  });
}

test('session extensions keep concurrent handler fetch policies independent', async () => {
  const { calls } = await scenario('fetch_net_policy_does_not_leak_between_concurrent_tools', { httpClient: true });
  const result = value(calls[0]);
  assert.match(result.restricted, /denied:/);
  assert.match(result.restricted, /permission denied/);
  assert.doesNotMatch(result.open, /permission denied/);
});

test('session extensions register host-conditional contributions in the matching host', async () => {
  const name = 'extension_branches_on_ferridriver_host_flag';
  for (const [host, source, expected] of [['mcp', scripts[name][0], 'tool-ran'],
    ['bdd', scripts[name][1], 'undefined']]) {
    const { calls } = await execute({ 'extension.ts': plugins[name] }, [source], { host });
    assert.equal(value(calls[0]), expected);
  }
});

test('session extensions install bytecode once and retain handler state across calls', async () => {
  const { calls } = await execute({ 'extension.ts': plugins.DEMO_PLUGIN }, [
    "return await tools['demo']({ x: 1 });", "return await tools['demo']({ x: 2 });",
  ]);
  assert.deepEqual(calls.map(value), [{ n: 1, got: { x: 1 } }, { n: 2, got: { x: 2 } }]);
});

test('session extensions reject duplicate and empty tool names during extraction', async () => {
  for (const [source, message] of [
    [plugins.duplicate_tool_name_is_rejected_at_load, 'duplicate tool name `dup`'],
    ["defineTool({ name: '  ', handler: async () => 1 });\n", 'non-empty string'],
  ]) {
    const { results } = await runtimeProbe([{ op: 'compile-extensions', groups: [['extension.ts']] }],
      { 'extension.ts': source });
    const result = observation(results[0]);
    assert.equal(result.compiled.length, 0);
    assert.equal(result.failures.length, 1);
    assert.ok(JSON.stringify(result.failures).includes(message), JSON.stringify(result));
  }
});

test('session extensions enforce per-tool deadlines through JS calls', async () => {
  const { calls } = await scenario('per_tool_timeout_ms_is_enforced_for_every_caller');
  assert.match(value(calls[0]), /timed out after 50ms/);
  assert.equal(value(calls[1]), 'quick');
});

test('session extensions await top-level tool registration before executing callers', async () => {
  const { calls } = await execute({ 'extension.ts': `const v = await Promise.resolve('deferred');
    defineTool({ name: 'late', handler: async () => v });` }, scripts.plugin_top_level_await_registers_tools_in_session);
  assert.equal(value(calls[0]), 'deferred');
});

test('session extensions isolate a broken installation from its healthy sibling', async () => {
  const { calls } = await execute({
    'bad.js': `if (globalThis.ferridriver?.host === 'script') { throw new Error('boom'); }
      defineTool({ name: 'bad', handler: async () => 'never' });`,
    'good.js': "defineTool({ name: 'good', handler: async () => 'fine' });",
  }, [...scripts.broken_plugin_is_skipped_without_killing_the_session, "return typeof tools['bad'];"]);
  assert.equal(value(calls[0]), 'fine');
  assert.equal(value(calls[1]), 'undefined');
});

test('session extensions attenuate handler requests without changing global authority', async () => {
  const { calls } = await execute({ 'extension.ts': plugins.NET_GLOBAL },
    scripts.allow_net_capability_attenuates_the_handler_request_not_the_global, { httpClient: true });
  denied(calls[0]);
  allowed(calls[1]);
  allowed(calls[2]);
  assert.doesNotMatch(value(calls[3]), /allow\.net|permission denied/);
});

test('session extensions retain fetch attenuation through timer callbacks', async () => {
  const { calls } = await scenario('allow_net_follows_the_capability_into_a_timer_callback', { httpClient: true });
  assert.match(value(calls[0]), /denied:/);
  assert.match(value(calls[0]), /permission denied/);
  assert.doesNotMatch(value(calls[1]), /allow\.net|permission denied/);
});

test('session extensions provide the same standard globals during extraction and execution', async () => {
  const { calls, compiled } = await scenario('extraction_environment_matches_session_for_top_level_globals');
  assert.ok(compiled[0].manifests.includes('"ambient"'));
  assert.deepEqual(value(calls[0]), { len: 2, hasId: true });
});

test('session extensions share a persistent registry across native and JS tool calls', async () => {
  const { calls } = await execute({ 'extension.ts': plugins.DEMO_PLUGIN }, [
    { op: 'execute-tool', name: 'demo', args: { x: 1 } },
    ...scripts.execute_tool_invokes_natively_and_reports_missing_tools,
    { op: 'execute-tool', name: 'nope', args: {} },
  ]);
  assert.deepEqual(value(calls[0]), { n: 1, got: { x: 1 } });
  assert.deepEqual(value(calls[1]), { n: 2, got: { x: 2 } });
  assert.match(error(calls[2]), /`nope`/);
  assert.match(error(calls[2]), /not installed/);
});

test('session extensions propagate native handler failures and enforce tool deadlines', async () => {
  const { calls } = await execute({ 'extension.ts': plugins.execute_tool_propagates_handler_failures_and_timeouts }, [
    { op: 'execute-tool', name: 'boom', args: {} },
    { op: 'execute-tool', name: 'slow', args: {} },
  ]);
  assert.match(error(calls[0]), /handler exploded/);
  assert.equal(calls[0].poisoned, false);
  assert.match(error(calls[1]), /timed out after 50ms/);
});
