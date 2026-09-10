import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { observation, passed, run, runtimeProbe, workspace } from './support.mjs';

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

const hosts = ['mcp', 'bdd', 'test', 'script'];
const snapshot = 'return { tools: Object.keys(tools).sort(), host: ferridriver.host };';

function value(result) {
  assert.equal(result.status, 'ok', JSON.stringify(result));
  return result.value;
}

test('one extension exposes the same tool registrations under all four hosts', async () => {
  const { results } = await runtimeProbe(hosts.map(host => ({
    op: 'extension-session', entries: ['./plug.ts'], host, sources: [snapshot],
  })), {
    'plug.ts': "defineTool({ name: 'alpha', handler: async () => 'a' });\ndefineTool({ name: 'beta.nested', handler: async () => 'b' });\nGiven('a step from an extension', function () {});\n",
  });
  const seen = results.map((item, index) => {
    const result = observation(item);
    assert.deepEqual(result.failures, []);
    assert.deepEqual(result.blocked, []);
    assert.equal(result.bindings.length, 1);
    const actual = value(result.outcomes[0]);
    assert.deepEqual(actual.tools, ['alpha', 'beta', 'beta.nested']);
    assert.equal(actual.host, hosts[index]);
    return actual.tools;
  });
  for (const tools of seen.slice(1)) assert.deepEqual(tools, seen[0]);
});

test('an unmet package requirement prevents registrations under all four hosts', async () => {
  const { results } = await runtimeProbe(hosts.map(host => ({
    op: 'extension-session', entries: ['./pkg'], host, sources: [snapshot],
  })), {
    'pkg/package.json': JSON.stringify({ name: 'needs-binary', ferridriver: {
      entries: ['index.ts'], requires: { commands: ['ferri-not-a-real-binary'] },
    } }),
    'pkg/index.ts': "defineTool({ name: 'gated', handler: async () => 'g' });\n",
  });
  for (const item of results) {
    const result = observation(item);
    assert.equal(result.blocked.length, 1);
    assert.deepEqual(result.bindings, []);
    assert.deepEqual(value(result.outcomes[0]).tools, []);
  }
});

test('extraction records host-specific tools steps hooks and fixtures separately', async () => {
  const { results } = await runtimeProbe([{
    op: 'extension-session', entries: ['./branching.ts'], host: 'script', sources: [],
  }], {
    'branching.ts': `import { test } from '@ferridriver/test';
      if (ferridriver.host === 'mcp') defineTool({ name: 'only.mcp', handler: async () => 1 });
      if (ferridriver.host === 'bdd') { Given('a step only bdd sees', function () {}); Before(function () {}); }
      if (ferridriver.host === 'test') test.extend({ onlyTest: ['x', { option: true }] });
      if (ferridriver.host === 'script') defineTool({ name: 'only.script', handler: async () => 2 });`,
  });
  const result = observation(results[0]);
  assert.deepEqual(result.failures, []);
  assert.equal(result.compiled.length, 1);
  const { mcp, bdd, test: testHost, script } = result.compiled[0].snapshot.hosts;
  assert.equal(mcp.tools.length, 1);
  assert.deepEqual(mcp.steps, []);
  assert.deepEqual(bdd.steps, ['Given a step only bdd sees']);
  assert.deepEqual(bdd.hooks, ['Before']);
  assert.deepEqual(bdd.tools, []);
  assert.deepEqual(testHost.fixtures, ['onlyTest']);
  assert.equal(script.tools.length, 1);
  assert.ok(result.compiled[0].manifests.includes('only.mcp'));
  assert.ok(!result.compiled[0].manifests.includes('only.script'));
});
