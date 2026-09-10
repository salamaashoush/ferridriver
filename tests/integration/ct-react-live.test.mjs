import { test, expect } from '@ferridriver/test';

const url = process.env.VITE_URL;

if (!url) {
  test.skip('React component counter (set VITE_URL to run)', () => {});
} else {
  test('React component counter renders and responds to input', async ({ page }) => {
    await page.goto(url);
    await expect.poll(() => page.locator('#count').textContent(), { timeout: 5000 }).toBe('0');
    await page.locator('#inc').click({ clickCount: 3 });
    await expect(page.locator('#count')).toHaveText('3');
    await page.locator('#dec').click();
    await expect(page.locator('#count')).toHaveText('2');
  });
}
