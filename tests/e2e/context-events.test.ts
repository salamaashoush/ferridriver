// A context hears the network and console traffic of every page it
// owns, and its own close. Ported from Playwright's
// browsercontext-events.spec.ts and browsercontext-network-event.spec.ts.
// Each test runs its own browser: opening and closing contexts on the
// worker's shared browser perturbs the rest of the suite.
import { test, expect } from '@ferridriver/test';
import type { Browser, ConsoleMessage, Request, Response } from '@ferridriver/test';

const ORIGIN = 'http://127.0.0.1:47831';

function launch(browserName: string): Promise<Browser> {
  const factory = browserName === 'firefox' ? firefox() : browserName === 'webkit' ? webkit() : chromium();
  return factory.launch({ headless: true });
}

async function withBrowser(browserName: string, body: (browser: Browser) => Promise<void>) {
  const browser = await launch(browserName);
  try {
    await body(browser);
  } finally {
    await browser.close();
  }
}

test.describe('context events', () => {
  test('request, response and requestfinished from every page', async ({ browserName }) => withBrowser(browserName, async browser => {
    const context = await browser.newContext();
    const requests: string[] = [];
    const responses: string[] = [];
    const finished: string[] = [];
    context.on('request', r => requests.push((r as Request).url()));
    context.on('response', r => responses.push((r as Response).url()));
    context.on('requestfinished', r => finished.push((r as Request).url()));

    const first = await context.newPage();
    const second = await context.newPage();
    const page = context.waitForEvent('request', r => (r as Request).url().endsWith('/empty.html?two'));
    await first.goto(`${ORIGIN}/empty.html?one`);
    await second.goto(`${ORIGIN}/empty.html?two`);
    const request = await page as Request;
    expect(request.frame()!.page()).toBe(second);

    await expect.poll(() => finished.filter(u => u.includes('/empty.html?')).length).toBe(2);
    expect(requests.filter(u => u.includes('/empty.html?')).sort())
      .toEqual([`${ORIGIN}/empty.html?one`, `${ORIGIN}/empty.html?two`]);
    expect(responses.filter(u => u.includes('/empty.html?')).length).toBe(2);
  }));

  test('requestfailed carries the failure', async ({ browserName }) => withBrowser(browserName, async browser => {
    const context = await browser.newContext();
    await context.route('**/never', route => route.abort());
    const page = await context.newPage();
    await page.goto(`${ORIGIN}/empty.html`);
    const failed = context.waitForEvent('requestfailed', r => (r as Request).url().endsWith('/never'));
    await page.evaluate(() => fetch('/never').catch(() => null));
    const request = await failed as Request;
    expect(request.failure()).not.toBeNull();
  }));

  test('console from any page, with the page it came from', async ({ browserName }) => withBrowser(browserName, async browser => {
    const context = await browser.newContext();
    const page = await context.newPage();
    const message = context.waitForEvent('console', m => (m as ConsoleMessage).text() === 'from the page');
    await page.evaluate(() => console.log('from the page'));
    const received = await message as ConsoleMessage;
    expect(received.type()).toBe('log');
    expect(received.page()).toBe(page);
  }));

  test('close fires once, for close() and for its browser', async ({ browserName }) => {
    const browser = await launch(browserName);
    const context = await browser.newContext();
    let closes = 0;
    context.on('close', () => { closes++; });
    const closed = context.waitForEvent('close');
    await context.close();
    await closed;
    await context.close();
    expect(closes).toBe(1);

    const owned = await browser.newContext();
    const ownedClosed = owned.waitForEvent('close');
    await browser.close();
    await ownedClosed;
  });
});
