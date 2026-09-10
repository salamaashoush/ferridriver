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
