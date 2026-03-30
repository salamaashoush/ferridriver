import { test } from '../src/index.js';

test('smoke test', async ({ page }) => {
  await page.goto('data:text/html,<title>Hello</title>');
  const title = await page.title();
  if (title !== 'Hello') throw new Error(`expected Hello, got ${title}`);
});
