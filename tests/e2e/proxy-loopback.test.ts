// Loopback through a context proxy, on every backend.
//
// With a proxy set, `localhost` means whatever the proxy reaches by that
// name, so a page on a loopback address leaves through the proxy like any
// other host unless the bypass list names it. The fixture server's
// forwarding proxy (crates/ferridriver-fixtures, forward_proxy.rs) logs each
// request line, and the key in the query keeps each test's entries its own.

import { test, describe, expect } from '@ferridriver/test';
import type { APIRequestContext, Browser } from '@ferridriver/test';
import { release } from 'node:os';
import { fxUrl } from './helpers/server';

const HTTP_PROXY = 'http://127.0.0.1:47832';

// WebKit on macOS 26 and later (Darwin 25+) sends loopback straight to the
// host whatever the proxy says. Playwright's WebKit build overrides the
// connection host so loopback goes through the proxy, and there it works
// only now and then; Playwright itself fails the same way on those builds.
const MAC_WEBKIT_SENDS_LOOPBACK_DIRECT = 'macOS 26+ WebKit sends loopback direct, past the proxy';
function macWebKitSendsLoopbackDirect(browserName: string): boolean {
  return browserName === 'webkit' && process.platform === 'darwin' && Number(release().split('.')[0]) >= 25;
}
const CREDENTIALS = { username: 'fixture-user', password: 'fixture-pass' };

function loopback(baseURL: string | undefined, host: string, key: string): string {
  const port = new URL(baseURL ?? 'http://127.0.0.1:47831').port;
  return `http://${host}:${port}/fx/landed?loopback=${key}`;
}

async function forwardLog(request: APIRequestContext, baseURL: string | undefined, key: string): Promise<string[]> {
  const resp = await request.get(`${fxUrl(baseURL, '/forward-log')}?key=${key}`);
  return ((await resp.json()) as { lines: string[] }).lines;
}

async function dropLog(request: APIRequestContext, baseURL: string | undefined, key: string): Promise<void> {
  await request.delete(`${fxUrl(baseURL, '/forward-log')}?key=${key}`);
}

async function visit(browser: Browser, bypass: string | undefined, url: string): Promise<void> {
  const ctx = await browser.newContext({ proxy: { server: HTTP_PROXY, bypass, ...CREDENTIALS } });
  try {
    const page = await ctx.newPage();
    const response = await page.goto(url);
    expect(response?.status()).toBe(200);
    expect(await page.textContent('body')).toContain('landed');
  } finally {
    await ctx.close();
  }
}

describe('loopback and a context proxy', () => {
  for (const host of ['127.0.0.1', 'localhost']) {
    test(`${host} goes through the proxy`, async ({ browser, browserName, baseURL, request }) => {
      test.skip(macWebKitSendsLoopbackDirect(browserName), MAC_WEBKIT_SENDS_LOOPBACK_DIRECT);
      const key = crypto.randomUUID();
      try {
        await visit(browser, undefined, loopback(baseURL, host, key));
        expect(await forwardLog(request, baseURL, key)).toEqual([`http GET ${loopback(baseURL, host, key)}`]);
      } finally {
        await dropLog(request, baseURL, key);
      }
    });

    test(`${host} goes through the proxy when the bypass names other hosts`, async ({ browser, browserName, baseURL, request }) => {
      test.skip(browserName === 'webkit' && process.platform === 'darwin', 'macOS WebKit does not proxy loopback when bypass rules are set');
      const key = crypto.randomUUID();
      try {
        await visit(browser, 'elsewhere.example.com', loopback(baseURL, host, key));
        expect(await forwardLog(request, baseURL, key)).toEqual([`http GET ${loopback(baseURL, host, key)}`]);
      } finally {
        await dropLog(request, baseURL, key);
      }
    });

    test(`${host} is reached directly when the bypass names it`, async ({ browser, baseURL, request }) => {
      const key = crypto.randomUUID();
      try {
        await visit(browser, `elsewhere.example.com, ${host}`, loopback(baseURL, host, key));
        expect(await forwardLog(request, baseURL, key)).toEqual([]);
      } finally {
        await dropLog(request, baseURL, key);
      }
    });
  }
});

describe('loopback and test.use({ proxy })', () => {
  test.use({ proxy: { server: HTTP_PROXY, ...CREDENTIALS } });

  test('the test context sends loopback through the proxy', async ({ page, browserName, baseURL, request }) => {
    test.skip(macWebKitSendsLoopbackDirect(browserName), MAC_WEBKIT_SENDS_LOOPBACK_DIRECT);
    const key = crypto.randomUUID();
    try {
      await page.goto(loopback(baseURL, '127.0.0.1', key));
      expect(await page.textContent('body')).toContain('landed');
      // The request fixture takes the proxy too, so its own call to the log is
      // in the log.
      const lines = await forwardLog(request, baseURL, key);
      expect(lines.filter((line) => line.includes('/fx/landed'))).toEqual([
        `http GET ${loopback(baseURL, '127.0.0.1', key)}`,
      ]);
    } finally {
      await dropLog(request, baseURL, key);
    }
  });
});
