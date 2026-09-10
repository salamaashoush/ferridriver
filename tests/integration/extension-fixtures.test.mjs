import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { observation, passed, run, runtimeProbe, workspace } from './support.mjs';

const first = `defineFixtures({
  label: async ({}, use) => { await use('first'); },
  onlyFirst: async ({}, use) => { await use('kept'); },
});`;
const second = `defineFixtures({ label: async ({ label }, use) => { await use(label + '+second'); } });`;
const composed = `import { test, expect } from '@ferridriver/test';
test('receives the contributed fixtures', async ({ label, onlyFirst }) => {
  expect(label).toBe('first+second');
  expect(onlyFirst).toBe('kept');
});`;

async function suite(extensions, spec, policy = {}) {
  const files = Object.fromEntries(extensions.map((source, i) => [`ext${i}.ts`, source]));
  const cwd = await workspace({ ...files, 'contributed.test.ts': spec,
    'ferridriver.toml': `[extensions]
paths = ${JSON.stringify(Object.keys(files).map(name => `./${name}`))}
[extensions.policy]
${Object.entries(policy).map(([key, value]) => `${key} = ${JSON.stringify(value)}`).join('\n')}
[test]
testMatch = ['contributed.test.ts']
workers = 4
retries = 0
`,
  });
  return run(['test', '--no-inherit', '--headless'], { cwd });
}

function onePassed(result) {
  passed(result);
  assert.match(result.text, /1 passed/);
}

function error(result) {
  assert.equal(result.status, 'error', JSON.stringify(result));
  return result.error.message;
}

async function probe(entries, sources, files = {}, options = {}) {
  const { results } = await runtimeProbe([{
    op: 'extension-session', entries, sources, host: 'test', ...options,
  }], files);
  return results[0];
}

test('extension fixtures compose in package order and resolve the overridden fixture', async () => {
  onePassed(await suite([first, second], composed));
});

test('the same contributed-fixture spec fails without its extension packages', async () => {
  const result = await suite([], composed);
  assert.notEqual(result.code, 0, result.text);
  assert.match(result.text, /label/);
});

test('reversing extension package order changes which fixture wins', async () => {
  onePassed(await suite([second, first], `import { test, expect } from '@ferridriver/test';
    test('later package wins', async ({ label }) => { expect(label).toBe('first'); });`));
});

test('an automatic extension fixture runs exactly once without being requested', async () => {
  onePassed(await suite([`defineFixtures({ marker: [async ({}, use) => {
    globalThis.__autoRan = (globalThis.__autoRan ?? 0) + 1; await use(1);
  }, { auto: true }] });`], `import { test, expect } from '@ferridriver/test';
    test('auto ran', async () => { expect(globalThis.__autoRan).toBe(1); });`));
});

test('fixture registration after extension installation fails with the supported alternative', async () => {
  const result = observation(await probe([], ['defineFixtures({ late: async ({}, use) => { await use(1); } });']));
  const message = error(result.outcomes[0]);
  assert.ok(message.includes('defineFixtures() can only be called while an extension is loading'), message);
  assert.ok(message.includes('test.extend()'), message);
});

test('the session fixture ceiling refuses a contribution compiled under an open policy', async () => {
  const result = await probe(['./ext.ts'], [], { 'ext.ts': first }, { sessionPolicy: { fixtures: false } });
  assert.equal(typeof result.error, 'string', JSON.stringify(result));
  assert.ok(result.error.includes('extension.policy.refused'), result.error);
  assert.ok(result.error.includes('[extensions.policy] fixtures = false'), result.error);
});

test('a closed fixture ceiling preserves explicit extend merge and custom matcher APIs', async () => {
  onePassed(await suite([`
    const a = ferridriver.test.extend({ a: async ({}, use) => { await use('a'); } });
    const b = ferridriver.test.extend({ b: async ({}, use) => { await use('b'); } });
    const merged = ferridriver.mergeTests(a, b);
    expect.extend({ toBeFortyTwo(received) { return { pass: received === 42, message: () => 'nope' }; } });
    expect(42).toBeFortyTwo();
    globalThis.__ownChain = typeof merged === 'function';
  `], `import { test, expect } from '@ferridriver/test';
    test('own chains survive the ceiling', async () => { expect(globalThis.__ownChain).toBe(true); });`,
  { fixtures: false }));
});

test('the base test object refuses reassignment', async () => {
  const result = observation(await probe([], ["'use strict'; ferridriver.test = function fake() {}; return 'assigned';"]));
  assert.match(error(result.outcomes[0]), /read-only|read only/);
});

test('extraction reports contributed fixtures for every host', async () => {
  const result = observation(await probe(['./ext.ts'], [], { 'ext.ts': first }));
  assert.deepEqual(result.failures, []);
  assert.equal(result.compiled.length, 1);
  for (const host of ['mcp', 'bdd', 'test', 'script']) {
    const registrations = result.compiled[0].snapshot.hosts[host];
    assert.ok(registrations.fixtures.includes('onlyFirst'), JSON.stringify(registrations));
  }
});

test('extraction enforces the same fixture ceiling as session installation', async () => {
  const result = observation(await probe(['./ext.ts'], [], { 'ext.ts': first }, { loadPolicy: { fixtures: false } }));
  assert.deepEqual(result.compiled, []);
  assert.ok(result.failures.some(([, failure]) => failure.message.includes('[extensions.policy] fixtures = false')),
    JSON.stringify(result.failures));
});

test('module imports expose defineFixtures bindSteps and the same base test object', async () => {
  onePassed(await suite([`
    import { defineFixtures, bindSteps, test, mergeTests } from 'ferridriver';
    import { test as fromTestModule } from '@ferridriver/test';
    if (test !== fromTestModule) throw new Error('the two surfaces must hand back one test object');
    if (typeof mergeTests !== 'function') throw new Error('mergeTests');
    defineFixtures({ viaImport: async ({}, use) => { await use('imported'); } });
    const { Given } = bindSteps(test);
    Given('the import worked', async ({ viaImport }) => { globalThis.__seen = viaImport; });
  `], `import { test, expect } from '@ferridriver/test';
    test('the imported entry point contributed', async ({ viaImport }) => { expect(viaImport).toBe('imported'); });`));
});

test('extension fixtures are allowed by the default policy', async () => {
  assert.equal(observation(await probe([], [])).loadPolicy.fixtures, true);
});
