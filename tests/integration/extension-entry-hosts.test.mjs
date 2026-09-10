import assert from 'node:assert/strict';
import { basename } from 'node:path';
import { test } from '@ferridriver/test';
import { observation, runtimeProbe } from './support.mjs';

const absent = 'definitely-not-a-real-binary-xyz';
const hosts = ['mcp', 'bdd', 'test', 'script'];

async function gate(manifest, selected = hosts) {
  const { results } = await runtimeProbe(selected.map(host => ({ op: 'gate-extensions', entries: ['.'], host })), {
    'package.json': JSON.stringify({ name: '@acme/entries', type: 'module', ferridriver: manifest }),
    'mcp.ts': 'export const nothing = 1;',
    'fixtures.ts': 'export const nothing = 1;',
  });
  return results.map(observation);
}

test('entry host filters retain open fixtures on other hosts', async () => {
  const [mcp, testHost] = await gate({ entries: [{ path: './mcp.ts', hosts: ['mcp'] }, './fixtures.ts'] }, ['mcp', 'test']);
  assert.equal(mcp.files.length, 2);
  assert.deepEqual(testHost.files.map(file => basename(file)), ['fixtures.ts']);
});

test('unmet requirements on a narrowed entry block only its host', async () => {
  const [mcp, testHost] = await gate({ entries: [
    { path: './mcp.ts', hosts: ['mcp'], requires: { commands: [absent] } }, './fixtures.ts',
  ] }, ['mcp', 'test']);
  assert.equal(mcp.blocked.length, 1);
  assert.deepEqual(mcp.files, []);
  assert.deepEqual(testHost.blocked, []);
  assert.equal(testHost.files.length, 1);
});

test('package requirements block every extension host', async () => {
  for (const result of await gate({ entries: ['./mcp.ts', './fixtures.ts'], requires: { commands: [absent] } })) {
    assert.equal(result.blocked.length, 1);
    assert.deepEqual(result.files, []);
  }
});

test('runtime hosts match the manifest validator in order', async () => {
  const { results } = await runtimeProbe([{ op: 'runtime-contract' }]);
  const contract = observation(results[0]);
  assert.deepEqual(contract.hosts, contract.manifestHosts);
});
