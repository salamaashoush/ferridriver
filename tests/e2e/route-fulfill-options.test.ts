// `route.fulfill` used to accept `{ status, body: string, contentType,
// headers }` and nothing else: a `Buffer` body was silently mangled,
// `json` and `path` did not exist, and an `APIResponse` could not be
// replayed. A mocking library (MSW's browser integration is the case
// that forced this) uses every one of them.
//
// Each assertion reads the mocked response back through the page, so it
// only passes when the option actually reached the wire.

import { test, describe, expect } from '@ferridriver/test';
import type { Page, Route } from '@ferridriver/test';

/// A same-origin page from the fixture server: a `data:` document has an
/// opaque origin, so its `fetch` of the mocked URL is refused before any
/// route can answer it.
const PAGE = '/fx/landed';

/// Fetch through the page so the intercepted response is what the DOM
/// sees, not what the test process fetched.
async function fetchText(page: Page, url: string): Promise<string> {
  return String(await page.evaluate(`fetch(${JSON.stringify(url)}).then((r) => r.text())`));
}

describe('route.fulfill options', () => {
  test('json sets the body and implies application/json', async ({ page }) => {
    await page.goto(PAGE);
    await page.route('**/api/json', (route: Route) => route.fulfill({ json: { ok: true, n: 3 } }));
    const seen = String(
      await page.evaluate(
        "(async () => { const r = await fetch('/fx/api/json'); return r.headers.get('content-type') + '|' + (await r.text()); })()",
      ),
    );
    expect(seen).toContain('application/json');
    const body = await fetchText(page, '/fx/api/json');
    expect(JSON.parse(body)).toEqual({ ok: true, n: 3 });
  });

  test('a binary body survives as bytes', async ({ page }) => {
    await page.goto(PAGE);
    // A 1x1 transparent PNG: any UTF-8 round-trip corrupts it, and the
    // image then fails to decode.
    const png = Buffer.from(
      'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==',
      'base64',
    );
    await page.route('**/pixel.png', (route: Route) =>
      route.fulfill({ body: png, contentType: 'image/png' }),
    );
    const size = await page.evaluate(`
      new Promise((resolve) => {
        const img = new Image();
        img.onload = () => resolve(img.naturalWidth + 'x' + img.naturalHeight);
        img.onerror = () => resolve('error');
        img.src = '/fx/pixel.png';
      })
    `);
    expect(String(size)).toBe('1x1');
  });

  test('path serves the file and names its type', async ({ page }) => {
    await page.goto(PAGE);
    await page.route('**/served.css', (route: Route) => route.fulfill({ path: 'tests/e2e/helpers/fixture.css' }));
    const seen = String(
      await page.evaluate(
        "(async () => { const r = await fetch('/fx/served.css'); return r.headers.get('content-type') + '|' + (await r.text()); })()",
      ),
    );
    expect(seen).toContain('text/css');
    expect(seen).toContain('--fulfilled-from-disk');
  });

  test('response replays an APIResponse', async ({ page, request }) => {
    await page.goto(PAGE);
    // The upstream call goes through the HTTP client, which page routes
    // do not intercept — so the replayed status, headers and body are
    // the fixture server's own, not another mock's.
    await page.route('**/api/echo', async (route: Route) => {
      const upstream = await request.get(new URL('/fx/api/users', page.url()).href);
      await route.fulfill({ response: upstream });
    });
    const seen = String(
      await page.evaluate(
        "(async () => { const r = await fetch('/fx/api/echo'); return r.status + '|' + r.headers.get('content-type') + '|' + (await r.text()); })()",
      ),
    );
    expect(seen).toContain('200|');
    expect(seen).toContain('application/json');
    expect(seen).toContain('alice');
  });

  test('json and body together are refused', async ({ page }) => {
    await page.goto(PAGE);
    let message = '';
    await page.route('**/api/both', (route: Route) => {
      try {
        (route.fulfill as (o: unknown) => void)({ json: { a: 1 }, body: 'x' });
      } catch (e) {
        message = String((e as Error).message);
        void route.fulfill({ status: 500, body: 'refused' });
      }
    });
    await fetchText(page, '/fx/api/both');
    expect(message).toContain('Can specify either body or json parameters');
  });

  test('unroute(url, handler) drops one handler and leaves the other', async ({ page }) => {
    await page.goto(PAGE);
    const first = (route: Route) => route.fulfill({ body: 'first' });
    const second = (route: Route) => route.fulfill({ body: 'second' });
    await page.route('**/api/two', first);
    await page.route('**/api/two', second);
    // Playwright runs the LAST registered handler first; removing it
    // leaves the earlier one serving.
    expect(await fetchText(page, '/fx/api/two')).toBe('second');
    await page.unroute('**/api/two', second);
    expect(await fetchText(page, '/fx/api/two')).toBe('first');
    await page.unroute('**/api/two', first);
  });
});
