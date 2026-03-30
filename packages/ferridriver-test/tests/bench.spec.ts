// Benchmark: 50 tests — identical workload to Playwright bench.
// Navigation + click + text verification.

import { test, expect } from '../src/index.js';

function dataUrl(html: string): string {
  return `data:text/html,${encodeURIComponent(html)}`;
}

for (let i = 0; i < 50; i++) {
  if (i % 2 === 0) {
    test(`nav_${String(i).padStart(3, '0')}`, async ({ page }) => {
      const html = `<title>Test ${i}</title><body><h1>Page ${i}</h1></body>`;
      await page.goto(dataUrl(html));
      const title = await page.title();
      if (!title.includes(`Test ${i}`)) throw new Error(`title mismatch: ${title}`);
    });
  } else {
    test(`interact_${String(i).padStart(3, '0')}`, async ({ page }) => {
      const html = `<button id='btn' onclick="this.textContent='done ${i}'">Click ${i}</button>`;
      await page.goto(dataUrl(html));
      await page.locator('#btn').click();
      const text = await page.locator('#btn').textContent();
      if (!text?.includes(`done ${i}`)) throw new Error(`text mismatch: ${text}`);
    });
  }
}
