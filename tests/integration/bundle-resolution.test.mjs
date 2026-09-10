import assert from 'node:assert/strict';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { observation, runtimeProbe } from './support.mjs';

const files = {
  'node_modules/dual-build/package.json': JSON.stringify({ name: 'dual-build', type: 'module', exports: {
    '.': { browser: './browser.js', node: './node.js', default: './default.js' },
  } }),
  'node_modules/dual-build/browser.js': "export const build = 'browser';",
  'node_modules/dual-build/node.js': "export const build = 'node';",
  'node_modules/dual-build/default.js': "export const build = 'default';",
  'node_modules/main-only/package.json': JSON.stringify({ name: 'main-only', type: 'module', main: './lib/index.js' }),
  'node_modules/main-only/lib/index.js': "export const flavor = 'main';",
  'node_modules/legacy-browser/package.json': JSON.stringify({ name: 'legacy-browser', type: 'module', main: './node.js',
    browser: { './node.js': './browser.js' } }),
  'node_modules/legacy-browser/node.js': "export const legacy = 'node';",
  'node_modules/legacy-browser/browser.js': "export const legacy = 'browser';",
  'tsconfig.test.json': JSON.stringify({ compilerOptions: { baseUrl: '.', paths: { '@app/*': ['src/*'] } } }),
  'src/mapped.ts': "export const via = 'paths';",
  'conditions.ts': "import { build } from 'dual-build'; export default build;",
  'main-field.ts': "import { flavor } from 'main-only'; export default flavor;",
  'legacy.ts': "import { legacy } from 'legacy-browser'; export default legacy;",
  'paths.ts': "import { via } from '@app/mapped'; export default via;",
};

function value(result) {
  const outcome = observation(result);
  assert.equal(outcome.status, 'ok', JSON.stringify(outcome));
  return outcome.value;
}

test('configured export conditions choose their branch and an unconfigured resolver uses default', async () => {
  const operations = [null, 'browser', 'node'].flatMap(condition => [
    { op: 'set-bundler', config: { conditions: condition ? [condition] : [] } },
    { op: 'execute-module', entries: ['conditions.ts'] },
  ]);
  const { results } = await runtimeProbe(operations, files);
  for (const index of [0, 2, 4]) observation(results[index]);
  assert.deepEqual([results[1], results[3], results[5]].map(value), ['default', 'browser', 'node']);
});

test('default main fields resolve a main-only package and an empty list rejects it', async () => {
  const { results } = await runtimeProbe([
    { op: 'set-bundler' },
    { op: 'execute-module', entries: ['main-field.ts'] },
    { op: 'set-bundler', config: { mainFields: [] } },
    { op: 'bundle', entries: ['main-field.ts'] },
  ], files);
  observation(results[0]);
  assert.equal(value(results[1]), 'main');
  observation(results[2]);
  assert.equal(typeof results[3].error, 'string');
});

test('legacy browser mappings only apply when their alias field is selected', async () => {
  const { results } = await runtimeProbe([
    { op: 'set-bundler' },
    { op: 'execute-module', entries: ['legacy.ts'] },
    { op: 'set-bundler', config: { aliasFields: [['browser']] } },
    { op: 'execute-module', entries: ['legacy.ts'] },
  ], files);
  observation(results[0]);
  observation(results[2]);
  assert.equal(value(results[1]), 'node');
  assert.equal(value(results[3]), 'browser');
});

test('explicit tsconfig selection resolves paths and records its dependency while missing configs fail', async () => {
  const { results, cwd } = await runtimeProbe([
    { op: 'set-bundler' },
    { op: 'bundle', entries: ['paths.ts'] },
    { op: 'set-bundler', tsconfig: 'tsconfig.test.json' },
    { op: 'execute-module', entries: ['paths.ts'] },
    { op: 'bundle-source', entries: ['paths.ts'] },
    { op: 'set-bundler', tsconfig: 'tsconfig.missing.json' },
    { op: 'bundle', entries: ['paths.ts'] },
  ], files);
  observation(results[0]);
  assert.equal(typeof results[1].error, 'string');
  observation(results[2]);
  assert.equal(value(results[3]), 'paths');
  assert.ok(observation(results[4]).configInputs.includes(join(cwd, 'tsconfig.test.json')));
  observation(results[5]);
  assert.equal(typeof results[6].error, 'string');
});

test('every module resolution control produces a distinct cache fingerprint', async () => {
  const { results } = await runtimeProbe([
    { op: 'set-bundler' },
    { op: 'set-bundler', config: { conditions: ['browser'] } },
    { op: 'set-bundler', config: { conditions: ['node'] } },
    { op: 'set-bundler', config: { mainFields: [] } },
    { op: 'set-bundler', config: { aliasFields: [['browser']] } },
    { op: 'set-bundler', tsconfig: 'tsconfig.test.json' },
  ], files);
  const fingerprints = results.map(result => observation(result).fingerprint);
  assert.equal(fingerprints.length, 6);
  assert.equal(new Set(fingerprints).size, 6);
});
