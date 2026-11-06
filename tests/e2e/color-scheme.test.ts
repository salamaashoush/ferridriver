// A page whose context names no colour scheme is emulated light, on every
// engine and whatever the host's appearance. Left to itself WebKit follows
// the host, so on a Mac in dark mode it matched `prefers-color-scheme: dark`
// and painted a page with no colour scheme of its own on a dark canvas.
// `null` turns the emulation off, and the page follows the browser again.

import { test, describe, expect } from '@ferridriver/test';
import type { Browser, BrowserContextOptions, Page } from '@ferridriver/test';
import { decodePng } from './helpers/png';

async function scheme(page: Page): Promise<{ light: boolean; dark: boolean }> {
  return (await page.evaluate(() => ({
    light: matchMedia('(prefers-color-scheme: light)').matches,
    dark: matchMedia('(prefers-color-scheme: dark)').matches,
  }))) as { light: boolean; dark: boolean };
}

async function inContext<T>(
  browser: Browser,
  options: BrowserContextOptions,
  body: (page: Page) => Promise<T>,
): Promise<T> {
  const context = await browser.newContext(options);
  try {
    const page = await context.newPage();
    // WebKit's initial about:blank reports light whatever the host does;
    // a document of the page's own is what follows the host.
    await page.setContent('<p>text</p>');
    return await body(page);
  } finally {
    await context.close();
  }
}

describe('colour scheme', () => {
  test('a context that names none is emulated light', async ({ browser }) => {
    const unset = await inContext(browser, {}, scheme);
    expect(unset).toEqual({ light: true, dark: false });
    expect(unset).toEqual(await inContext(browser, { colorScheme: 'light' }, scheme));
  });

  test('the runner page draws a page without a colour scheme on a light canvas', async ({ page }) => {
    await page.setContent('<p>text</p>');
    expect(await scheme(page)).toEqual({ light: true, dark: false });
    const shot = decodePng(await page.screenshot({ scale: 'css' }));
    expect(shot.pixel(shot.width - 10, shot.height - 10)).toEqual([255, 255, 255, 255]);
  });

  test('a context that names dark is emulated dark', async ({ browser }) => {
    expect(await inContext(browser, { colorScheme: 'dark' }, scheme)).toEqual({ light: false, dark: true });
  });

  test('null leaves the page to the browser', async ({ browser }) => {
    // What the browser does by itself depends on the host, so `null` is
    // held to agreeing with itself: a context created with it, and a page
    // that clears its own override, both end up where the browser is.
    const own = await inContext(browser, { colorScheme: null }, async page => {
      const before = await scheme(page);
      await page.emulateMedia({ colorScheme: before.dark ? 'light' : 'dark' });
      expect((await scheme(page)).dark).toBe(!before.dark);
      await page.emulateMedia({ colorScheme: null });
      expect(await scheme(page)).toEqual(before);
      return before;
    });
    const cleared = await inContext(browser, {}, async page => {
      await page.emulateMedia({ colorScheme: null });
      return scheme(page);
    });
    expect(cleared).toEqual(own);
  });

  test("a page's own scheme outlives its context re-applying options", async ({ browser }) => {
    await inContext(browser, { colorScheme: 'light' }, async page => {
      await page.emulateMedia({ colorScheme: 'dark' });
      // Every context setter re-applies the whole options bag to each page.
      await page.context().setOffline(false);
      expect((await scheme(page)).dark).toBe(true);
      // Another handle to the same page continues from its state.
      await page.context().pages()[0].emulateMedia({});
      expect((await scheme(page)).dark).toBe(true);
    });
  });

  test('a popup is emulated light like the page that opened it', async ({ browser }) => {
    await inContext(browser, {}, async page => {
      const [popup] = await Promise.all([
        page.context().waitForEvent('page') as Promise<Page>,
        page.evaluate(() => void window.open('about:blank')),
      ]);
      await popup.waitForLoadState();
      await popup.setContent('<p>popup</p>');
      expect(await scheme(popup)).toEqual({ light: true, dark: false });
    });
  });

  test("a popup's first document already has its context's scheme", async ({ browser, baseURL }) => {
    // The popup's own script records what it matches while it parses,
    // before the popup can have been registered and configured.
    await inContext(browser, { colorScheme: 'dark' }, async page => {
      await page.goto(`${baseURL}/empty.html`);
      const [popup] = await Promise.all([
        page.context().waitForEvent('page') as Promise<Page>,
        page.evaluate(() => void window.open('/early-media.html')),
      ]);
      await popup.waitForLoadState();
      expect(await popup.evaluate(() => (window as unknown as { __early: unknown }).__early)).toEqual({
        dark: true,
        reduce: false,
      });
    });
    // A blank popup has no request to hold back until it is configured;
    // the opener reads it the moment it exists.
    await inContext(browser, { colorScheme: 'dark' }, async page => {
      await page.goto(`${baseURL}/empty.html`);
      const dark = await page.evaluate(() => window.open('')!.matchMedia('(prefers-color-scheme: dark)').matches);
      expect(dark).toBe(true);
    });
    // The context's, not the opener's own override.
    await inContext(browser, {}, async page => {
      await page.goto(`${baseURL}/empty.html`);
      await page.emulateMedia({ colorScheme: 'dark' });
      const [popup] = await Promise.all([
        page.context().waitForEvent('page') as Promise<Page>,
        page.evaluate(() => void window.open('/early-media.html')),
      ]);
      await popup.waitForLoadState();
      expect(await popup.evaluate(() => (window as unknown as { __early: unknown }).__early)).toEqual({
        dark: false,
        reduce: false,
      });
    });
  });
});

describe('use({ colorScheme: null })', () => {
  test.use({ colorScheme: null });

  test('turns the emulation off for the runner page', async ({ page, browser }) => {
    await page.setContent('<p>text</p>');
    expect(await scheme(page)).toEqual(await inContext(browser, { colorScheme: null }, scheme));
  });
});
