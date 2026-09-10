import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { passed, run, workspace } from './support.mjs';

test('native JS runner preserves locator matchers, polling, retries, and page assertions', async () => {
  const cwd = await workspace({
    'ferridriver.toml': `[test]
testMatch = ["features.spec.ts"]
workers = 2
timeout = 15000
expectTimeout = 5000
retries = 0
`,
    'features.spec.ts': `
import { test, expect } from '@ferridriver/test';

test('flaky test passes on retry', { retries: 1 }, async ({ testInfo }) => {
  if (testInfo.retry === 0) throw new Error('intentional first-attempt failure');
});

test('all locator matchers', async ({ page }) => {
  await page.goto('data:text/html,' + encodeURIComponent(\
    '<div id="visible">Visible</div><div id="hidden" style="display:none">Hidden</div>' +
    '<button id="btn" disabled class="primary large" aria-label="Submit Form" aria-description="Submits the form">Submit</button>' +
    '<input id="inp" type="text" value="hello"><input id="check" type="checkbox" checked>' +
    '<textarea id="area">Editable</textarea><select id="multi" multiple>' +
    '<option value="a" selected>A</option><option value="b" selected>B</option></select>' +
    '<div id="empty"></div><div id="styled" style="color:rgb(255, 0, 0)">Red</div>' +
    '<div class="extra"></div><div class="extra"></div><div class="extra"></div>'));
  await expect(page.locator('#visible')).toBeVisible();
  await expect(page.locator('#hidden')).toBeHidden();
  await expect(page.locator('#visible')).not.toBeHidden();
  await expect(page.locator('#btn')).toBeDisabled();
  await expect(page.locator('#inp')).toBeEnabled();
  await expect(page.locator('#check')).toBeChecked();
  await expect(page.locator('#area')).toBeEditable();
  await expect(page.locator('#visible')).toBeAttached();
  await expect(page.locator('#empty')).toBeEmpty();
  await expect(page.locator('#visible')).not.toBeEmpty();
  await expect(page.locator('#visible')).toHaveText('Visible');
  await expect(page.locator('#btn')).toContainText('Submit');
  await expect(page.locator('#inp')).toHaveValue('hello');
  await expect(page.locator('#inp')).toHaveAttribute('type', 'text');
  await expect(page.locator('#btn')).toHaveClass('primary large');
  await expect(page.locator('#btn')).toContainClass('primary');
  await expect(page.locator('#btn')).not.toContainClass('secondary');
  await expect(page.locator('#btn')).toHaveId('btn');
  await expect(page.locator('#btn')).toHaveRole('button');
  await expect(page.locator('#btn')).toHaveAccessibleName('Submit Form');
  await expect(page.locator('#btn')).toHaveAccessibleDescription('Submits the form');
  await expect(page.locator('.extra, #visible')).toHaveCount(4);
  await expect(page.locator('#styled')).toHaveCSS('color', 'rgb(255, 0, 0)');
  await expect(page.locator('#inp')).toHaveJSProperty('type', 'text');
  await expect(page.locator('#multi')).toHaveValues(['a', 'b']);
});

test('expect poll reaches a changing value', async () => {
  let value = 0;
  await expect.poll(() => ++value, { intervals: [10], timeout: 1000 }).toSatisfy(v => v >= 5);
});

test('toPass waits for a page update', async ({ page }) => {
  await page.goto('data:text/html,<div id="status">loading</div><script>setTimeout(() => document.getElementById("status").textContent = "ready", 100)</script>');
  await expect(async () => expect(await page.locator('#status').textContent()).toBe('ready'))
    .toPass({ intervals: [10], timeout: 2000 });
});

test('page title and URL assertions', async ({ page }) => {
  await page.goto('data:text/html,<title>My%20Title</title><body>Hello</body>');
  await expect(page).toHaveTitle('My Title');
  await expect(page).not.toHaveTitle('Wrong Title');
  await expect(page).toHaveURL(/^data:/);
});
`,
  });

  const result = await run(['test', '--no-inherit', '--config', 'ferridriver.toml', '--headless'], { cwd });
  passed(result);
  assert.match(result.text, /4 passed/);
  assert.match(result.text, /1 flaky/);
});
