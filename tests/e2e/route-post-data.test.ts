// A route handler reads what an intercepted request sends, as a spec
// picks out one save among many by its body.

import { describe, expect, test } from '@ferridriver/test';

describe('route request body', () => {
  test('a handler reads the body of a fetch it intercepts and aborts by it', async ({ page, browserName }) => {
    test.skip(browserName === 'firefox', "Playwright's BiDi driver never reads an intercepted request's body");
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
});
