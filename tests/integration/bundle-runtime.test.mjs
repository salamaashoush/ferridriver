import assert from 'node:assert/strict';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { observation, runtimeProbe } from './support.mjs';

function value(result) {
  const outcome = observation(result);
  assert.equal(outcome.status, 'ok', JSON.stringify(outcome));
  return outcome.value;
}

test('compiled TypeScript modules retain the entry and transitive imports in their source inputs', async () => {
  const { results, cwd } = await runtimeProbe([
    { op: 'bundle', entries: ['main.ts'] },
    { op: 'execute-module', entries: ['main.ts'] },
  ], {
    'main.ts': "import { triple } from './helper'; const v: number = triple(14); export default v;",
    'helper.ts': 'export const triple = (n: number): number => n * 3;',
  });
  const inputs = observation(results[0]).inputs;
  assert.ok(inputs.includes(join(cwd, 'main.ts')));
  assert.ok(inputs.includes(join(cwd, 'helper.ts')));
  assert.equal(value(results[1]), 42);
});

test('module sessions return null when the module has no default export', async () => {
  const { results } = await runtimeProbe([{ op: 'execute-module', entries: ['main.ts'] }], {
    'main.ts': 'export const x: number = 1; const _y = x + 1;',
  });
  assert.equal(value(results[0]), null);
});

test('bundled framework and cucumber modules expose the same step registrar', async () => {
  const { results } = await runtimeProbe([{ op: 'execute-module', entries: ['main.ts'] }], {
    'main.ts': `import { tool, bdd } from 'ferridriver'; import { Given } from '@cucumber/cucumber';
      export default { tool: typeof tool, bdd: typeof bdd.Given, same: Given === bdd.Given };`,
  });
  assert.deepEqual(value(results[0]), { tool: 'function', bdd: 'function', same: true });
});

test('operator aliases and virtual modules resolve while framework modules keep their identity', async () => {
  const config = {
    alias: { '@wdio/utils': 'wdio-shim.ts' },
    virtualModules: { 'acme:env': "export const env = 'staging'; export default env;" },
  };
  const { results, cwd } = await runtimeProbe([
    { op: 'set-bundler', config },
    { op: 'bundle', entries: ['main.ts'] },
    { op: 'execute-module', entries: ['main.ts'] },
    { op: 'set-bundler', config: { alias: { ferridriver: 'wdio-shim.ts' } } },
    { op: 'execute-module', entries: ['framework.ts'] },
  ], {
    'wdio-shim.ts': 'export const keys = (key: string): string => `key:${key}`;',
    'main.ts': "import { keys } from '@wdio/utils'; import { env } from 'acme:env'; export default `${keys('Enter')}|${env}`;",
    'framework.ts': "import { bdd } from 'ferridriver'; export default typeof bdd;",
  });
  assert.ok(observation(results[1]).inputs.includes(join(cwd, 'wdio-shim.ts')));
  assert.equal(value(results[2]), 'key:Enter|staging');
  assert.equal(value(results[4]), 'object');
  assert.notEqual(observation(results[0]).fingerprint, observation(results[3]).fingerprint);
});

test('native module aliases resolve both import paths and reject changes atomically', async () => {
  const aliases = [['@playwright/test', '@ferridriver/test'], ['playwright', 'ferridriver']];
  const { results } = await runtimeProbe([
    { op: 'set-aliases', aliases },
    { op: 'execute-module', entries: ['main.ts'] },
    { op: 'execute-script', source: `const pw = await import('@playwright/test'); const core = await import('playwright');
      return { expectIsFn: typeof pw.expect === 'function', host: core.host };` },
    { op: 'bundle', entries: ['missing.ts'] },
    { op: 'set-aliases', aliases: [['playwright', 'playwright-core']] },
    { op: 'set-aliases', aliases: [['ferridriver', '@ferridriver/test']] },
    { op: 'set-aliases', aliases },
    { op: 'set-aliases', aliases: [['@playwright/test', 'ferridriver']] },
    { op: 'aliases' },
  ], {
    'main.ts': `import { test, expect } from '@playwright/test'; import { bdd, chromium } from 'playwright';
      export default { expectIsFn: typeof expect === 'function', testDeclared: 'test' in { test },
        bddIsObject: typeof bdd === 'object', chromiumIsFn: typeof chromium === 'function' };`,
    'missing.ts': "import x from '@playwright/experimental-ct-react'; export default x;",
  });
  observation(results[0]);
  assert.deepEqual(value(results[1]), { expectIsFn: true, testDeclared: true, bddIsObject: true, chromiumIsFn: true });
  assert.deepEqual(value(results[2]), { expectIsFn: true, host: 'script' });
  assert.equal(typeof results[3].error, 'string');
  assert.match(results[4].error, /is not a native module/);
  assert.match(results[5].error, /already serves natively/);
  observation(results[6]);
  assert.match(results[7].error, /already serves natively/);
  assert.deepEqual(observation(results[8]).map(([name]) => name), aliases.map(([name]) => name));
});

test('the configured bytecode cache round trips metadata and invalidates changed transitive inputs', async () => {
  const bytecode = Array.from(new TextEncoder().encode('BYTECODE-V1'));
  const inputs = ['a.js', 'helper.js'];
  const { results, cache, cwd } = await runtimeProbe([
    { op: 'cache-info' },
    { op: 'cache-store', key: 7, bytecode, moduleName: 'm.js', aux: '[{"name":"x"}]', inputs },
    { op: 'cache-load', key: 7 },
    { op: 'inputs-fingerprint', inputs },
    { op: 'write-file', path: 'helper.js', content: 'export const h = 222;' },
    { op: 'cache-load', key: 7 },
  ], { 'a.js': "import './helper.js'; const v = 1;", 'helper.js': 'export const h = 1;' });
  const info = observation(results[0]);
  assert.equal(info.enabled, true);
  assert.ok(info.directory.startsWith(join(cache, 'ferridriver')));
  assert.ok(info.directory.includes('bytecode'));
  observation(results[1]);
  const hit = observation(results[2]);
  assert.deepEqual(hit.bytecode, bytecode);
  assert.equal(hit.moduleName, 'm.js');
  assert.equal(hit.aux, '[{"name":"x"}]');
  assert.equal(hit.inputFingerprint, observation(results[3]));
  assert.deepEqual(hit.inputs, inputs.map(path => join(cwd, path)));
  observation(results[4]);
  assert.equal(observation(results[5]), null);
});
