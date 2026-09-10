import assert from 'node:assert/strict';
import { readFile, stat } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { passed, run, workspace } from './support.mjs';

test('native JS runner executes lifecycle hooks and worker fixtures', async () => {
  const cwd = await workspace({
    'ferridriver.toml': '[test]\ntestMatch = ["hooks.spec.ts"]\nworkers = 1\nretries = 0\n',
    'hooks.spec.ts': `
import { readFile, writeFile } from 'node:fs/promises';
import { test as base, expect } from '@ferridriver/test';
let workerSetups = 0;
const record = async event => {
  let previous = '';
  try { previous = await readFile('events.log', 'utf8'); } catch (error) { if (error.code !== 'ENOENT') throw error; }
  await writeFile('events.log', previous + event + '\\n');
};
const test = base.extend({
  workerValue: [async ({}, use) => { workerSetups++; await use(workerSetups); }, { scope: 'worker', auto: true }],
});
test.beforeAll(async () => record('before-all'));
test.beforeEach(async ({ workerValue }) => record('before-each-' + workerValue));
test.afterEach(async () => record('after-each'));
test.afterAll(async () => record('after-all'));
test('first', async ({ workerValue }) => { expect(workerValue).toBe(1); });
test('second', async ({ workerValue, testInfo }) => {
  expect(workerValue).toBe(1);
  expect(testInfo.retry).toBe(0);
});
`,
  });
  const result = await run(['test', '--no-inherit', '--headless'], { cwd });
  passed(result);
  assert.match(result.text, /2 passed/);
  assert.deepEqual((await readFile(join(cwd, 'events.log'), 'utf8')).trim().split('\n'), [
    'before-all', 'before-each-1', 'after-each', 'before-each-1', 'after-each', 'after-all',
  ]);
});

test('serial mode skips later tests after a failure', async () => {
  const cwd = await workspace({
    'ferridriver.toml': '[test]\ntestMatch = ["serial.spec.ts"]\nworkers = 2\nretries = 0\n',
    'serial.spec.ts': `
import { writeFile } from 'node:fs/promises';
import { test } from '@ferridriver/test';
test.describe('ordered', () => {
  test.describe.configure({ mode: 'serial' });
  test('fails first', async () => { throw new Error('serial failure'); });
  test('must be skipped', async () => { await writeFile('ran', 'bad'); });
});
`,
  });
  const result = await run(['test', '--no-inherit', '--headless'], { cwd });
  assert.notEqual(result.code, 0, result.text);
  assert.match(result.text, /1 failed/);
  assert.match(result.text, /1 skipped/);
  await assert.rejects(stat(join(cwd, 'ran')), /ENOENT/);
});

test('expected failures and soft assertions remain visible to the runner', async () => {
  const cwd = await workspace({
    'ferridriver.toml': '[test]\ntestMatch = ["expect.spec.ts"]\nworkers = 1\nretries = 0\n',
    'expect.spec.ts': `
import { test, expect } from '@ferridriver/test';
test.fail('expected failure', async ({ testInfo }) => {
  expect.soft(1).toBe(2);
  expect(testInfo.retry).toBe(0);
  expect(testInfo.expectedStatus).toBe('failed');
  throw new Error('known failure');
});
`,
  });
  const result = await run(['test', '--no-inherit', '--headless'], { cwd });
  passed(result);
  assert.match(result.text, /1 passed/);
});

test('native JS runner writes the HTML reporter artifact', async () => {
  const cwd = await workspace({
    'ferridriver.toml': '[test]\ntestMatch = ["report.spec.ts"]\nworkers = 1\nreporter = ["html"]\n',
    'report.spec.ts': `import { test, expect } from '@ferridriver/test'; test('reported', () => expect(true).toBe(true));`,
  });
  passed(await run(['test', '--no-inherit', '--headless'], { cwd }));
  const report = await stat(join(cwd, 'test-results', 'report.html'));
  assert.ok(report.size > 1000);
});
