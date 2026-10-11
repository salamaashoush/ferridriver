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

test('test.step params and subtitle reach reporters', async () => {
  const cwd = await workspace({
    'ferridriver.toml': '[test]\ntestMatch = ["steps.spec.ts"]\nworkers = 1\nretries = 0\n' +
      'reporter = [{ name = "perfetto" }, { name = "html" }, { name = "json" }, { name = "./recorder.ts" }]\n',
    'recorder.ts': `
import { writeFileSync } from 'node:fs';
const seen = { begin: [], end: [], tree: [] };
const pick = step => ({ title: step.title, subtitle: step.subtitle ?? null, params: step.params ?? null });
export default class Recorder {
  onStepBegin(test, result, step) { if (step.category === 'test.step') seen.begin.push(pick(step)); }
  onStepEnd(test, result, step) { if (step.category === 'test.step') seen.end.push(pick(step)); }
  onTestEnd(test, result) {
    const walk = steps => steps.filter(s => s.category === 'test.step').map(s => ({ ...pick(s), steps: walk(s.steps) }));
    seen.tree = walk(result.steps);
  }
  onEnd() { writeFileSync('steps.json', JSON.stringify(seen)); }
}
`,
    'steps.spec.ts': `
import { test } from '@ferridriver/test';
test('buys', async () => {
  await test.step('checkout', async () => {
    await test.step('pay', async () => {}, { subtitle: 'card' });
  }, { params: { orderId: 42, items: ['a', 'b'] }, subtitle: 'SKU 42' });
  await test.step('plain', async () => {});
});
`,
  });
  passed(await run(['test', '--no-inherit', '--headless'], { cwd }));

  const checkout = { title: 'checkout', subtitle: 'SKU 42', params: { orderId: 42, items: ['a', 'b'] } };
  const pay = { title: 'pay', subtitle: 'card', params: null };
  const plain = { title: 'plain', subtitle: null, params: null };
  const seen = JSON.parse(await readFile(join(cwd, 'steps.json'), 'utf8'));
  assert.deepEqual(seen.begin, [checkout, pay, plain]);
  assert.deepEqual(seen.tree, [{ ...checkout, steps: [{ ...pay, steps: [] }] }, { ...plain, steps: [] }]);

  const trace = JSON.parse(await readFile(join(cwd, 'test-results', 'perfetto.json'), 'utf8'));
  const slices = trace.traceEvents.filter(e => e.cat === 'test.step').map(e => [e.name, e.args?.params ?? null]);
  assert.deepEqual(slices, [['checkout SKU 42', checkout.params], ['pay card', null], ['plain', null]]);

  const html = await readFile(join(cwd, 'test-results', 'report.html'), 'utf8');
  assert.ok(html.includes('"title":"checkout","subtitle":"SKU 42"'), 'the HTML report carries the subtitle');

  // A report lists steps as the tree they ran as, in the order they started.
  const outline = steps => steps.map(step => [step.title, outline(step.steps ?? [])]);
  const json = JSON.parse(await readFile(join(cwd, 'test-results', 'results.json'), 'utf8'));
  const result = json.suites[0].specs[0].tests[0].results[0];
  assert.deepEqual(outline(result.steps), [['checkout', [['pay', []]]], ['plain', []]]);
  const report = JSON.parse(html.match(/<script[^>]*type="application\/json"[^>]*>([\s\S]*?)<\/script>/)[1].replaceAll('<\\/', '</'));
  assert.deepEqual(outline(report.tests[0].attempts[0].steps), [['checkout', [['pay', []]]], ['plain', []]]);
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

test('a timed-out test names the call it was stuck in, on a browser it launched itself', async () => {
  const cwd = await workspace({
    'ferridriver.toml': '[test]\ntestMatch = ["stuck.spec.ts"]\nworkers = 1\nretries = 0\ntimeout = 1500\n',
    'stuck.spec.ts': `
import { test } from '@ferridriver/test';
test('waits for an element that never appears', async () => {
  const browser = await chromium().launch({ headless: true });
  const page = await browser.newPage();
  await page.goto('data:text/html,<h1>sashoush</h1>');
  await page.locator('#nope').click();
});
`,
  });
  const result = await run(['test', '--no-inherit', '--headless'], { cwd });
  assert.equal(result.code, 1, result.text);
  assert.match(result.text,
    /test timed out after 1\.5s\n\n {4}locator\.click had not finished \(stuck\.spec\.ts:7\)\n {4}Call log:\n {4} {2}- waiting for locator\('#nope'\)/);
});
