// Ported from Playwright's tests/page/page-event-popup.spec.ts: a page
// emits `popup` with the page it opened, whether by window.open or a
// target=_blank link, with or without an opener.
import { test, expect } from '@ferridriver/test';
import type { Page } from '@ferridriver/test';

const ORIGIN = 'http://127.0.0.1:47831';
const EMPTY_PAGE = `${ORIGIN}/page-popup/empty.html`;

async function serve(page: Page) {
  await page.context().route('**/page-popup/**', route =>
    route.fulfill({ body: '<!doctype html><body>popup</body>', contentType: 'text/html' }));
}

const hasOpener = (page: Page) => page.evaluate(() => !!window.opener);

test.describe('page popup event', () => {
  test('works', async ({ page }) => {
    const [popup] = await Promise.all([
      page.waitForEvent('popup') as Promise<Page>,
      page.evaluate(() => { (window as any).__popup = window.open('about:blank'); }),
    ]);
    expect(await hasOpener(page)).toBe(false);
    expect(await hasOpener(popup)).toBe(true);
    expect(await popup.opener()).toBe(page);
  });

  test('works with window features', async ({ page }) => {
    await serve(page);
    await page.goto(EMPTY_PAGE);
    const [popup] = await Promise.all([
      page.waitForEvent('popup') as Promise<Page>,
      page.evaluate(() => { (window as any).__popup = window.open(window.location.href, 'Title', 'toolbar=no,location=no,width=780,height=200,top=0,left=0'); }),
    ]);
    expect(await hasOpener(popup)).toBe(true);
  });

  test('emits for immediately closed popups', async ({ page }) => {
    const [popup] = await Promise.all([
      page.waitForEvent('popup'),
      page.evaluate(() => { window.open('about:blank')!.close(); }),
    ]);
    expect(popup).toBeTruthy();
  });

  test('works with noopener and about:blank', async ({ page }) => {
    const [popup] = await Promise.all([
      page.waitForEvent('popup') as Promise<Page>,
      page.evaluate(() => { (window as any).__popup = window.open('about:blank', undefined, 'noopener'); }),
    ]);
    expect(await hasOpener(page)).toBe(false);
    expect(await hasOpener(popup)).toBe(false);
  });

  test('works with noopener and url', async ({ page }) => {
    await serve(page);
    await page.goto(EMPTY_PAGE);
    const [popup] = await Promise.all([
      page.waitForEvent('popup') as Promise<Page>,
      page.evaluate((url: string) => { (window as any).__popup = window.open(url, undefined, 'noopener'); }, EMPTY_PAGE),
    ]);
    expect(await hasOpener(popup)).toBe(false);
  });

  test('works with clicking target=_blank', async ({ page }) => {
    await serve(page);
    await page.goto(EMPTY_PAGE);
    await page.setContent('<a target=_blank rel="opener" href="/page-popup/one.html">yo</a>');
    const [popup] = await Promise.all([
      page.waitForEvent('popup') as Promise<Page>,
      page.click('a'),
    ]);
    expect(await hasOpener(page)).toBe(false);
    expect(await hasOpener(popup)).toBe(true);
    expect(popup.mainFrame().page()).toBe(popup);
  });

  test('works with clicking target=_blank and rel=noopener', async ({ page }) => {
    await serve(page);
    await page.goto(EMPTY_PAGE);
    await page.setContent('<a target=_blank rel=noopener href="/page-popup/one.html">yo</a>');
    const [popup] = await Promise.all([
      page.waitForEvent('popup') as Promise<Page>,
      page.click('a'),
    ]);
    expect(await hasOpener(popup)).toBe(false);
  });

  test('reports a popup opened from an iframe', async ({ page }) => {
    await serve(page);
    await page.goto(EMPTY_PAGE);
    await page.setContent(`<iframe src="${EMPTY_PAGE}"></iframe>`);
    await expect.poll(() => page.frames().length).toBe(2);
    const frame = page.frames()[1];
    await frame.waitForSelector('body');
    const [popup] = await Promise.all([
      page.waitForEvent('popup'),
      frame.evaluate(() => { window.open(''); }),
    ]);
    expect(popup).toBeTruthy();
  });

  test('reports popups of a page in the default context', async ({ browser }) => {
    const page = await browser.newPage();
    try {
      const [popup] = await Promise.all([
        page.waitForEvent('popup') as Promise<Page>,
        page.evaluate("window.open('about:blank'); null"),
      ]);
      expect(await popup.opener()).toBe(page);
      await popup.close();
    } finally {
      await page.close();
    }
  });

  test('page.on delivers the popup', async ({ page }) => {
    const seen: Page[] = [];
    page.on('popup', (popup: unknown) => { seen.push(popup as Page); });
    await page.evaluate(() => { window.open('about:blank'); });
    await expect.poll(() => seen.length).toBe(1);
    expect(await seen[0].opener()).toBe(page);
  });
});
