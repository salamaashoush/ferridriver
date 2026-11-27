// A route handler reads what an intercepted request sends, as a spec
// picks out one save among many by its body.

import { describe, expect, test } from '@ferridriver/test';

describe('route request body', () => {
  test('a handler reads the body of a fetch it intercepts and aborts by it', async ({ page }) => {
    await page.goto('/fx/landed');
    const seen: (string | null)[] = [];
    await page.route('**/fx/echo-post**', (route) => {
      const body = route.request().postData();
      seen.push(body);
      return body?.includes('drop me') ? route.abort('internetdisconnected') : route.continue();
    });
    const outcome = await page.evaluate(async () => {
      const send = (body: string) =>
        fetch('/fx/echo-post', { method: 'POST', body, headers: { 'content-type': 'application/json' } }).then(
          () => 'sent',
          () => 'failed',
        );
      return [await send('{"words":"keep me"}'), await send('{"words":"drop me"}')];
    });
    expect(seen).toEqual(['{"words":"keep me"}', '{"words":"drop me"}']);
    expect(outcome).toEqual(['sent', 'failed']);
  });

  test('route.continue overrides postData with a different length across all browsers', async ({ page }) => {
    await page.goto('/fx/landed');
    await page.route('**/fx/echo**', (route) =>
      route.continue({ postData: JSON.stringify({ rewritten: 'much-longer-payload-than-original', ok: true }) }),
    );
    const echoed = (await page.evaluate(async () => {
      const res = await fetch('/fx/echo', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: '{"a":1}',
      });
      return res.text();
    })) as string;
    expect(echoed).toContain('much-longer-payload-than-original');
  });

  test('route.fulfill sets cookies via Set-Cookie header across all browsers', async ({ page, context }) => {
    await page.goto('/fx/landed');
    await page.route('**/fx/fake-cookie-endpoint', (route) =>
      route.fulfill({
        status: 200,
        headers: {
          'content-type': 'text/plain',
          'set-cookie': 'routed_cookie=parity_ok; Path=/',
        },
        body: 'ok',
      }),
    );
    await page.evaluate(() => fetch('/fx/fake-cookie-endpoint').then((r) => r.text()));
    const cookies = await context.cookies();
    expect(cookies.some((c) => c.name === 'routed_cookie' && c.value === 'parity_ok')).toBe(true);
  });
});
