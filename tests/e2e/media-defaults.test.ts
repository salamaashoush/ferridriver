// A page is emulated with no reduced motion, no forced colours and no
// contrast preference unless its context or `emulateMedia` names one, the
// way it is emulated light. Firefox overrides none of these per page, so
// there they stay the browser's own, and only a value asked for outright
// is refused.

import { test, describe, expect } from '@ferridriver/test';
import type { Browser, BrowserContextOptions, Page } from '@ferridriver/test';
import { runOnceAcrossProjects } from './helpers/projects';

async function features(page: Page): Promise<Record<string, boolean>> {
  await page.setContent('<p>text</p>');
  return (await page.evaluate(() => ({
    reduce: matchMedia('(prefers-reduced-motion: reduce)').matches,
    forced: matchMedia('(forced-colors: active)').matches,
    more: matchMedia('(prefers-contrast: more)').matches,
  }))) as Record<string, boolean>;
}

async function inContext<T>(browser: Browser, options: BrowserContextOptions, body: (page: Page) => Promise<T>) {
  const context = await browser.newContext(options);
  try {
    return await body(await context.newPage());
  } finally {
    await context.close();
  }
}

const NONE = { reduce: false, forced: false, more: false };
const ALL = { reduce: true, forced: true, more: true };
const NULLS = { reducedMotion: null, forcedColors: null, contrast: null } as const;

describe('media feature defaults', () => {
  test('a context that names none is emulated without them', async ({ browser }) => {
    expect(await inContext(browser, {}, features)).toEqual(NONE);
  });

  test('null is accepted on every backend', async ({ browser }) => {
    await inContext(browser, NULLS, async page => {
      await page.emulateMedia(NULLS);
      await features(page);
    });
  });

  test('Firefox refuses only a value asked for outright', async ({ browser, browserName }) => {
    test.skip(browserName !== 'firefox', 'the other engines emulate these');
    for (const options of [{ reducedMotion: 'reduce' }, { forcedColors: 'active' }, { contrast: 'more' }] as const) {
      let message = 'no-throw';
      await inContext(browser, {}, page => page.emulateMedia(options)).catch(e => {
        message = String((e as Error).message ?? e);
      });
      expect([options, message.includes('does not support')]).toEqual([options, true]);
    }
  });

  test("a host that prefers them gets the defaults, and null hands them back", async () => {
    runOnceAcrossProjects();
    // These switches make Chromium report what a host with reduced motion
    // and a high contrast theme would.
    const browser = await chromium().launch({
      headless: true,
      args: ['--force-prefers-reduced-motion', '--force-prefers-contrast=more', '--force-high-contrast'],
    });
    try {
      expect(await inContext(browser, {}, features)).toEqual(NONE);
      expect(await inContext(browser, NULLS, features)).toEqual(ALL);
    } finally {
      await browser.close();
    }
  });
});
