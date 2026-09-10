import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { observation, runtimeProbe } from './support.mjs';

async function invoke(extension, sources, sessionPolicy = {}) {
  const { results } = await runtimeProbe([{
    op: 'extension-session', entries: ['./extension.ts'], host: 'script', sources,
    httpClient: true, sessionPolicy,
  }], { 'extension.ts': extension });
  return results[0];
}

function error(outcome) {
  assert.equal(outcome.status, 'error', JSON.stringify(outcome));
  return outcome.error.message;
}

function value(outcome) {
  assert.equal(outcome.status, 'ok', JSON.stringify(outcome));
  return outcome.value;
}

for (const declared of [false, true]) {
  test(`the network ceiling ${declared ? 'clamps declared hosts' : 'restricts undeclared tools'}`, async () => {
    const blockedHost = declared ? 'evil.example' : 'blocked.test';
    const result = observation(await invoke(`defineTool({ name: 'request',
      ${declared ? "allow: { net: ['127.0.0.1', 'evil.example'] }," : ''}
      handler: async ({ args, request }) => { await request.get(args.url); return 'ok'; }
    });`, [
      `return await tools.request({ url: 'http://${blockedHost}/' });`,
      "return await tools.request({ url: 'http://127.0.0.1:1/' });",
    ], { net: ['127.0.0.1'] }));
    assert.deepEqual(result.failures, []);
    const denied = error(result.outcomes[0]);
    assert.ok(denied.includes('permission denied') && denied.includes(blockedHost), denied);
    if (result.outcomes[1].status === 'error') {
      assert.ok(!result.outcomes[1].error.message.includes('permission denied'), result.outcomes[1].error.message);
    } else {
      value(result.outcomes[1]);
    }
  });
}

test('an empty network ceiling denies every host to the handler fetch capability', async () => {
  const result = observation(await invoke(`defineTool({ name: 'noop',
    handler: async ({ args, fetch }) => { const response = await fetch(args.url); return response.status; }
  });`, ["return await tools.noop({ url: 'http://127.0.0.1:1/' });"], { net: [] }));
  assert.match(error(result.outcomes[0]), /permission denied/);
});

test('argvOnly refuses a compiled shell command as a session policy failure', async () => {
  const result = await invoke(`defineTool({ name: 'sh', allow: { commands: { echo: 'echo hi' } },
    handler: async ({ commands }) => commands.run('echo') });`, [], { commands: 'argvOnly' });
  assert.equal(typeof result.error, 'string', JSON.stringify(result));
  assert.ok(result.error.includes('extension.policy.refused') && result.error.includes('argvOnly'), result.error);
});

test('argvOnly allows argv commands to execute normally', async () => {
  const result = observation(await invoke(`defineTool({ name: 'argv', allow: { commands: { echo: { run: ['echo', 'hi'] } } },
    handler: async ({ commands }) => commands.run('echo') });`, ["return await tools.argv();"], { commands: 'argvOnly' }));
  assert.equal(value(result.outcomes[0]), 'hi');
});

test('the none command ceiling refuses a command-declaring extension', async () => {
  const result = await invoke(`defineTool({ name: 'cmd', allow: { commands: { echo: { run: ['echo', 'hi'] } } },
    handler: async ({ commands }) => commands.run('echo') });`, [], { commands: 'none' });
  assert.equal(typeof result.error, 'string', JSON.stringify(result));
  assert.ok(result.error.includes('extension.policy.refused'), result.error);
});

test('the none command ceiling leaves command-free tools usable', async () => {
  const result = observation(await invoke("defineTool({ name: 'plain', handler: async () => 'fine' });",
    ['return await tools.plain();'], { commands: 'none' }));
  assert.equal(value(result.outcomes[0]), 'fine');
});

test('a timed-out extension fires its AbortSignal with a TimeoutError reason', async () => {
  const result = observation(await invoke(`defineTool({ name: 'slow', timeoutMs: 200,
    handler: ({ signal }) => new Promise(() => {
      signal.addEventListener('abort', () => { globalThis.__abort_name = signal.reason.name; });
    }) });`, ['return await tools.slow();', 'return globalThis.__abort_name;']));
  assert.match(error(result.outcomes[0]), /timed out after 200ms/);
  assert.equal(value(result.outcomes[1]), 'TimeoutError');
});

test('a handler fetch capability retains its restriction inside a microtask', async () => {
  const result = observation(await invoke(`defineTool({ name: 'micro', allow: { net: ['127.0.0.1'] },
    handler: ({ fetch }) => new Promise(resolve => { queueMicrotask(async () => {
      try { await fetch('http://blocked.test/'); resolve('unexpectedly allowed'); }
      catch (e) { resolve(String((e && e.message) || e)); }
    }); }) });`, ['return await tools.micro();']));
  const message = value(result.outcomes[0]);
  assert.ok(message.includes('permission denied') && message.includes('blocked.test'), message);
});

test('manifest extraction preserves tool titles output schemas and annotations', async () => {
  const result = observation(await invoke(`defineTool({ name: 'meta', title: 'Meta Tool',
    outputSchema: { type: 'object', properties: { ok: { type: 'boolean' } }, required: ['ok'] },
    annotations: { readOnlyHint: true, openWorldHint: false }, handler: async () => ({ ok: true }) });`, []));
  assert.deepEqual(result.failures, []);
  assert.ok(result.compiled.length > 0);
  const [tool] = JSON.parse(result.compiled[0].manifests);
  assert.equal(tool.name, 'meta');
  assert.equal(tool.title, 'Meta Tool');
  assert.equal(tool.outputSchema.required[0], 'ok');
  assert.equal(tool.annotations.readOnlyHint, true);
  assert.equal(tool.annotations.openWorldHint, false);
});
