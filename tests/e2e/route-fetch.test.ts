// Playwright's route.fetch(): a handler sends the intercepted request on
// to the network, changed or not, and fulfils with what came back.
import { test, expect } from '@ferridriver/test';

const ORIGIN = 'http://127.0.0.1:47831';

test.describe('route.fetch', () => {
  test('fetches the intercepted request and fulfils with a changed body', async ({ page }) => {
    await page.route('**/fx/landed', async route => {
      const response = await route.fetch();
      expect(response.status()).toBe(200);
      const body = await response.text();
      await route.fulfill({ response, body: `${body.toUpperCase()}!`, headers: { ...response.headers(), 'x-patched': '1' } });
    });
    const response = await page.goto(`${ORIGIN}/fx/landed`);
    expect(await page.evaluate(() => document.body.innerText)).toBe('LANDED!');
    expect(response!.headers()['x-patched']).toBe('1');
  });

  test('overrides url, method, headers and body', async ({ page }) => {
    await page.goto(`${ORIGIN}/empty.html`);
    let echoed: any;
    await page.route('**/never-served', async route => {
      const response = await route.fetch({
        url: `${ORIGIN}/_api/echo?via=route`,
        method: 'POST',
        headers: { ...route.request().headers(), 'x-route': 'fetched' },
        postData: 'sashoush',
      });
      echoed = await response.json();
      await route.fulfill({ response });
    });
    const text = String(await page.evaluate(() => fetch('/never-served').then(r => r.text())));
    expect(echoed.url).toBe('/_api/echo?via=route');
    expect(echoed.method).toBe('POST');
    expect(echoed.headers['x-route']).toBe('fetched');
    expect(echoed.data).toBe('sashoush');
    expect(JSON.parse(text).method).toBe('POST');
  });

  test('a context route fetches with the context cookies', async ({ page, context }) => {
    await context.addCookies([{ name: 'sid', value: 'sashoush', url: ORIGIN }]);
    let cookie = '';
    await context.route('**/_api/cookie-check', async route => {
      const response = await route.fetch();
      cookie = ((await response.json()) as { headers: Record<string, string> }).headers.cookie ?? '';
      await route.fulfill({ response });
    });
    await page.goto(`${ORIGIN}/empty.html`);
    await page.evaluate(() => fetch('/_api/cookie-check'));
    expect(cookie).toContain('sid=sashoush');
  });

  test('maxRedirects stops at a redirect', async ({ page }) => {
    await page.goto(`${ORIGIN}/empty.html`);
    let status = 0;
    await page.route('**/redirect-me', async route => {
      const response = await route.fetch({ url: `${ORIGIN}/fx/redirect`, maxRedirects: 0 });
      status = response.status();
      await route.fulfill({ status: 200, body: 'ok' });
    });
    await page.evaluate(() => fetch('/redirect-me'));
    expect(status).toBe(302);
  });
});
