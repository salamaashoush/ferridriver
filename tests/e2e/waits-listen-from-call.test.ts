// Every wait listens from the moment it is called, as Playwright's do.
//
// The tests share a lock. A busy script blocks the runtime thread it runs
// on, and a task parked on that thread waits for it, so two of these at
// once could hold up each other's setup past the page's timer.
import { test, describe, expect } from '@ferridriver/test';
import type { ConsoleMessage, Dialog, Download, Page, Request, Response, WebSocket } from '@ferridriver/test';
import { fxWsUrl } from './helpers/server';

function busy(ms: number): void {
  const until = Date.now() + ms;
  while (Date.now() < until) {
    // The VM does not yield, so the event arrives first.
  }
}

/**
 * Has the page run `source` at a moment just ahead, makes the waits with
 * `arm` before that moment, and keeps the script busy until shortly after
 * it, so nothing polls the waits' promises before the page's event: a
 * wait that only listened once polled misses it and times out. A round in
 * which the page set its timer too late for the waits to come first is run
 * again.
 */
async function whileBusy<T>(page: Page, source: string, arm: () => T): Promise<T> {
  for (let round = 0; round < 5; round++) {
    const due = Date.now() + 250;
    await page.evaluate(`window.__later = setTimeout(() => { ${source} }, ${due} - Date.now()); null`);
    if (Date.now() >= due - 50) {
      await page.evaluate('clearTimeout(window.__later); null');
      continue;
    }
    const armed = arm();
    busy(due + 400 - Date.now());
    return armed;
  }
  throw new Error('the page never set its timer ahead of the waits');
}

describe('waits listen from the call', () => {
  describe.configure({ lock: 'busy-script' });

  for (const form of ['plain', 'predicate'] as const) {
    test(`page.waitForEvent (${form})`, async ({ page }) => {
      await page.goto('/fx/landed');
      const waiter = await whileBusy(page, "console.log('while busy');", () =>
        form === 'plain'
          ? page.waitForEvent('console', { timeout: 5000 })
          : page.waitForEvent('console', {
              predicate: (message) => (message as ConsoleMessage).text() === 'while busy',
              timeout: 5000,
            }),
      );
      expect(((await waiter) as ConsoleMessage).text()).toBe('while busy');
    });
  }

  test('page.waitForRequest, by URL and by predicate', async ({ page }) => {
    await page.goto('/fx/landed');
    const [byUrl, byPredicate] = await whileBusy(
      page,
      "fetch('/fx/echo?by=url'); fetch('/fx/echo?by=predicate');",
      () => [
        page.waitForRequest('**/fx/echo?by=url', { timeout: 5000 }),
        page.waitForRequest((request: Request) => request.url().endsWith('by=predicate'), { timeout: 5000 }),
      ],
    );
    expect((await byUrl).url()).toContain('by=url');
    expect((await byPredicate).url()).toContain('by=predicate');
  });

  test('page.waitForResponse, by URL and by predicate', async ({ page }) => {
    await page.goto('/fx/landed');
    const [byUrl, byPredicate] = await whileBusy(
      page,
      "fetch('/fx/echo?by=url'); fetch('/fx/echo?by=predicate');",
      () => [
        page.waitForResponse('**/fx/echo?by=url', { timeout: 5000 }),
        page.waitForResponse((response: Response) => response.url().endsWith('by=predicate'), { timeout: 5000 }),
      ],
    );
    expect((await byUrl).status()).toBe(200);
    expect((await byPredicate).status()).toBe(200);
  });

  test('page.waitForNavigation', async ({ page }) => {
    await page.goto('/fx/landed');
    const navigation = await whileBusy(page, "location.href = '/fx/landed?next';", () =>
      page.waitForNavigation({ timeout: 5000 }),
    );
    await navigation;
    expect(page.url()).toContain('?next');
  });

  test('frame.waitForNavigation', async ({ page }) => {
    await page.goto('/fx/landed');
    const navigation = await whileBusy(page, "location.href = '/fx/landed?frame';", () =>
      page.mainFrame().waitForNavigation({ timeout: 5000 }),
    );
    await navigation;
    expect(page.url()).toContain('?frame');
  });

  test("page.waitForEvent('dialog') claims the dialog", async ({ page }) => {
    await page.goto('/fx/landed');
    const dialog = await whileBusy(page, "alert('later');", () => page.waitForEvent('dialog', { timeout: 5000 }));
    const opened = (await dialog) as Dialog;
    expect(opened.message()).toBe('later');
    await opened.dismiss();
  });

  test("page.waitForEvent('download') claims the download", async ({ page }) => {
    await page.goto('/fx/iframe');
    await page.evaluate(
      "const a = document.createElement('a'); a.id = 'dl'; a.href = '/fx/download'; a.textContent = 'dl'; document.body.appendChild(a); null",
    );
    // A first download turns on the browser's download reporting, which
    // takes a round trip; what follows is about the claim alone.
    const first = page.waitForEvent('download', { timeout: 15000 });
    await page.click('#dl');
    await first;
    const download = await whileBusy(page, "document.getElementById('dl').click();", () =>
      page.waitForEvent('download', { timeout: 15000 }),
    );
    expect(((await download) as Download).url()).toMatch(/\/fx\/download$/);
  });

  test('context.waitForEvent, plain and with a predicate', async ({ page, context }) => {
    await page.goto('/fx/landed');
    const [plain, matching] = await whileBusy(page, "window.open('/fx/landed?popup');", () => [
      context.waitForEvent('page', { timeout: 5000 }),
      context.waitForEvent('page', { predicate: () => true, timeout: 5000 }),
    ]);
    const [first, second] = [(await plain) as Page, (await matching) as Page];
    expect(first).toBe(second);
    await first.close();
  });

  test('webSocket.waitForEvent', async ({ page, baseURL, browserName }) => {
    // WebDriver BiDi reports no WebSocket traffic; Playwright's BiDi
    // backend has no WebSocket events either.
    test.skip(browserName === 'firefox', 'WebDriver BiDi has no WebSocket events');
    await page.goto('/fx/landed');
    const opened = page.waitForEvent('websocket');
    await page.evaluate((url: string) => {
      const ws = new WebSocket(url);
      (window as any).__opened = new Promise(resolve => {
        ws.onopen = resolve;
      });
      (window as any).__ws = ws;
    }, fxWsUrl(baseURL));
    await page.evaluate(() => (window as any).__opened);
    const ws = (await opened) as WebSocket;
    const frame = await whileBusy(page, "window.__ws.send('later');", () =>
      ws.waitForEvent('framereceived', { timeout: 5000 }),
    );
    expect((await frame).payload).toBe('later');
  });
});
