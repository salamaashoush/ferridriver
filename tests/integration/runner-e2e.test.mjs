import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { passed, run, workspace } from './support.mjs';

test('native JS runner executes browser fixtures, retries, workers, and skips', async () => {
  const cwd = await workspace({
    'ferridriver.toml': `[test]
testMatch = ["runner.spec.ts"]
workers = 2
timeout = 15000
expectTimeout = 5000
headless = true
`,
    'runner.spec.ts': `
import { test, expect } from '@ferridriver/test';

test.describe('navigation', () => {
  test('basic navigation', async ({ page }) => {
    await page.goto('data:text/html,<title>Test%20Page</title><h1>Hello%20World</h1>');
    await expect(page).toHaveTitle('Test Page');
    await expect(page.locator('h1')).toHaveText('Hello World');
  });
});

test.describe('interaction', () => {
  test('click button', async ({ page }) => {
    await page.goto("data:text/html,<button id='btn' onclick=\\"this.textContent='clicked'\\">Click%20Me</button>");
    await page.locator('#btn').click();
    await expect(page.locator('#btn')).toHaveText('clicked');
  });

  test('fill input', async ({ page }) => {
    await page.goto("data:text/html,<input id='input' type='text'>");
    await page.locator('#input').fill('hello world');
    await expect(page.locator('#input')).toHaveValue('hello world');
  });
});

test('auto retry expectation', async ({ page }) => {
  await page.goto("data:text/html,<div id='message'>Initial</div><button id='go' onclick=\\"setTimeout(() => message.textContent='Updated', 100)\\">Go</button>");
  await page.locator('#go').click();
  await expect(page.locator('#message')).toHaveText('Updated');
  await expect(page.locator('#message')).not.toHaveText('Initial');
});

test.skip('skipped test', async () => {
  throw new Error('a skipped test must not execute');
});
`,
  });

  const result = await run(['test', '--no-inherit', '--config', 'ferridriver.toml', '--headless'], { cwd });
  passed(result);
  assert.match(result.text, /4 passed/);
  assert.match(result.text, /1 skipped/);
});
