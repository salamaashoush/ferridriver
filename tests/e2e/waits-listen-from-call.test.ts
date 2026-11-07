// Every wait listens from the moment it is called, as Playwright's do. In
// each test the page fires the event on a timer while the script keeps
// the VM busy, so nothing polls the wait's promise before the event:
// a wait that only listened once polled would miss it and time out.
//
// The tests share a lock. A busy script blocks the runtime thread it runs
// on, and a task woken onto that thread waits for it, so two of these at
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
 * Runs `source` in the page 400 ms from now: after the wait the test
 * creates next, and well inside the two seconds `busy` holds the VM.
 */
async function later(page: Page, source: string): Promise<void> {
  await page.evaluate(`setTimeout(() => { ${source} }, 400); null`);
}

describe('waits listen from the call', () => {
  describe.configure({ lock: 'busy-script' });

  for (const form of ['plain', 'predicate'] as const) {
    test(`page.waitForEvent (${form})`, async ({ page }) => {
      await page.goto('/fx/landed');
      await later(page, "console.log('while busy');");
      const waiter =
        form === 'plain'
          ? page.waitForEvent('console', { timeout: 5000 })
          : page.waitForEvent('console', {
              predicate: (message) => (message as ConsoleMessage).text() === 'while busy',
              timeout: 5000,
            });
      busy(2000);
      expect(((await waiter) as ConsoleMessage).text()).toBe('while busy');
    });
  }

  test('page.waitForRequest, by URL and by predicate', async ({ page }) => {
    await page.goto('/fx/landed');
    await later(page, "fetch('/fx/echo?by=url'); fetch('/fx/echo?by=predicate');");
    const byUrl = page.waitForRequest('**/fx/echo?by=url', { timeout: 5000 });
    const byPredicate = page.waitForRequest((request: Request) => request.url().endsWith('by=predicate'), {
      timeout: 5000,
    });
    busy(2000);
    expect((await byUrl).url()).toContain('by=url');
    expect((await byPredicate).url()).toContain('by=predicate');
  });

  test('page.waitForResponse, by URL and by predicate', async ({ page }) => {
    await page.goto('/fx/landed');
    await later(page, "fetch('/fx/echo?by=url'); fetch('/fx/echo?by=predicate');");
    const byUrl = page.waitForResponse('**/fx/echo?by=url', { timeout: 5000 });
    const byPredicate = page.waitForResponse((response: Response) => response.url().endsWith('by=predicate'), {
      timeout: 5000,
    });
    busy(2000);
    expect((await byUrl).status()).toBe(200);
    expect((await byPredicate).status()).toBe(200);
  });

  test('page.waitForNavigation', async ({ page }) => {
    await page.goto('/fx/landed');
    await later(page, "location.href = '/fx/landed?next';");
    const navigation = page.waitForNavigation({ timeout: 5000 });
    busy(2000);
    await navigation;
    expect(page.url()).toContain('?next');
  });

  test('frame.waitForNavigation', async ({ page }) => {
    await page.goto('/fx/landed');
    await later(page, "location.href = '/fx/landed?frame';");
    const navigation = page.mainFrame().waitForNavigation({ timeout: 5000 });
    busy(2000);
    await navigation;
    expect(page.url()).toContain('?frame');
  });

  test("page.waitForEvent('dialog') claims the dialog", async ({ page }) => {
    await page.goto('/fx/landed');
    await later(page, "alert('later');");
    const dialog = page.waitForEvent('dialog', { timeout: 5000 });
    busy(2000);
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
    await later(page, "document.getElementById('dl').click();");
    const download = page.waitForEvent('download', { timeout: 15000 });
    busy(2000);
    expect(((await download) as Download).url()).toMatch(/\/fx\/download$/);
  });

  test('context.waitForEvent, plain and with a predicate', async ({ page, context }) => {
    await page.goto('/fx/landed');
    await later(page, "window.open('/fx/landed?popup');");
    const plain = context.waitForEvent('page', { timeout: 5000 });
    const matching = context.waitForEvent('page', { predicate: () => true, timeout: 5000 });
    busy(2000);
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
    await later(page, "window.__ws.send('later');");
    const frame = ws.waitForEvent('framereceived', { timeout: 5000 });
    busy(2000);
    expect((await frame).payload).toBe('later');
  });
});
