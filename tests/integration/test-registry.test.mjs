import assert from 'node:assert/strict';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { observation, passed, run, runtimeProbe, workspace } from './support.mjs';

async function collect(source, name = 'shapes.test.ts', host = 'test') {
  const { results } = await runtimeProbe([{ op: 'test-registry', entries: [name], host }], { [name]: source });
  return observation(results[0]);
}

const shapes = `import { test, describe, expect } from '@ferridriver/test';

if (typeof test !== 'function') throw new Error('test is not a function');
if (typeof describe !== 'function') throw new Error('describe is not a function');
if (typeof expect !== 'function') throw new Error('expect is not a function');

test('plain test', async ({ page, context }) => {
  await page.goto('about:blank');
});

test('with details', { tag: ['smoke', 'fast'], annotation: { type: 'issue', description: 'JIRA-1' }, timeout: 1234, retries: 2 }, async ({ request }) => {});

test.skip('registration skip', async ({ page }) => {});
test.fixme('registration fixme', () => {});
test.fail('registration fail', async ({ page }) => {});
test.slow('registration slow', async ({ page }) => {});

describe('outer', () => {
  test.use({ locale: 'de-DE' });
  test('inner test', async ({ page, testInfo }) => {});
  describe.serial('nested serial', () => {
    test('serial child', async () => {});
  });
});

describe.skip('skipped suite', () => {
  test('inside skipped', () => {});
});

test.use({ colorScheme: 'dark' });

test.beforeEach(async ({ page }) => {});
test.afterAll(() => {});

test.each([
  { name: 'Salama', n: 1 },
  { name: 'Sashoush', n: 2 },
])('greets $name', async ({ page }, row) => {});

const extended = test.extend({
  greeting: async ({ browserName }, use) => {
    await use('hi');
  },
  port: [4321, { option: true }],
});

extended('uses custom fixture', async ({ page, greeting, port }) => {});
`;

const focused = `import { test, describe } from '@ferridriver/test';

test.only('focused', async ({ page }) => {});

describe('configured', () => {
  describe.configure({ mode: 'serial', retries: 3, timeout: 9000 });
  test('in configured', () => {});
});

describe.each([{ backend: 'cdp' }, { backend: 'webkit' }])('on $backend', (row) => {
  test('per-backend test', () => {});
});
`;

test('test registration retains fixtures, suites, annotations and source locations', async () => {
  const c = await collect(shapes);
  assert.deepEqual(c.tests.map(t => t.title), ['plain test', 'with details', 'registration skip',
    'registration fixme', 'registration fail', 'registration slow', 'inner test', 'serial child',
    'inside skipped', 'greets Salama', 'greets Sashoush', 'uses custom fixture']);
  assert.equal(c.hasOnly, false);
  assert.deepEqual(c.tests[0].requested, ['page', 'context']);
  assert.deepEqual(c.tests[3].requested, []);
  const details = c.tests[1];
  assert.deepEqual(details.annotations.map(a => [a.kind, a.value]), [['tag', 'smoke'], ['tag', 'fast'], ['info', 'issue']]);
  assert.equal(details.annotations[2].description, 'JIRA-1');
  assert.equal(details.timeoutMs, 1234);
  assert.equal(details.retries, 2);
  for (const [index, kind] of [[2, 'skip'], [3, 'fixme'], [4, 'fail'], [5, 'slow']]) {
    assert.equal(c.tests[index].annotations.length, 1);
    assert.equal(c.tests[index].annotations[0].kind, kind);
  }
  assert.equal(c.suites.length, 3);
  assert.equal(c.suites[0].name, 'outer');
  assert.equal(c.suites[0].parent, null);
  assert.deepEqual(c.suites[0].useOptions, { locale: 'de-DE' });
  assert.equal(c.suites[1].name, 'nested serial');
  assert.equal(c.suites[1].parent, 0);
  assert.equal(c.suites[1].mode, 'serial');
  assert.equal(c.suites[2].name, 'skipped suite');
  assert.equal(c.suites[2].annotations[0].kind, 'skip');
  assert.deepEqual([c.tests[6].suite, c.tests[7].suite, c.tests[8].suite, c.tests[0].suite], [0, 1, 2, null]);
  assert.equal(c.fileUse.length, 1);
  assert.deepEqual(c.fileUse[0].options, { colorScheme: 'dark' });
  assert.ok(c.fileUse[0].line > 0);
  assert.equal(c.hooks.length, 2);
  assert.equal(c.hooks[0].kind, 'beforeEach');
  assert.equal(c.hooks[0].suite, null);
  assert.deepEqual(c.hooks[0].requested, ['page']);
  assert.equal(c.hooks[1].kind, 'afterAll');
  assert.deepEqual([c.tests[9].hasEachArg, c.tests[10].hasEachArg, c.tests[0].hasEachArg], [true, true, false]);
  assert.equal(c.fixtures.length, 2);
  assert.equal(c.fixtures[0].name, 'greeting');
  assert.deepEqual(c.fixtures[0].deps, ['browserName']);
  assert.equal(c.fixtures[1].name, 'port');
  assert.equal(c.fixtures[1].option, true);
  assert.equal(c.fixtureSets.length, 2);
  assert.deepEqual(c.fixtureSets[1], [0, 1]);
  assert.deepEqual([c.tests[11].fixtureSet, c.tests[0].fixtureSet], [1, 0]);
  for (const entry of c.tests) {
    assert.ok(entry.line > 0, entry.title);
    assert.ok(entry.source, entry.title);
    assert.ok(entry.source[0].endsWith('shapes.test.ts'), entry.title);
    assert.ok(entry.source[1] > 0, entry.title);
  }
  const lines = c.tests.map(t => t.source[1]);
  assert.deepEqual(lines, [...lines].sort((a, b) => a - b));
});

test('focused tests and configured parameterized suites retain their plan', async () => {
  const c = await collect(focused, 'only.test.ts');
  assert.equal(c.hasOnly, true);
  assert.equal(c.tests[0].annotations[0].kind, 'only');
  assert.equal(c.suites[0].name, 'configured');
  assert.equal(c.suites[0].mode, 'serial');
  assert.equal(c.suites[0].retries, 3);
  assert.equal(c.suites[0].timeoutMs, 9000);
  assert.deepEqual(c.suites.slice(1).map(s => s.name), ['on cdp', 'on webkit']);
  assert.deepEqual([c.tests[2].suite, c.tests[3].suite], [1, 2]);
});

test('only truthy file-scope modifiers annotate the file', async () => {
  const c = await collect(`import { test } from '@ferridriver/test';
    test.skip(); test.slow(false, 'not this one'); test('a', async () => {});`, 'annotated.test.ts');
  assert.deepEqual(c.fileAnnotations.flatMap(item => item.annotations.map(a => a.kind)), ['skip']);
});

for (const host of ['script', 'bdd', 'mcp']) {
  test(`${host} sessions expose the complete test and fixture composition surface`, async () => {
    await collect(`import { test, describe, expect, mergeTests } from '@ferridriver/test';
      for (const [name, value] of Object.entries({ test, describe, expect, mergeTests })) {
        if (typeof value !== 'function') throw new Error(name + ' missing');
      }
      const a = test.extend({ a: async ({}, use) => { await use(1); } });
      const b = test.extend({ b: async ({}, use) => { await use(2); } });
      if (typeof mergeTests(a, b).extend !== 'function') throw new Error('merged chain missing');`, 'probe.test.ts', host);
  });
}

test('registering a test outside a test host emits its named diagnostic', async () => {
  const cwd = await workspace({ 'probe.test.ts': "import { test } from '@ferridriver/test'; test('inert', async () => {});" });
  const result = await run(['run', '--no-inherit', '--json', join(cwd, 'probe.test.ts')], { cwd, env: { RUST_LOG: 'warn' } });
  passed(result);
  assert.match(result.stderr, /test\.registration\.ignored/);
});
