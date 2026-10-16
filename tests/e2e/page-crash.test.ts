// page.on('crash'). Ported from Playwright's page-event-crash.spec.ts,
// which crashes Chromium with chrome://crash; the other engines have no
// page-reachable way to crash a renderer.
import { test, expect } from '@ferridriver/test';
import type { Page } from '@ferridriver/test';
import { runOnceAcrossProjects } from './helpers/projects';

for (const transport of ['pipe', 'ws'] as const) {
  test(`a renderer crash is page.on('crash') over ${transport}`, async () => {
    runOnceAcrossProjects();
    const browser = await chromium({ transport }).launch({ headless: true });
    try {
      const page = await browser.newPage();
      await page.goto('http://127.0.0.1:47831/empty.html');
      const crashed = page.waitForEvent('crash');
      page.goto('chrome://crash').catch(() => null);
      expect(await crashed).toBe(page);
      expect(page.isClosed()).toBe(false);
    } finally {
      await browser.close();
    }
  });
}

test('crash is not fired by a close', async ({ browserName }) => {
  const factory = browserName === 'firefox' ? firefox() : browserName === 'webkit' ? webkit() : chromium();
  const browser = await factory.launch({ headless: true });
  try {
    const page: Page = await browser.newPage();
    let crashes = 0;
    page.on('crash', () => { crashes++; });
    await page.goto('http://127.0.0.1:47831/empty.html');
    await page.goto('http://localhost:47831/empty.html');
    await page.close();
    expect(crashes).toBe(0);
  } finally {
    await browser.close();
  }
});
