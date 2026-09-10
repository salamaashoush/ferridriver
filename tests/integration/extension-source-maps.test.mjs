import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { observation, runtimeProbe } from './support.mjs';

function stack(result) {
  assert.equal(result.status, 'error', JSON.stringify(result));
  assert.equal(typeof result.error.stack, 'string');
  return result.error.stack;
}

function load(entries, sources) {
  return { op: 'extension-session', entries, sources, host: 'script' };
}

test('a throwing extension reports its author source instead of the bundle module', async () => {
  const { results } = await runtimeProbe([
    load(['./boom.ts'], ['return await tools.boom({});']),
  ], { 'boom.ts': "const label: string = 'boom';\ndefineTool({ name: 'boom', handler: async () => { throw new Error(label); } });\n" });
  const result = observation(results[0]);
  assert.deepEqual(result.failures, []);
  assert.equal(result.bindings.length, 1);
  assert.equal(result.bindings[0].hasSourceMap, true);
  const trace = stack(result.outcomes[0]);
  assert.ok(trace.includes('boom.ts'), trace);
  assert.ok(!trace.includes('ferri_extension_'), trace);
});

test('two extensions remap errors through their own source maps in one session', async () => {
  const { results } = await runtimeProbe([
    load(['./first.ts', './second.ts'], ['return await tools.first({});', 'return await tools.second({});']),
  ], {
    'first.ts': "const pad = 1;\nconst pad2 = 2;\nconst pad3 = 3;\nconst pad4 = 4;\nconst pad5 = 5;\ndefineTool({ name: 'first', handler: async () => { throw new Error('first'); } });\n",
    'second.ts': "defineTool({ name: 'second', handler: async () => { throw new Error('second'); } });\n",
  });
  const result = observation(results[0]);
  assert.deepEqual(result.failures, []);
  assert.equal(result.bindings.length, 2);
  const first = stack(result.outcomes[0]);
  const second = stack(result.outcomes[1]);
  assert.ok(first.includes('first.ts') && !first.includes('second.ts'), first);
  assert.ok(second.includes('second.ts') && !second.includes('first.ts'), second);
});

test('a cached extension preserves the exact error stack from its cold load', async () => {
  const operation = load(['./cached.ts'], ['return await tools.cached({});']);
  const { results } = await runtimeProbe([operation, operation], {
    'cached.ts': "defineTool({ name: 'cached', handler: async () => { throw new Error('cached'); } });\n",
  });
  const cold = stack(observation(results[0]).outcomes[0]);
  const warm = stack(observation(results[1]).outcomes[0]);
  assert.ok(cold.includes('cached.ts'), cold);
  assert.equal(warm, cold);
});
