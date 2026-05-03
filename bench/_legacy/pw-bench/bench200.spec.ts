import { test, expect } from '@playwright/test';
function dataUrl(html: string): string { return `data:text/html,${encodeURIComponent(html)}`; }
for (let i = 0; i < 200; i++) {
  if (i % 2 === 0) {
    test(`nav_${String(i).padStart(3, '0')}`, async ({ page }) => {
      await page.goto(dataUrl(`<title>Test ${i}</title><body><h1>Page ${i}</h1></body>`));
      expect(await page.title()).toContain(`Test ${i}`);
    });
  } else {
    test(`interact_${String(i).padStart(3, '0')}`, async ({ page }) => {
      await page.goto(dataUrl(`<button id='btn' onclick="this.textContent='done ${i}'">Click ${i}</button>`));
      await page.locator('#btn').click();
      expect(await page.locator('#btn').textContent()).toContain(`done ${i}`);
    });
  }
}
