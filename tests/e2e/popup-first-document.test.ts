// Ported from Playwright's tests/library/popup.spec.ts: a popup's first
// document already runs under its context's configuration, whether or
// not the popup shares a browsing instance with its opener.
import { test, expect } from '@ferridriver/test';
import type { Page } from '@ferridriver/test';

const ORIGIN = 'http://127.0.0.1:47831';
const POPUP = `<!doctype html><script>
  window.sawInjected = window.injected;
  window.sawBinding = typeof window.report;
  window.sawUserAgent = navigator.userAgent;
</script><body>popup</body>`;

const openers: [string, (page: Page, url: string) => Promise<unknown>][] = [
  ['window.open', (page, url) => page.evaluate((u: string) => { window.open(u); }, url)],
  ['window.open with noopener', (page, url) => page.evaluate((u: string) => { window.open(u, '_blank', 'noopener'); }, url)],
  ['a target=_blank rel=noopener link', async (page, url) => {
    await page.setContent(`<a target=_blank rel=noopener href="${url}">open</a>`);
    await page.click('a');
  }],
];

test.describe('popup first document', () => {
  for (const [how, open] of openers) {
    test(`sees the context configuration (${how})`, async ({ browser }) => {
      const context = await browser.newContext({ userAgent: 'hey' });
      try {
        const requested: (string | undefined)[] = [];
        await context.addInitScript(() => { (window as any).injected = 123; });
        await context.exposeFunction('report', () => 1);
        await context.route('**/popup-first/**', route => {
          requested.push(route.request().headers()['user-agent']);
          return route.fulfill({ body: POPUP, contentType: 'text/html' });
        });
        const page = await context.newPage();
        await page.goto(`${ORIGIN}/popup-first/opener.html`);
        const [popup] = await Promise.all([
          context.waitForEvent('page') as Promise<Page>,
          open(page, `${ORIGIN}/popup-first/popup.html`),
        ]);
        await popup.waitForLoadState();
        expect(await popup.evaluate(() => document.body.textContent)).toBe('popup');
        expect(await popup.evaluate(() => (window as any).sawInjected)).toBe(123);
        expect(await popup.evaluate(() => (window as any).sawBinding)).toBe('function');
        expect(await popup.evaluate(() => (window as any).sawUserAgent)).toBe('hey');
        expect(requested).toEqual(['hey', 'hey']);
      } finally {
        await context.close();
      }
    });
  }
});
