import { test } from '@ferridriver/test';

test('passes', async ({ page }) => {
  await page.goto('data:text/html,<title>ok</title>');
});

test('fails', async () => {
  throw new Error('deliberate red');
});
