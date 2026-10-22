import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { passed, repo, run, workspace } from './support.mjs';

async function reporterWorkspace(reporter) {
  return workspace({
    'specs/report.test.ts': `import { test, expect } from '@ferridriver/test';
      test('adds a row', async ({ page }) => {
        await page.setContent('<b>hi</b>');
        await test.step('read the text', async () => {
          expect(await page.locator('b').textContent()).toBe('hi');
        });
      });`,
    'reporters/counting-reporter.ts': reporter,
    'ferridriver.toml': `[test]
      testMatch = ["specs/*.test.ts"]
      workers = 2
      reporter = [{ name = "./reporters/counting-reporter.ts", outputFile = "summary.json" }]
      [[test.projects]]
      name = "cdp-pipe"
      [test.projects.browser]
      browser = "chromium"
      backend = "cdp-pipe"
      headless = true
      [[test.projects]]
      name = "bidi"
      [test.projects.browser]
      browser = "firefox"
      backend = "bidi"
      headless = true
    `,
  });
}

test('a reporter module receives one run boundary and each project outcome', async () => {
  const cwd = await reporterWorkspace(await readFile(join(repo, 'tests/fixtures/counting-reporter.ts'), 'utf8'));
  passed(await run(['test', '--no-inherit', '--headless'], { cwd }));
  const summary = JSON.parse(await readFile(join(cwd, 'summary.json'), 'utf8'));
  for (const hook of ['onBegin', 'onEnd']) assert.equal(summary.calls[hook], 1, hook);
  for (const hook of ['onTestBegin', 'onTestEnd']) assert.equal(summary.calls[hook], 2, hook);
  assert.equal(summary.configuredCalled, false);
  assert.equal(summary.allTests.length, 2);
  for (const project of ['cdp-pipe', 'bidi']) assert.ok(JSON.stringify(summary.allTests).includes(project));
  assert.deepEqual(summary.entryTypes, ['project', 'project']);
  assert.deepEqual(summary.statuses, ['passed', 'passed']);
  assert.deepEqual(summary.outcomes, ['expected', 'expected']);
  assert.ok(summary.stepTitles.includes('read the text'));
  assert.deepEqual(summary.errors, []);
});

test('a reporter constructor failure fails the command and preserves its message', async () => {
  const cwd = await reporterWorkspace(`export default class Broken {
    constructor() { throw new Error('no reporter for you'); }
  }`);
  const result = await run(['test', '--no-inherit', '--headless'], { cwd });
  assert.notEqual(result.code, 0, result.text);
  assert.match(result.text, /no reporter for you/);
});

test('a reporter module prints to the terminal like any Playwright reporter', async () => {
  const cwd = await reporterWorkspace(`export default class Loud {
    onBegin() {
      console.log('reporter console.log');
      process.stdout.write('reporter stdout write\\n');
      console.error('reporter console.error');
    }
  }`);
  const result = await run(['test', '--no-inherit', '--headless', '--project', 'cdp-pipe'], { cwd });
  passed(result);
  assert.match(result.stdout, /^reporter console\.log$/m);
  assert.match(result.stdout, /^reporter stdout write$/m);
  assert.match(result.stderr, /^reporter console\.error$/m);
});

test('a reporter module receives browser calls and assertions as live nested steps', async () => {
  const cwd = await workspace({
    'specs/steps.test.ts': `import { test, expect } from '@ferridriver/test';
      test('home page loads', async ({ page }) => {
        await test.step('open the site', async () => {
          await page.goto('data:text/html,<title>Example</title><h1>hi</h1>');
        });
        await expect(page).toHaveTitle(/Example/);
        expect(await page.title()).not.toBe('Other');
        await Promise.all([page.getByRole('heading').click(), page.getByRole('heading').hover()]);
        await expect(async () => { await page.getByRole('heading').click(); }).toPass();
        await expect.soft(page.getByRole('heading')).toHaveText('nope', { timeout: 100 });
      });`,
    'reporters/steps.ts': `export default class Steps {
      onStepBegin(test, result, step) {
        process.stdout.write('BEGIN ' + JSON.stringify([step.category, step.title, step.subtitle ?? null,
          step.parent ? step.parent.title : null, step.location ? step.location.line : null]) + '\\n');
      }
      onStepEnd(test, result, step) {
        console.log('END ' + JSON.stringify([step.category, step.title, step.error ? step.error.message.split('\\n')[0] : null]));
      }
    }`,
    'ferridriver.toml': `[test]
      testMatch = ["specs/*.test.ts"]
      workers = 1
      reporter = [{ name = "./reporters/steps.ts" }]
      [test.browser]
      browser = "chromium"
      backend = "cdp-pipe"
      headless = true
    `,
  });
  const result = await run(['test', '--no-inherit', '--headless'], { cwd });
  assert.equal(result.code, 1, result.text);
  const lines = result.stdout.split('\n').filter(line => /^(BEGIN|END) /.test(line))
    .map(line => [line.slice(0, line.indexOf(' ')), ...JSON.parse(line.slice(line.indexOf(' ') + 1))]);
  const begins = lines.filter(([kind]) => kind === 'BEGIN').map(([, ...rest]) => rest);
  assert.deepEqual(begins.map(([category, title, subtitle, parent]) => [category, title, subtitle, parent]), [
    ['test.step', 'open the site', null, null],
    ['pw:api', 'Navigate', 'data:', 'open the site'],
    ['expect', 'Expect "toHaveTitle"', null, null],
    ['expect', 'Expect "not toBe"', null, null],
    ['pw:api', 'Click', "getByRole('heading')", null],
    ['pw:api', 'Hover', "getByRole('heading')", null],
    ['expect', 'Expect "toPass"', null, null],
    ['pw:api', 'Click', "getByRole('heading')", 'Expect "toPass"'],
    ['expect', 'Expect "soft toHaveText"', "getByRole('heading')", null],
  ], result.stdout);
  assert.deepEqual(begins.map(step => step[4]), [3, 4, 6, 7, 8, 8, 9, 9, 10], 'each step names the line that made it');
  const order = lines.map(([kind, category, title]) => `${kind} ${title}`);
  assert.ok(order.indexOf('END Navigate') < order.indexOf('END open the site'), 'a child ends before its step');
  assert.ok(order.indexOf('END open the site') < order.indexOf('BEGIN Expect "toHaveTitle"'), 'steps arrive as they happen');
  const soft = lines.find(([kind, , title]) => kind === 'END' && title === 'Expect "soft toHaveText"');
  assert.match(soft[3], /toHaveText/, 'a failed soft assertion fails its step');
});
