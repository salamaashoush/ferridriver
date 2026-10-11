// storageState() over every origin the context visited, not only the
// ones a page is still on. Playwright keeps the visited origins
// (`BrowserContext._origins`) and reads the rest from a page of its own
// (`server/browserContext.ts::storageState` / `visitOrigins`); restoring
// goes through the same kind of page, so no page the caller holds moves.
import { test, describe, expect } from '@ferridriver/test';
import type { BrowserContext, Page } from '@ferridriver/test';

const ORIGIN = 'http://127.0.0.1:47831';
const OTHER = 'http://localhost:47831';

const seed = (page: Page, key: string, value: string) =>
  page.evaluate(([key, value]: string[]) => localStorage.setItem(key, value), [key, value]);

const local = async (context: BrowserContext) =>
  Object.fromEntries((await context.storageState()).origins.map(o => [o.origin, o.localStorage]));

describe('storage state of visited origins', () => {
  test('keeps an origin whose page has closed', async ({ browser }) => {
    const context = await browser.newContext();
    try {
      const page = await context.newPage();
      await page.goto(`${ORIGIN}/fx/landed`);
      await seed(page, 'token', 'kept');
      await page.close();
      expect(await context.pages()).toEqual([]);
      expect(await local(context)).toEqual({ [ORIGIN]: [{ name: 'token', value: 'kept' }] });
    } finally {
      await context.close();
    }
  });

  test('keeps an origin the page navigated away from', async ({ browser }) => {
    const context = await browser.newContext();
    try {
      const page = await context.newPage();
      await page.goto(`${ORIGIN}/fx/landed`);
      await seed(page, 'first', 'one');
      await page.goto(`${OTHER}/fx/landed`);
      await seed(page, 'second', 'two');
      expect(await local(context)).toEqual({
        [ORIGIN]: [{ name: 'first', value: 'one' }],
        [OTHER]: [{ name: 'second', value: 'two' }],
      });
      // The page the caller holds stays where it was.
      expect(page.url()).toBe(`${OTHER}/fx/landed`);
    } finally {
      await context.close();
    }
  });

  test('the page it reads through is not one of the context', async ({ browser }) => {
    const context = await browser.newContext();
    try {
      const opened: Page[] = [];
      context.on('page', page => opened.push(page as Page));
      const requests: string[] = [];
      context.on('request', request => requests.push((request as { url(): string }).url()));
      const page = await context.newPage();
      await page.goto(`${ORIGIN}/fx/landed`);
      await seed(page, 'token', 'kept');
      await page.goto(`${OTHER}/fx/landed`);
      const seen = requests.length;
      expect(Object.keys(await local(context))).toEqual([ORIGIN]);
      expect(opened.length).toBe(1);
      expect((await context.pages()).length).toBe(1);
      expect(requests.length).toBe(seen);
    } finally {
      await context.close();
    }
  });

  test('a context created from a state leaves its first page blank', async ({ browser }) => {
    const source = await browser.newContext();
    let state;
    try {
      const page = await source.newPage();
      await page.goto(`${ORIGIN}/fx/landed`);
      await seed(page, 'token', 'kept');
      state = await source.storageState();
    } finally {
      await source.close();
    }

    const context = await browser.newContext({ storageState: state });
    try {
      const page = await context.newPage();
      expect(await page.evaluate(() => location.href)).toBe('about:blank');
      // Restored without a visit: the state names the origin.
      expect(await local(context)).toEqual({ [ORIGIN]: [{ name: 'token', value: 'kept' }] });
      await page.goto(`${ORIGIN}/fx/landed`);
      expect(await page.evaluate(() => localStorage.getItem('token'))).toBe('kept');
    } finally {
      await context.close();
    }
  });

  test('setStorageState empties the origins the state does not name', async ({ browser }) => {
    const context = await browser.newContext();
    try {
      const page = await context.newPage();
      await page.goto(`${ORIGIN}/fx/landed`);
      await seed(page, 'stale', 'gone');
      await page.goto(`${OTHER}/fx/landed`);
      await seed(page, 'stale', 'gone too');
      await page.goto('about:blank');

      await context.setStorageState({
        cookies: [],
        origins: [{ origin: OTHER, localStorage: [{ name: 'fresh', value: 'new' }] }],
      });
      expect(page.url()).toBe('about:blank');
      expect((await context.pages()).length).toBe(1);
      expect(await local(context)).toEqual({ [OTHER]: [{ name: 'fresh', value: 'new' }] });

      await page.goto(`${ORIGIN}/fx/landed`);
      expect(await page.evaluate(() => localStorage.length)).toBe(0);
      await page.goto(`${OTHER}/fx/landed`);
      expect(await page.evaluate(() => ({ fresh: localStorage.getItem('fresh'), stale: localStorage.getItem('stale') }))).toEqual({
        fresh: 'new',
        stale: null,
      });
    } finally {
      await context.close();
    }
  });
});
