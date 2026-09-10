import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { passed, run, workspace } from './support.mjs';

const backends = [['cdp-pipe', 'chromium'], ['cdp-raw', 'chromium'], ['bidi', 'firefox'], ['webkit', 'webkit']];
const projects = backends.map(([backend, browser]) => `
[[test.projects]]
name = "${backend}"
[test.projects.browser]
browser = "${browser}"
backend = "${backend}"
headless = true
`).join('');
const spec = `import { test, expect } from '@ferridriver/test';
test('extension contribution', async () => {
  expect(globalThis.__fromExtension).toEqual({ value: 'from-extension', host: 'test' });
});`;
const extension = `globalThis.__fromExtension = { value: 'from-extension', host: ferridriver.host };`;

async function suite(files, extensions, extra = '') {
  const cwd = await workspace({ ...files, 'ferridriver.toml': `extensions = ${JSON.stringify(extensions)}
[test]
testMatch = ["*.spec.ts"]
workers = 4
retries = 0
${extra}
` });
  return run(['test', '--no-inherit', '--headless'], { cwd });
}

for (const enabled of [true, false]) {
  test(`native test host ${enabled ? 'loads' : 'requires'} the extension's global contribution`, async () => {
    const result = await suite({ 'host.spec.ts': spec, 'extension.ts': extension }, enabled ? ['./extension.ts'] : []);
    if (enabled) passed(result);
    else assert.notEqual(result.code, 0, result.text);
    assert.ok(result.text.includes(enabled ? '1 passed' : '1 failed'), result.text);
  });
}

test('native projects share the module instance supplied by an extension', async () => {
  const result = await suite({
    'provider.spec.ts': `import { test, expect } from '@ferridriver/test';
import { greet, calls } from 'fake-vendor';
test('provided module', async () => {
  expect(greet('world')).toBe('hello world');
  expect(calls).toEqual(['from-entry', 'hello world']);
});`,
    'pkg/package.json': JSON.stringify({ name: 'vendor', ferridriver: {
      apiVersion: 2, name: 'vendor', entries: ['entry.ts'], provides: { modules: { 'fake-vendor': 'vendor.ts' } },
    } }),
    'pkg/vendor.ts': `export const calls: string[] = [];
export function greet(who: string) { const result = 'hello ' + who; calls.push(result); return result; }`,
    'pkg/entry.ts': `import { calls } from 'fake-vendor'; calls.push('from-entry');`,
  }, ['./pkg'], projects);
  passed(result);
  assert.match(result.text, /4 passed/);
});

test('an unmet package requirement is reported without dropping valid test extensions', async () => {
  const result = await suite({
    'host.spec.ts': spec, 'extension.ts': extension,
    'pkg/package.json': JSON.stringify({ name: 'blocked-pkg', ferridriver: {
      entries: ['index.ts'], requires: { commands: ['ferri-not-a-real-binary'] },
    } }),
    'pkg/index.ts': 'globalThis.__fromBlockedPackage = true;',
  }, ['./extension.ts', './pkg']);
  passed(result);
  assert.match(result.text, /ferri-not-a-real-binary/);
});

for (const [backend, browser] of backends) {
  for (const enabled of [true, false]) {
    test(`${backend}: contributed fixtures ${enabled ? 'reach the page test' : 'are reported missing without their extension'}`, async () => {
      const result = await suite({
        'fixtures.spec.ts': `import { test, expect } from '@ferridriver/test';
test('contributed fixture', async ({ page, deployment }) => {
  expect(deployment).toBe('staging');
  await page.goto('about:blank');
  expect(await page.evaluate(() => 1 + 1)).toBe(2);
});`,
        'fixtures.ts': `defineFixtures({ deployment: async ({}, use) => { await use('staging'); } });`,
      }, enabled ? ['./fixtures.ts'] : [], `[test.browser]\nbrowser = "${browser}"\nbackend = "${backend}"\nheadless = true`);
      if (enabled) passed(result);
      else {
        assert.notEqual(result.code, 0, result.text);
        assert.ok(result.text.includes('Test has unknown parameter "deployment".'), result.text);
      }
      assert.ok(result.text.includes(enabled ? '1 passed' : '1 failed'), result.text);
    });
  }
}
