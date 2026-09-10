import { test, expect } from '@ferridriver/test';

test('an exposed binding can navigate without blocking browser events', async ({ page }) => {
  const destination = 'data:text/html,<title>Binding destination</title><p>Navigation finished</p>';
  let complete!: () => void;
  let fail!: (error: unknown) => void;
  const navigation = new Promise<void>((resolve, reject) => { complete = resolve; fail = reject; });
  await page.exposeFunction('__navigateFromBinding', async () => {
    try {
      await page.goto(destination);
      complete();
    } catch (error) {
      fail(error);
    }
  });
  await page.evaluate('setTimeout(() => { void window.__navigateFromBinding(); }, 0)');
  await navigation;
  expect(page.url()).toBe(destination);
  await expect(page.locator('p')).toHaveText('Navigation finished');
});
