import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { passed, run, workspace } from './support.mjs';

const spec = "import { test, expect } from '@ferridriver/test'; test('runs', async ({}) => { expect(1).toBe(1); });";
const settings = { reporter: [{ name: 'list' }], testMatch: ['specs/alpha.spec.ts'],
  browser: { browser: 'chromium', backend: 'cdp-pipe', headless: true } };
const toml = '[test]\nreporter = [{ name = "list" }]\ntestMatch = ["specs/alpha.spec.ts"]\n[test.browser]\nbrowser = "chromium"\nbackend = "cdp-pipe"\nheadless = true\n';

async function project(files) {
  return workspace({ 'specs/alpha.spec.ts': spec, 'specs/beta.spec.ts': spec, ...files });
}
const list = cwd => run(['test', '--list', '--no-inherit'], { cwd });
const corpus = text => text.split('\n').filter(line => line.includes('.spec.ts')).map(line => line.trim()).sort();

test('equivalent module and TOML configurations discover the same selected tests', async () => {
  const [module, document] = await Promise.all([
    project({ 'ferridriver.config.ts': `export default ${JSON.stringify({ test: settings })};` }).then(list),
    project({ 'ferridriver.toml': toml }).then(list),
  ]);
  passed(module);
  passed(document);
  assert.deepEqual(corpus(module.text), corpus(document.text));
  assert.ok(module.text.includes('alpha.spec.ts'));
  assert.ok(!module.text.includes('beta.spec.ts'));
});

test('configuration reports attribute module values to their source file', async () => {
  const cwd = await project({ 'ferridriver.config.ts': `import { defineConfig } from '@ferridriver/test';
    export default { test: defineConfig({ projects: [{ name: 'one' }, { name: 'two' }] }) };` });
  const result = await run(['config', '--no-inherit'], { cwd });
  passed(result);
  assert.ok(result.text.includes('ferridriver.config.ts'));
  const row = result.text.split('\n').find(line => line.includes('test.projects'));
  assert.ok(row?.includes('[{"name":"one"},{"name":"two"}]'), result.text);
  assert.ok(row.includes('ferridriver.config.ts'));
});

test('a module layers over its sibling document without shadowing it', async () => {
  const result = await list(await project({ 'ferridriver.toml': toml,
    'ferridriver.config.ts': "export default { test: { testMatch: ['specs/beta.spec.ts'] } };" }));
  passed(result);
  assert.ok(result.text.includes('beta.spec.ts'));
  assert.ok(!result.text.includes('alpha.spec.ts'));
  assert.ok(!result.text.includes('also present and ignored'));
});

test('competing documents follow basename precedence', async () => {
  const result = await list(await project({ 'ferridriver.toml': toml,
    'ferridriver.yaml': 'test:\n  testMatch:\n    - specs/beta.spec.ts\n' }));
  passed(result);
  assert.ok(result.text.includes('alpha.spec.ts'));
  assert.ok(!result.text.includes('beta.spec.ts'));
});

for (const [title, source, message] of [
  ['modules cannot configure their own loader', "export default { test: { moduleAliases: { '@acme/test': '@ferridriver/test' } } };", 'test.moduleAliases'],
  ['missing default exports fail with an explanation', 'export const config = { test: {} };', 'no default export'],
  ['configuration exceptions retain their original message', "throw new Error('the config could not decide'); export default {};", 'the config could not decide'],
]) {
  test(title, async () => {
    const result = await list(await project({ 'ferridriver.config.ts': source }));
    assert.notEqual(result.code, 0, result.text);
    assert.ok(result.text.includes(message), result.text);
  });
}
