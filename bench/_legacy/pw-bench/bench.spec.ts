// Equivalent benchmark to ferridriver-test bench_runner.rs
// Tests: navigate to data URL, read title, click button, verify text.

import { test, expect } from '@playwright/test';

function dataUrl(html: string): string {
  return `data:text/html,${encodeURIComponent(html)}`;
}

// Generate 50 tests — same mix as Rust benchmark
for (let i = 0; i < 50; i++) {
  if (i % 2 === 0) {
    // Navigation test
    test(`nav_${String(i).padStart(3, '0')}`, async ({ page }) => {
      const html = `<title>Test ${i}</title><body><h1>Page ${i}</h1></body>`;
      await page.goto(dataUrl(html));
      const title = await page.title();
      expect(title).toContain(`Test ${i}`);
    });
  } else {
    // Interaction test
    test(`interact_${String(i).padStart(3, '0')}`, async ({ page }) => {
      const html = `<button id='btn' onclick="this.textContent='done ${i}'">Click ${i}</button>`;
      await page.goto(dataUrl(html));
      await page.locator('#btn').click();
      const text = await page.locator('#btn').textContent();
      expect(text).toContain(`done ${i}`);
    });
  }
}
