// `networkidle` is a lifecycle event of the document, as Playwright's
// frames.ts keeps it: it fires once 500ms pass with nothing in flight,
// and stays fired for that document. A wait after it returns at once,
// even while requests the page started since are still running; a new
// document has to go quiet again.

import { describe, expect, test } from '@ferridriver/test';

describe('waitForLoadState networkidle', () => {
  test('waits for the requests in flight, then for 500ms of quiet', async ({ page }) => {
    await page.goto('/fx/landed');
    await page.evaluate(() => {
      (window as any).done = false;
      fetch('/fx/slow?ms=800').then(() => ((window as any).done = true));
    });
    await page.waitForLoadState('networkidle');
    expect(await page.evaluate(() => (window as any).done)).toBe(true);
  });

  test('returns at once after it fired, while later requests run', async ({ page }) => {
    await page.goto('/fx/landed');
    await page.waitForLoadState('networkidle');
    await page.evaluate(() => {
      (window as any).done = false;
      fetch('/fx/slow?ms=3000').then(() => ((window as any).done = true));
    });
    const started = Date.now();
    await page.waitForLoadState('networkidle');
    expect(Date.now() - started).toBeLessThan(1000);
    expect(await page.evaluate(() => (window as any).done)).toBe(false);
  });

  test('a new document goes quiet again before it counts', async ({ page }) => {
    await page.goto('/fx/landed');
    await page.waitForLoadState('networkidle');
    await page.goto('/fx/landed?again');
    await page.evaluate(() => {
      (window as any).done = false;
      fetch('/fx/slow?ms=800').then(() => ((window as any).done = true));
    });
    await page.waitForLoadState('networkidle');
    expect(await page.evaluate(() => (window as any).done)).toBe(true);
  });
});
