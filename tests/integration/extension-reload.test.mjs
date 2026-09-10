import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { observation, runtimeProbe } from './support.mjs';

function toolNames(result) {
  const { count, failures, manifests } = observation(result);
  assert.deepEqual(failures, []);
  assert.equal(count, 1);
  assert.equal(manifests.length, 1);
  assert.ok(Array.isArray(manifests[0]));
  return manifests[0].map(manifest => manifest.name);
}

for (const [label, entry, helper, files] of [
  ['standalone', 'tool.ts', 'lib/shared.ts', {}],
  ['package', 'pkg/src/login.ts', 'pkg/src/lib/shared.ts', { 'pkg/package.json': '{"name":"acme","type":"module"}' }],
]) {
  test(`${label} extension reload observes an edited inlined helper`, async () => {
    const { results } = await runtimeProbe([
      { op: 'load-extensions', includeManifests: true, entries: [entry] },
      { op: 'write-file', path: helper, content: "export const NAME = 'probe.second';\n" },
      { op: 'load-extensions', includeManifests: true, entries: [entry] },
    ], {
      ...files,
      [helper]: "export const NAME = 'probe.first';\n",
      [entry]: `import { NAME } from './lib/shared';
        defineTool({ name: NAME, exposeAsTool: true, handler: async () => ({}) });`,
    });
    assert.deepEqual(toolNames(results[0]), ['probe.first']);
    observation(results[1]);
    assert.deepEqual(toolNames(results[2]), ['probe.second']);
  });
}

test('an unchanged extension keeps the same manifest when compiled again', async () => {
  const { results } = await runtimeProbe([
    { op: 'load-extensions', includeManifests: true, entries: ['tool.ts'] },
    { op: 'load-extensions', includeManifests: true, entries: ['tool.ts'] },
  ], { 'tool.ts': "defineTool({ name: 'stable.tool', handler: async () => ({}) });" });
  assert.deepEqual(toolNames(results[0]), ['stable.tool']);
  assert.deepEqual(toolNames(results[1]), ['stable.tool']);
});
