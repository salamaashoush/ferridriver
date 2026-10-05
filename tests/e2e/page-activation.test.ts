import { test, expect } from '@ferridriver/test';

test('bringToFront activates the page without executing page focus code', async ({ page }) => {
  await page.evaluate(() => {
    window.focus = () => { throw new Error('Page focus override must not run'); };
  });
  await page.bringToFront();
  expect(await page.evaluate(() => document.visibilityState)).toBe('visible');
});

test('bringToFront rejects a closed page', async ({ context }) => {
  const closed = await context.newPage();
  await closed.close();
  await expect(closed.bringToFront()).rejects.toThrow();
});
