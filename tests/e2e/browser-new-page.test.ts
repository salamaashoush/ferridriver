// `browser.newPage()` opens a page in a context of its own, made from
// the options it is given over the test's `use` options, and closes that
// context with the page: Playwright's `Browser.newPage`
// (/tmp/playwright/packages/playwright-core/src/client/browser.ts), whose
// page carries `_ownedContext`. Two such pages therefore share no
// cookies, which is what a spec signing in two people at once relies on.

import { describe, expect, test } from '@ferridriver/test';

describe('browser.newPage', () => {
  test('each page has a context of its own, sharing no cookies', async ({ browser }) => {
    const first = await browser.newPage();
    const second = await browser.newPage();
    try {
      expect(first.context()).not.toBe(second.context());
      await first.goto('/fx/landed');
      await second.goto('/fx/landed');
      await first.evaluate(() => {
        document.cookie = 'who=first; path=/';
      });
      expect(await first.evaluate(() => document.cookie)).toBe('who=first');
      expect(await second.evaluate(() => document.cookie)).toBe('');
    } finally {
      await first.close();
      await second.close();
    }
  });

  test('takes its own options over the ones the test uses', async ({ browser, baseURL }) => {
    const page = await browser.newPage({ locale: 'de-DE', userAgent: 'Own/1.0' });
    try {
      await page.goto('/fx/landed');
      expect(page.url()).toBe(new URL('/fx/landed', baseURL).href);
      expect(await page.evaluate(() => [navigator.language, navigator.userAgent])).toEqual(['de-DE', 'Own/1.0']);
    } finally {
      await page.close();
    }
  });

  test('closing the page closes its context', async ({ browser }) => {
    const before = browser.contexts().length;
    const page = await browser.newPage();
    const context = page.context();
    expect(browser.contexts().length).toBe(before + 1);
    let closed = false;
    context.on('close', () => {
      closed = true;
    });
    await page.close();
    expect(closed).toBe(true);
    expect(browser.contexts().length).toBe(before);
  });
});
