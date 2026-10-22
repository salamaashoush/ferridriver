// Traffic through proxies that want credentials, on every backend.
//
// The fixture server runs a forwarding HTTP proxy and a SOCKS5 proxy, both
// wanting fixture-user / fixture-pass (crates/ferridriver-fixtures,
// forward_proxy.rs). Each resolves `*.test` to loopback and nothing else
// does, so a page or request that reaches `http://<key>.proxied.test:<port>/`
// can only have gone through one of them, with the credentials accepted.
// The key in the host name keeps each test's log entries its own.

import { test, describe, expect } from '@ferridriver/test';
import type { APIRequestContext } from '@ferridriver/test';
import { fxUrl } from './helpers/server';

const USER = 'fixture-user';
const PASSWORD = 'fixture-pass';
// Fixed in ferridriver.toml's webServer command so `test.use` can name them.
const HTTP_PROXY = 'http://127.0.0.1:47832';
const SOCKS_PROXY = 'socks5://127.0.0.1:47833';

function hidden(baseURL: string | undefined, key: string, path: string): string {
  const port = new URL(baseURL ?? 'http://127.0.0.1:47831').port;
  return `http://${key}.proxied.test:${port}${path}`;
}

async function forwardLog(request: APIRequestContext, baseURL: string | undefined, key: string): Promise<string[]> {
  const resp = await request.get(`${fxUrl(baseURL, '/forward-log')}?key=${key}`);
  return ((await resp.json()) as { lines: string[] }).lines;
}

async function dropLog(request: APIRequestContext, baseURL: string | undefined, key: string): Promise<void> {
  await request.delete(`${fxUrl(baseURL, '/forward-log')}?key=${key}`);
}

describe('proxies wanting credentials', () => {
  test('an http proxy carries the page and the context request', async ({ browser, baseURL, request }) => {
    const key = crypto.randomUUID();
    const ctx = await browser.newContext({ proxy: { server: HTTP_PROXY, username: USER, password: PASSWORD } });
    try {
      const page = await ctx.newPage();
      const response = await page.goto(hidden(baseURL, key, '/fx/landed'));
      expect(response?.status()).toBe(200);
      expect(await page.textContent('body')).toContain('landed');

      const api = await ctx.request.get(hidden(baseURL, key, '/fx/landed?via=request'));
      expect(api.status()).toBe(200);
      expect(await api.text()).toContain('landed');

      const lines = await forwardLog(request, baseURL, key);
      expect(lines.some((line) => line.startsWith('http GET') && line.endsWith('/fx/landed'))).toBe(true);
      expect(lines.some((line) => line.includes('via=request'))).toBe(true);
    } finally {
      await ctx.close();
      await dropLog(request, baseURL, key);
    }
  });

  test('a socks5 proxy carries the page and the context request, resolving the name itself', async ({ browser, baseURL, request }) => {
    const key = crypto.randomUUID();
    const ctx = await browser.newContext({ proxy: { server: SOCKS_PROXY, username: USER, password: PASSWORD } });
    try {
      const page = await ctx.newPage();
      const response = await page.goto(hidden(baseURL, key, '/fx/landed'));
      expect(response?.status()).toBe(200);
      expect(await page.textContent('body')).toContain('landed');

      const api = await page.request.get(hidden(baseURL, key, '/fx/landed'));
      expect(await api.text()).toContain('landed');

      // The proxy was handed the name, not an address this machine resolved.
      const lines = await forwardLog(request, baseURL, key);
      expect(lines.filter((line) => line.startsWith(`socks ${key}.proxied.test:`)).length).toBeGreaterThanOrEqual(2);
    } finally {
      await ctx.close();
      await dropLog(request, baseURL, key);
    }
  });

  test('route.fetch leaves through the context proxy', async ({ browser, baseURL, request }) => {
    const key = crypto.randomUUID();
    const target = hidden(baseURL, key, '/fx/landed');
    const ctx = await browser.newContext({ proxy: { server: HTTP_PROXY, username: USER, password: PASSWORD } });
    try {
      await ctx.route(target, async (route) => {
        const fetched = await route.fetch();
        await route.fulfill({ response: fetched, headers: { ...fetched.headers(), 'x-fetched': 'yes' } });
      });
      const page = await ctx.newPage();
      const response = await page.goto(target);
      expect(response?.headers()['x-fetched']).toBe('yes');
      expect(await page.textContent('body')).toContain('landed');
      // The page's own request was held by the route, so the one the proxy saw
      // is the route's fetch. (A browser may also ask the host for a favicon.)
      const fetched = (await forwardLog(request, baseURL, key)).filter((line) => line.endsWith('/fx/landed'));
      expect(fetched.length).toBe(1);
    } finally {
      await ctx.close();
      await dropLog(request, baseURL, key);
    }
  });

  test('a route continued to another URL still leaves through the context proxy', async ({ browser, baseURL, request }) => {
    const key = crypto.randomUUID();
    const rewritten = crypto.randomUUID();
    const ctx = await browser.newContext({ proxy: { server: SOCKS_PROXY, username: USER, password: PASSWORD } });
    try {
      // BiDi cannot rewrite a URL in the browser and re-issues the request
      // itself; only a client on the context's proxy can resolve the name.
      await ctx.route(hidden(baseURL, key, '/fx/inner'), (route) =>
        route.continue({ url: hidden(baseURL, rewritten, '/fx/landed') }));
      const page = await ctx.newPage();
      await page.goto(hidden(baseURL, key, '/fx/inner'));
      expect(await page.textContent('body')).toContain('landed');
      expect((await forwardLog(request, baseURL, rewritten)).length).toBeGreaterThanOrEqual(1);
    } finally {
      await ctx.close();
      await dropLog(request, baseURL, key);
      await dropLog(request, baseURL, rewritten);
    }
  });

  test('request.newContext takes a proxy, http or socks5, with credentials', async ({ baseURL, request }) => {
    for (const server of [HTTP_PROXY, SOCKS_PROXY]) {
      const key = crypto.randomUUID();
      const api = await request.newContext({ proxy: { server, username: USER, password: PASSWORD } });
      try {
        const resp = await api.get(hidden(baseURL, key, '/fx/landed'));
        expect([server, resp.status()]).toEqual([server, 200]);
        expect((await forwardLog(request, baseURL, key)).length).toBe(1);
      } finally {
        await api.dispose();
        await dropLog(request, baseURL, key);
      }
    }
  });

  test('refused credentials are not retried forever', async ({ browser, baseURL, request }) => {
    const key = crypto.randomUUID();
    // Firefox keeps a proxy's credentials browser-wide, keyed by the proxy's
    // address, so an address another test logged in to would be answered
    // with those. The same proxy under another name has no such entry.
    const server = HTTP_PROXY.replace('127.0.0.1', 'localhost');
    const ctx = await browser.newContext({ proxy: { server, username: USER, password: 'wrong' } });
    try {
      const page = await ctx.newPage();
      const outcome = await page
        .goto(hidden(baseURL, key, '/fx/landed'), { timeout: 15_000 })
        .then((response) => String(response?.status()), (error: Error) => error.message);
      expect(outcome).not.toBe('200');
      expect(await forwardLog(request, baseURL, key)).toEqual([]);
    } finally {
      await ctx.close();
    }
  });
});

describe('test.use({ proxy })', () => {
  test.use({ proxy: { server: SOCKS_PROXY, username: USER, password: PASSWORD } });

  test('the test context sends through the proxy', async ({ page, baseURL, request }) => {
    const key = crypto.randomUUID();
    try {
      await page.goto(hidden(baseURL, key, '/fx/landed'));
      expect(await page.textContent('body')).toContain('landed');
      expect((await forwardLog(request, baseURL, key)).length).toBeGreaterThanOrEqual(1);
    } finally {
      await dropLog(request, baseURL, key);
    }
  });
});
