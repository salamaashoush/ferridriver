// Head-to-head benchmark: same tests for both Playwright and ferridriver.
// Tests: navigate data URL, read title, click button, evaluate JS, screenshot.
import { test, expect } from '@ferridriver/test';

function dataUrl(html: string): string {
  return `data:text/html,${encodeURIComponent(html)}`;
}

// 100 tests — mixed navigation, interaction, and evaluation
for (let i = 0; i < 100; i++) {
  const kind = i % 3;
  if (kind === 0) {
    test(`nav_${String(i).padStart(3, '0')}`, async ({ page }) => {
      await page.goto(dataUrl(`<title>Test ${i}</title><body><h1>Page ${i}</h1></body>`));
      await expect(page).toHaveTitle(`Test ${i}`);
    });
  } else if (kind === 1) {
    test(`click_${String(i).padStart(3, '0')}`, async ({ page }) => {
      await page.goto(dataUrl(`<button id='btn' onclick="this.textContent='done ${i}'">Click ${i}</button>`));
      await page.locator('#btn').click();
      await expect(page.locator('#btn')).toHaveText(`done ${i}`);
    });
  } else {
    test(`eval_${String(i).padStart(3, '0')}`, async ({ page }) => {
      await page.goto(dataUrl(`<title>Eval ${i}</title><div id='out'>${i}</div>`));
      const text = await page.evaluate(`document.getElementById('out')?.textContent`);
      if (text !== String(i)) throw new Error(`expected ${i}, got ${text}`);
    });
  }
}
