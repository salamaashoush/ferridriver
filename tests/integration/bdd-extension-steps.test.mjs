import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { observation, runtimeProbe } from './support.mjs';

async function registry(files, extensions, queries) {
  const { results } = await runtimeProbe([{ op: 'bdd-registry', globs: ['steps/**/*.js'], extensions, queries }], files);
  return observation(results[0]);
}

test('extensions contribute BDD steps alongside step files and obey host branches', async () => {
  const result = await registry({
    'steps/plain.js': "Given('a plain step', function () {});",
    'ext/tool_and_step.js': `defineTool({ name: 'bdd.tool', handler: async () => 'x' });
      if (ferridriver.host === 'bdd') { Given('an extension step', function () {}); }
      if (ferridriver.host === 'mcp') { Given('an mcp-only step', function () {}); }`,
  }, ['./ext'], ['a plain step', 'an extension step', 'an mcp-only step']);
  assert.deepEqual(result.matches, { 'a plain step': true, 'an extension step': true, 'an mcp-only step': false });
});

test('a file loaded as both an extension and a step glob registers exactly once', async () => {
  const result = await registry({ 'steps/shared.js': "Given('a shared step', function () {});" }, ['./steps/shared.js'], ['a shared step']);
  assert.equal(result.patterns.filter(pattern => pattern === 'a shared step').length, 1);
  assert.equal(result.matches['a shared step'], true);
});

test('a blocked extension contributes no BDD steps while ordinary step files still load', async () => {
  const result = await registry({
    'steps/plain.js': "Given('a plain step', function () {});",
    'pkg/package.json': JSON.stringify({ name: 'blocked-pkg', ferridriver: {
      entries: ['index.js'], requires: { commands: ['ferri-not-a-real-binary'] },
    } }),
    'pkg/index.js': "Given('a blocked step', function () {});",
  }, ['./pkg'], ['a plain step', 'a blocked step']);
  assert.deepEqual(result.matches, { 'a plain step': true, 'a blocked step': false });
});
