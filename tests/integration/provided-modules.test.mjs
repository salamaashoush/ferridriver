import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { observation, runtimeProbe } from './support.mjs';

const files = {
  'pkg/package.json': JSON.stringify({ name: 'vendor-pkg', ferridriver: {
    apiVersion: 2, name: 'vendor-pkg', entries: ['one.ts', 'two.ts', 'a.ts', 'b.ts'],
    provides: { modules: { 'fake-vendor': 'vendor.ts' }, aliases: { 'fake-vendor/alias': 'fake-vendor' } },
  } }),
  'pkg/vendor.ts': 'export const marker = { seen: [] as string[] };\nexport function note(who: string) { marker.seen.push(who); }\n',
  'pkg/one.ts': "import { marker, note } from 'fake-vendor';\nnote('one');\n(globalThis as Record<string, unknown>).__one = marker;\n",
  'pkg/two.ts': "import { marker } from 'fake-vendor/alias';\n(globalThis as Record<string, unknown>).__two = marker;\n",
  'pkg/state.ts': 'export const state = { entries: [] as string[] };\n',
  'pkg/a.ts': "import { state } from './state.ts';\nstate.entries.push('a');\n(globalThis as Record<string, unknown>).__stateA = state;\n",
  'pkg/b.ts': "import { state } from './state.ts';\nstate.entries.push('b');\n(globalThis as Record<string, unknown>).__stateB = state;\n",
};

function operation(sources = [], options = {}) {
  return { op: 'extension-session', entries: ['./pkg'], host: 'script', sources, ...options };
}

function loaded(result) {
  const value = observation(result);
  assert.deepEqual(value.failures, []);
  return value;
}

function value(outcome) {
  assert.equal(outcome.status, 'ok', JSON.stringify(outcome));
  return outcome.value;
}

test('claimed specifiers and aliases share one module and expose entry mutations', async () => {
  const { results } = await runtimeProbe([operation([
    'return { same: globalThis.__one === globalThis.__two, seen: globalThis.__one.seen };',
  ])], files);
  const result = loaded(results[0]);
  assert.ok(result.issues.every(issue => !issue.message.includes('claim')), JSON.stringify(result.issues));
  assert.deepEqual(value(result.outcomes[0]), { same: true, seen: ['one'] });
});

test('a plain script imports the provider module already mutated by an extension', async () => {
  const { results } = await runtimeProbe([operation([], { modules: ['consumer.ts'] })], {
    ...files,
    'consumer.ts': "import { marker, note } from 'fake-vendor';\nnote('script');\nexport default marker.seen.join(',');\n",
  });
  assert.equal(value(loaded(results[0]).outcomes[0]), 'one,script');
});

test('require and import expose the same live object through a provider alias', async () => {
  const { results } = await runtimeProbe([operation([`
    const v = require('fake-vendor');
    const a = require('fake-vendor/alias');
    v.note('require');
    return { same: v.marker === a.marker, live: v.marker === globalThis.__one, seen: v.marker.seen };
  `])], files);
  const facts = value(loaded(results[0]).outcomes[0]);
  assert.equal(facts.same, true);
  assert.equal(facts.live, true);
  assert.ok(Array.isArray(facts.seen) && facts.seen.includes('require'), JSON.stringify(facts));
});

test('a provider claim arriving after resolution is sealed is reported and remains unresolvable', async () => {
  const { results } = await runtimeProbe([
    operation(), operation([], { entries: ['./late'], resolve: ['late-vendor'] }),
  ], {
    ...files,
    'late/package.json': JSON.stringify({ name: 'late-pkg', ferridriver: {
      entries: ['e.ts'], provides: { modules: { 'late-vendor': 'v.ts' } },
    } }),
    'late/v.ts': 'export const late = 1;\n',
    'late/e.ts': 'export {};\n',
  });
  loaded(results[0]);
  const result = observation(results[1]);
  assert.ok(result.issues.some(issue => issue.message.includes('sealed') && issue.message.includes('late-vendor')),
    JSON.stringify(result.issues));
  assert.equal(result.resolved['late-vendor'], null);
});

test('entries from one package share their helper module and preserve entry order', async () => {
  const { results } = await runtimeProbe([operation([
    'return { same: globalThis.__stateA === globalThis.__stateB, entries: globalThis.__stateA.entries };',
  ])], files);
  assert.deepEqual(value(loaded(results[0]).outcomes[0]), { same: true, entries: ['a', 'b'] });
});
