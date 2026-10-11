// A page that has not navigated is on its initial empty document, and
// Playwright's `page.url()` reads `about:blank` there, as the document
// itself does.
import { test, expect } from '@ferridriver/test';
import type { Page } from '@ferridriver/test';

test('a new page is at about:blank', async ({ context }) => {
  const page = await context.newPage();
  expect(page.url()).toBe('about:blank');
  expect(page.mainFrame().url()).toBe('about:blank');
  expect(await page.evaluate(() => location.href)).toBe('about:blank');
});

test('a popup opened without a url is at about:blank', async ({ page, baseURL }) => {
  await page.goto(`${baseURL}/fx/landed`);
  const [popup] = await Promise.all([
    page.waitForEvent('popup') as Promise<Page>,
    page.evaluate(() => {
      window.open();
    }),
  ]);
  expect(popup.url()).toBe('about:blank');
  expect(popup.mainFrame().url()).toBe('about:blank');
  // The opener keeps its own url.
  expect(page.url()).toBe(`${baseURL}/fx/landed`);
});

test('the url follows the first navigation', async ({ context, baseURL }) => {
  const page = await context.newPage();
  await page.goto(`${baseURL}/fx/landed`);
  expect(page.url()).toBe(`${baseURL}/fx/landed`);
  await page.goto('about:blank');
  expect(page.url()).toBe('about:blank');
});
