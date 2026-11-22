// A context's cookies live on the browser context, so Playwright sets
// and reads them before the context has a page: a spec seeds a language
// cookie, then opens the page that should see it.

import { describe, expect, test } from '@ferridriver/test';

describe('context cookies before the first page', () => {
  test('a cookie added first reaches the first page and reads back', async ({ browser, baseURL }) => {
    const context = await browser.newContext();
    try {
      await context.addCookies([{ name: 'language-tag', value: 'en', url: baseURL! }]);
      expect((await context.cookies()).map((c) => [c.name, c.value])).toEqual([['language-tag', 'en']]);
      const page = await context.newPage();
      await page.goto('/fx/landed');
      expect(await page.evaluate(() => document.cookie)).toBe('language-tag=en');
    } finally {
      await context.close();
    }
  });

  test('clearCookies empties a context that has no page yet', async ({ browser, baseURL }) => {
    const context = await browser.newContext();
    try {
      await context.addCookies([{ name: 'a', value: '1', url: baseURL! }]);
      await context.clearCookies();
      expect(await context.cookies()).toEqual([]);
      expect(context.pages()).toEqual([]);
    } finally {
      await context.close();
    }
  });
});
