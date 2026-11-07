// `waitForURL` takes a `(url: URL) => boolean | Promise<boolean>` as
// Playwright's does, on the page and on a frame, and the predicate's
// own error rejects the wait.

import { test, describe, expect } from '@ferridriver/test';
import type { Page } from '@ferridriver/test';

async function pushSoon(page: Page, path: string): Promise<void> {
  await page.evaluate((target: string) => {
    setTimeout(() => history.pushState({}, '', target), 100);
  }, path);
}

describe('waitForURL with a predicate', () => {
  test('is called with a URL and resolves when it returns true', async ({ page, baseURL }) => {
    await page.goto(`${baseURL}/fx/landed`);
    await pushSoon(page, '/target?x=1');
    const seen: string[] = [];
    await page.waitForURL(
      url => {
        seen.push(url instanceof URL ? 'URL' : typeof url);
        return url.pathname === '/target' && url.searchParams.get('x') === '1';
      },
      { timeout: 5000 },
    );
    expect(page.url()).toBe(`${baseURL}/target?x=1`);
    expect(seen.every(kind => kind === 'URL')).toBe(true);
  });

  test('awaits a predicate that answers with a promise', async ({ page, baseURL }) => {
    await page.goto(`${baseURL}/fx/landed`);
    await pushSoon(page, '/later');
    await page.waitForURL(
      async url => {
        await Promise.resolve();
        return url.pathname === '/later';
      },
      { timeout: 5000 },
    );
    expect(page.url()).toBe(`${baseURL}/later`);
  });

  test('a frame waits on a predicate too', async ({ page, baseURL }) => {
    await page.goto(`${baseURL}/fx/landed`);
    await pushSoon(page, '/framed');
    await page.mainFrame().waitForURL(url => url.pathname === '/framed', { timeout: 5000 });
    expect(page.url()).toBe(`${baseURL}/framed`);
  });

  test('a predicate that never matches times out at the timeout asked for', async ({ page, baseURL }) => {
    await page.goto(`${baseURL}/fx/landed`);
    const started = Date.now();
    let message = '';
    try {
      await page.waitForURL(() => false, { timeout: 500 });
    } catch (e) {
      message = String((e as Error).message ?? e);
    }
    expect(message).toContain('500');
    expect(Date.now() - started < 3000).toBe(true);
  });

  test("the predicate's error rejects the wait", async ({ page, baseURL }) => {
    await page.goto(`${baseURL}/fx/landed`);
    let message = '';
    try {
      await page.waitForURL(
        () => {
          throw new Error('sashoush predicate failed');
        },
        { timeout: 5000 },
      );
    } catch (e) {
      message = String((e as Error).message ?? e);
    }
    expect(message).toContain('sashoush predicate failed');
  });
});
