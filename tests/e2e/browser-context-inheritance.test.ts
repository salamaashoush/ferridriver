// A context a test makes with `browser.newContext()` starts from the
// test's own `use` options, key by key, where the call names nothing:
// Playwright's `runBeforeCreateBrowserContext`
// (/tmp/playwright/packages/playwright/src/index.ts). Each assertion reads
// a page-visible effect that only the inherited option produces.

import { test, describe, expect } from '@ferridriver/test';

describe('a context the test makes itself', () => {
  test.use({
    locale: 'de-DE',
    userAgent: 'Inherited/1.0',
    viewport: { width: 640, height: 480 },
  });

  let landedInHook = '';

  test.beforeAll(async ({ browser }) => {
    const context = await browser.newContext();
    try {
      const page = await context.newPage();
      await page.goto('/fx/landed');
      landedInHook = new URL(page.url()).pathname;
    } finally {
      await context.close();
    }
  });

  test('resolves a relative goto against the configured baseURL', async ({ browser, baseURL }) => {
    const context = await browser.newContext();
    try {
      const page = await context.newPage();
      await page.goto('/fx/landed');
      expect(page.url()).toBe(new URL('/fx/landed', baseURL).href);
    } finally {
      await context.close();
    }
  });

  test('takes the locale, user agent and viewport the test uses', async ({ browser }) => {
    const context = await browser.newContext();
    try {
      const page = await context.newPage();
      const seen = await page.evaluate(() => ({
        language: navigator.language,
        agent: navigator.userAgent,
        width: window.innerWidth,
      }));
      expect(seen).toEqual({ language: 'de-DE', agent: 'Inherited/1.0', width: 640 });
    } finally {
      await context.close();
    }
  });

  test('keeps what the call names over what the test uses', async ({ browser }) => {
    const context = await browser.newContext({ userAgent: 'Own/2.0', viewport: { width: 800, height: 600 } });
    try {
      const page = await context.newPage();
      const seen = await page.evaluate(() => ({
        language: navigator.language,
        agent: navigator.userAgent,
        width: window.innerWidth,
      }));
      expect(seen).toEqual({ language: 'de-DE', agent: 'Own/2.0', width: 800 });
    } finally {
      await context.close();
    }
  });

  test('applies in a beforeAll hook too', () => {
    expect(landedInHook).toBe('/fx/landed');
  });
});
