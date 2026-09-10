import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { observation, runtimeProbe } from './support.mjs';

function compile(entries, options = {}) {
  return { op: 'compile-extensions', groups: entries.map(entry => [entry]), ...options };
}

function evaluate(source) {
  return { op: 'execute-script', source };
}

function compiled(result) {
  const value = observation(result);
  assert.deepEqual(value.failures, []);
  return value.compiled;
}

function value(result) {
  const outcome = observation(result);
  assert.equal(outcome.status, 'ok', JSON.stringify(outcome));
  return outcome.value;
}

test('extensions compiled in separate batches both register in one session', async () => {
  const { results } = await runtimeProbe([
    compile(['alpha.js']), compile(['beta.js'], { append: true }),
    evaluate("return Object.keys(tools).sort().join(',');"),
  ], {
    'alpha.js': "defineTool({ name: 'alpha', handler: async () => ({ from: 'alpha' }) });",
    'beta.js': "defineTool({ name: 'beta', handler: async () => ({ from: 'beta' }) });",
  });
  assert.equal(compiled(results[0]).length, 1);
  assert.equal(compiled(results[1]).length, 1);
  const names = value(results[2]);
  assert.ok(names.includes('alpha'), names);
  assert.ok(names.includes('beta'), names);
});

test('extensions see the test surface during extraction and session execution', async () => {
  const { results } = await runtimeProbe([
    compile(['uses-test.ts']), evaluate('return await tools.shapes({});'),
  ], {
    'uses-test.ts': `import { test, expect } from '@ferridriver/test';
      const withUser = test.extend<{ user: string }>({ user: ['guest', { option: true }] });
      defineTool({ name: 'shapes', handler: async () => ({
        test: typeof test, extended: typeof withUser.extend, expect: typeof expect }) });`,
  });
  assert.ok(compiled(results[0])[0].manifests.includes('shapes'));
  assert.deepEqual(value(results[1]), { test: 'function', extended: 'function', expect: 'function' });
});

test('cached providers still evaluate before cold consumers during extraction', async () => {
  const consumer = `defineTool({ name: 'reads', handler: async () => ({ saw: globalThis.__provided ?? 'nothing' }) });
    if (globalThis.__provided !== 'from-provider') throw new Error('provider had not evaluated');`;
  const { results } = await runtimeProbe([
    compile(['provider.js', 'consumer.js']),
    { op: 'write-file', path: 'consumer.js', content: consumer + "\ndefineTool({ name: 'reads2', handler: async () => ({ ok: true }) });" },
    compile(['provider.js', 'consumer.js']),
  ], { 'provider.js': "globalThis.__provided = 'from-provider';", 'consumer.js': consumer });
  compiled(results[0]);
  observation(results[1]);
  assert.equal(compiled(results[2]).length, 2);
});

test('extraction refuses extension commands above the operator ceiling', async () => {
  const { results } = await runtimeProbe([compile(['shelly.js'], { policy: { commands: 'none' } })], {
    'shelly.js': `defineTool({ name: 'shelly', allow: { commands: { build: 'sh -c "echo hi"' } }, handler: async () => ({ ok: true }) });`,
  });
  const result = observation(results[0]);
  assert.deepEqual(result.compiled, []);
  assert.ok(result.failures.some(([, error]) => error.message.includes('allow.commands')), JSON.stringify(result.failures));
});

test('every extension imports one native module instance with stable export values', async () => {
  const { results } = await runtimeProbe([
    compile(['first.ts', 'second.ts']), evaluate('return await tools.identity({});'),
  ], {
    'first.ts': `import { test, mergeTests } from '@ferridriver/test';
      globalThis.__firstTest = test;
      globalThis.__firstMerge = mergeTests;
      globalThis.__rebindThrew = false;
      try { ferridriver.test = function fake() {}; } catch (e) { globalThis.__rebindThrew = true; }
      ferridriver.mergeTests = function fakeMerge() {};`,
    'second.ts': `import { test, mergeTests } from '@ferridriver/test';
      defineTool({ name: 'identity', handler: async () => ({
        sameInstance: globalThis.__firstTest === test,
        rebindThrew: globalThis.__rebindThrew,
        testStillTheBase: ferridriver.test === test,
        reassignReachedTheImport: ferridriver.mergeTests === mergeTests,
        importStillTheOriginal: globalThis.__firstMerge === mergeTests }) });`,
  });
  compiled(results[0]);
  assert.deepEqual(value(results[1]), {
    sameInstance: true, rebindThrew: true, testStillTheBase: true,
    reassignReachedTheImport: false, importStillTheOriginal: true,
  });
});

test('extraction and runtime agree on the ambient globals an extension can access', async () => {
  const { results } = await runtimeProbe([
    compile(['ambient.ts']), evaluate('return await tools.ambient({});'),
  ], {
    'ambient.ts': `const NAMES = ['process', 'console', 'fetch', 'Buffer', 'URL', 'TextEncoder',
      'setTimeout', 'queueMicrotask', 'structuredClone', 'performance', 'crypto',
      'AbortController', 'ReadableStream', 'require', 'expect'];
      const present = () => NAMES.filter((n) => typeof (globalThis as any)[n] !== 'undefined').join(',');
      defineTool({ name: 'ambient', description: present(), handler: async () => present() });`,
  });
  const manifests = JSON.parse(compiled(results[0])[0].manifests);
  assert.ok(Array.isArray(manifests));
  assert.equal(typeof manifests[0].description, 'string');
  assert.equal(value(results[1]), manifests[0].description);
});
