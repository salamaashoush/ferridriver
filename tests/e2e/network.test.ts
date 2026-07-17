// Ported from crates/ferridriver-cli/tests/backends_support/
// {network,navigation_response}.rs — Request/Response lifecycle objects
// (redirect chain, abort/requestfailed, response body, post data,
// headers, httpVersion), route registration semantics (disposable,
// times budget, per-page handlers, unroute from sibling wrappers,
// routeFromHAR, fallback), and navigation responses (goto/reload/
// history traversal). Test titles mirror the original Rust fn names.
// All real-origin traffic goes through the fixture server (baseURL).

import { test, describe, expect } from '@ferridriver/test';
import type { Request, Response } from '@ferridriver/test';

describe('network', () => {
  test('network_redirect_chain', async ({ page, baseURL }) => {
    // Redirect: /fx/redirect -> 302 -> /fx/landed. The live Request
    // chain links forward (redirectedTo) and backwards (redirectedFrom),
    // and the prior 302 response is reachable via
    // request.redirectedFrom().response().
    const wait = page.waitForResponse(`${baseURL}/fx/landed`, { timeout: 10000 });
    await page.goto('/fx/redirect');
    const resp = await wait;
    const req = resp.request();
    const prev = req.redirectedFrom();
    expect(prev).not.toBeNull();
    const prevResp = await prev!.response();
    expect(resp.url()).toBe(`${baseURL}/fx/landed`);
    expect(resp.status()).toBe(200);
    expect(prev!.url().endsWith('/fx/redirect')).toBe(true);
    expect(prevResp?.status()).toBe(302);
  });

  test('network_request_failure', async ({ page, browserName }) => {
    // A route that aborts the request makes the page-side fetch reject
    // and fires `requestfailed` with failure().errorText carrying each
    // backend's native abort reason.
    await page.goto('/fx/landed');
    await page.route('**/api/blocked-by-route', (route) => {
      route.abort('blockedbyclient');
    });
    const failedPromise = page
      .waitForEvent('requestfailed', { timeout: 10000 })
      .catch(() => null);
    const fetchOutcome = await page.evaluate(
      "fetch('/fx/api/blocked-by-route').then(() => 'ok').catch(() => 'blocked')",
    );
    const failedEvent = (await failedPromise) as Request | null;
    expect(fetchOutcome).toBe('blocked');
    expect(failedEvent).not.toBeNull();
    expect(failedEvent!.url().includes('/api/blocked-by-route')).toBe(true);
    const failure = failedEvent!.failure();
    const text = failure ? failure.errorText : '';
    // CDP: net::ERR_BLOCKED_BY_CLIENT or the literal reason string;
    // BiDi (Firefox): NS_ERROR_ABORT; WebKit: "Blocked by Web Inspector".
    const known =
      text.includes('blockedbyclient') ||
      text.includes('net::ERR_BLOCKED') ||
      text.includes('NS_ERROR_ABORT') ||
      text.includes('Blocked by Web Inspector');
    expect([browserName, known]).toEqual([browserName, true]);
  });

  test('route_disposable', async ({ page }) => {
    // `page.route(url, handler)` returns a Disposable whose dispose()
    // reverses the registration; a second dispose() is an idempotent
    // no-op.
    await page.goto('/fx/landed');
    const disposable = await page.route('**/api/users', (route) => {
      route.fulfill({ status: 200, contentType: 'application/json', body: '{"mocked":true}' });
    });
    const mocked = (await page.evaluate("fetch('/fx/api/users').then(r => r.text())")) as string;
    await disposable.dispose();
    const afterFirst = (await page.evaluate("fetch('/fx/api/users').then(r => r.text())")) as string;
    await disposable.dispose();
    const afterSecond = (await page.evaluate("fetch('/fx/api/users').then(r => r.text())")) as string;
    expect(mocked.includes('"mocked":true')).toBe(true);
    expect(afterFirst.includes('alice')).toBe(true);
    expect(afterSecond.includes('alice')).toBe(true);
  });

  test('route_predicate_preserves_times_budget', async ({ page }) => {
    // A times:1 route whose URL predicate rejects a request must keep
    // its budget: predicates are evaluated during matching, before
    // willExpire consumes times.
    await page.goto('/fx/landed');
    await page.route(
      (url) => url.pathname === '/fx/api/users',
      (route) => {
        route.fulfill({ status: 200, contentType: 'application/json', body: '{"mocked":true}' });
      },
      { times: 1 },
    );
    const rejected = (await page.evaluate("fetch('/fx/api/posts').then(r => r.text())")) as string;
    const mocked = (await page.evaluate("fetch('/fx/api/users').then(r => r.text())")) as string;
    const after = (await page.evaluate("fetch('/fx/api/users').then(r => r.text())")) as string;
    expect(rejected.includes('first')).toBe(true);
    expect(mocked.includes('"mocked":true')).toBe(true);
    expect(after.includes('alice')).toBe(true);
  });

  test('route_two_pages_keep_their_own_handlers', async ({ page, context }) => {
    // Route ids live in the session-shared registry; every wrapper
    // minted for a page must draw from the registry-global counter so
    // the second page's registration never overwrites the first's.
    await page.goto('/fx/landed');
    const page2 = await context.newPage();
    try {
      await page.route('**/api/users', (route) => {
        route.fulfill({ status: 200, contentType: 'application/json', body: '"first-page"' });
      });
      await page2.route('**/api/users', (route) => {
        route.fulfill({ status: 200, contentType: 'application/json', body: '"second-page"' });
      });
      await page2.goto('/fx/landed');
      const fromFirst = await page.evaluate("fetch('/fx/api/users').then(r => r.text())");
      const fromSecond = await page2.evaluate("fetch('/fx/api/users').then(r => r.text())");
      expect(fromFirst).toBe('"first-page"');
      expect(fromSecond).toBe('"second-page"');
    } finally {
      await page.unrouteAll();
      await page2.close();
    }
  });

  test('unroute_predicate_from_other_wrapper', async ({ page }) => {
    // unroute(fn) works from ANY wrapper of the page — wrappers are
    // minted freely and the matcher table is keyed in the session-shared
    // registry.
    await page.goto('/fx/landed');
    const pred = (url: URL) => url.pathname === '/fx/api/users';
    await page.route(pred, (route) => {
      route.fulfill({ status: 200, contentType: 'application/json', body: '"routed"' });
    });
    const before = await page.evaluate("fetch('/fx/api/users').then(r => r.text())");
    const otherWrapper = page.mainFrame().page();
    await otherWrapper.unroute(pred);
    const after = (await page.evaluate("fetch('/fx/api/users').then(r => r.text())")) as string;
    expect(before).toBe('"routed"');
    expect(after.includes('alice')).toBe(true);
  });

  test('route_from_har', async ({ page }) => {
    // routeFromHAR replays a recorded response for a matching request;
    // an unrecorded URL with notFound:'fallback' reaches the real
    // server.
    await page.goto('/fx/landed');
    const recordedUrl = `${page.url().replace(/\/fx\/landed$/, '')}/fx/api/users`;
    const har = JSON.stringify({
      log: {
        version: '1.2',
        entries: [
          {
            request: { method: 'GET', url: recordedUrl },
            response: {
              status: 200,
              headers: [{ name: 'content-type', value: 'application/json' }],
              content: { mimeType: 'application/json', text: '{"users":["from-har"]}' },
            },
          },
        ],
      },
    });
    const harPath = test.info().outputPath('rec.har');
    await fs.writeFile(harPath, har);
    try {
      await page.routeFromHAR(harPath, { notFound: 'fallback' });
      const served = (await page.evaluate("fetch('/fx/api/users').then(r => r.text())")) as string;
      const real = (await page.evaluate("fetch('/fx/landed').then(r => r.text())")) as string;
      expect(served.includes('from-har')).toBe(true);
      expect(real.includes('landed')).toBe(true);
    } finally {
      await page.unrouteAll();
    }
  });

  test('network_response_body', async ({ page }) => {
    // BiDi bodies ride the session's network.addDataCollector
    // registration (without it Firefox discards the bytes).
    await page.goto('/fx/landed');
    const wait = page.waitForResponse('**/api/users', { timeout: 10000 });
    await page.evaluate("fetch('/fx/api/users').then(r => r.text())");
    const resp = await wait;
    const text = await resp.text();
    const json = (await resp.json()) as { users: string[] };
    const headerValue = await resp.headerValue('content-type');
    expect(resp.status()).toBe(200);
    expect(text.includes('alice')).toBe(true);
    expect(json.users.length).toBe(2);
    expect(headerValue?.includes('application/json')).toBe(true);
  });

  test('network_post_data', async ({ page, browserName }) => {
    await page.goto('/fx/landed');
    const wait = page.waitForRequest('**/fx/echo', { timeout: 10000 });
    await page.evaluate(
      "fetch('/fx/echo', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ ping: 'pong', n: 7 }) }).then(r => r.text())",
    );
    const req = await wait;
    const data = req.postData();
    const parsed = req.postDataJSON() as { ping: string; n: number } | null;
    expect(req.method()).toBe('POST');
    if (browserName === 'firefox') {
      // BiDi's network.beforeRequestSent.request.body is null for fetch
      // with a body in current Firefox builds — postData stays null.
      expect(data == null || data === '').toBe(true);
      return;
    }
    expect(data?.includes('"ping":"pong"')).toBe(true);
    expect(parsed?.ping).toBe('pong');
    expect(parsed?.n).toBe(7);
  });

  test('network_post_data_buffer', async ({ page, browserName }) => {
    // postDataBuffer() returns the raw POST body bytes as a Uint8Array.
    await page.goto('/fx/landed');
    const wait = page.waitForRequest('**/fx/echo', { timeout: 10000 });
    await page.evaluate(
      "fetch('/fx/echo', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ buf: 'yes', n: 9 }) }).then(r => r.text())",
    );
    const req = await wait;
    const buf = req.postDataBuffer();
    if (browserName === 'firefox') {
      expect(buf).toBeNull();
      return;
    }
    expect(buf).not.toBeNull();
    expect(buf!.length).toBeGreaterThan(0);
    const decoded = new TextDecoder().decode(buf!);
    expect(decoded.includes('"buf":"yes"')).toBe(true);
  });

  test('network_headers', async ({ page, browserName }) => {
    await page.goto('/fx/landed');
    const cookieWait = page.waitForResponse('**/fx/multi-cookie', { timeout: 10000 });
    const uaWait = page.waitForRequest('**/fx/echo-headers', { timeout: 10000 });
    await page.evaluate("fetch('/fx/multi-cookie').then(r => r.text())");
    const cookieResp = await cookieWait;
    await page.evaluate("fetch('/fx/echo-headers').then(r => r.text())");
    const uaReq = await uaWait;
    const uaHeaders = uaReq.headers();
    const uaName = Object.keys(uaHeaders).find((k) => k.toLowerCase() === 'user-agent');
    // Browser-added request headers surface on every backend.
    expect(uaName != null && uaHeaders[uaName!].length > 0).toBe(true);
    if (browserName === 'chromium') {
      // CDP preserves duplicate header entries via
      // responseReceivedExtraInfo.
      const cookieEntries = (await cookieResp.headersArray()).filter(
        (h) => h.name.toLowerCase() === 'set-cookie',
      );
      expect(cookieEntries.length).toBe(2);
    }
    const joined = await cookieResp.headerValue('set-cookie');
    expect(joined?.includes('a=1')).toBe(true);
    expect(joined?.includes('b=2')).toBe(true);
  });

  test('network_http_version', async ({ page, browserName }) => {
    // httpVersion() always resolves to a string; CDP reports the real
    // protocol version, BiDi surfaces ResponseData.protocol.
    await page.goto('/fx/landed');
    const wait = page.waitForResponse('**/api/users', { timeout: 10000 });
    await page.evaluate("fetch('/fx/api/users').then(r => r.text())");
    const resp = await wait;
    const hv = await resp.httpVersion();
    expect(typeof hv).toBe('string');
    if (browserName === 'chromium' || browserName === 'firefox') {
      expect(hv.toLowerCase().includes('http')).toBe(true);
    }
  });

  test('route_fallback_applies_overrides', async ({ page }) => {
    // route.fallback({ url }) with no further matching handler lets the
    // request proceed with the override applied — the page observes the
    // /fx/api/posts payload for a /fx/api/users fetch.
    await page.goto('/fx/landed');
    await page.route('**/api/users', (route) => {
      const target = route.request().url().replace('/fx/api/users', '/fx/api/posts');
      route.fallback({ url: target });
    });
    try {
      const text = (await page.evaluate("fetch('/fx/api/users').then(r => r.text())")) as string;
      expect(text.includes('posts')).toBe(true);
    } finally {
      await page.unroute('**/api/users');
    }
  });

  test('route_fallback_chains_to_next_handler', async ({ page }) => {
    // route.fallback() passes the request to the NEXT matching handler
    // (newest-first), with fallback overrides visible to that handler —
    // Playwright's Page._onRoute chain, not a plain continue.
    await page.goto('data:text/html,<body>chain</body>');
    const matcher = 'https://ferri.test/**';
    await page.route(matcher, (route) => {
      const seen = route.request().headers()['x-chain'] || 'missing';
      route.fulfill({ status: 200, contentType: 'text/html', body: '<body>OLDER:' + seen + '</body>' });
    });
    await page.route(matcher, (route) => {
      route.fallback({ headers: { ...route.request().headers(), 'x-chain': 'yes' } });
    });
    try {
      await page.goto('https://ferri.test/chained', { timeout: 10000 });
      const text = String(await page.evaluate(() => document.body.textContent));
      expect(text.includes('OLDER:')).toBe(true);
      expect(text.includes('OLDER:yes')).toBe(true);
    } finally {
      await page.unrouteAll();
    }
  });

  test('request_existing_response', async ({ page }) => {
    // request.existingResponse() (Playwright 1.59) returns the response
    // already received for a completed navigation, without awaiting.
    const resp = (await page.goto('data:text/html,<title>existing-response</title>')) as Response;
    const req = resp.request();
    const existing = await req.existingResponse();
    const viaWait = await req.response();
    expect(existing).not.toBeNull();
    expect(existing!.url()).toBe(resp.url());
    expect(existing!.status()).toBe(viaWait!.status());
  });

  test('goto_returns_response', async ({ page, baseURL }) => {
    const resp = await page.goto('/fx/landed');
    expect(resp).not.toBeNull();
    expect(resp!.status()).toBe(200);
    expect(resp!.ok()).toBe(true);
    expect(resp!.url()).toBe(`${baseURL}/fx/landed`);
  });

  test('goto_follows_redirects', async ({ page }) => {
    // goto follows redirects and returns the Response of the final
    // landed document (not the 302).
    const resp = await page.goto('/fx/redirect');
    expect(resp).not.toBeNull();
    expect(resp!.status()).toBe(200);
    expect(resp!.url().endsWith('/fx/landed')).toBe(true);
  });

  test('goto_network_failure', async ({ page }) => {
    // goto to an unreachable URL rejects with a typed error — not a
    // Response-with-status-0.
    let threw = false;
    let msg = '';
    try {
      await page.goto('http://127.0.0.1:65531/unreachable');
    } catch (e) {
      threw = true;
      msg = String((e as Error).message ?? e);
    }
    expect(threw).toBe(true);
    expect(
      msg.includes('ERR_CONNECTION') ||
        msg.includes('NS_ERROR') ||
        msg.includes('failed') ||
        msg.includes('refused') ||
        msg.includes('Navigation') ||
        msg.includes('Could not connect'),
    ).toBe(true);
  });

  test('reload_returns_response', async ({ page, baseURL }) => {
    await page.goto('/fx/landed');
    const resp = await page.reload();
    expect(resp).not.toBeNull();
    expect(resp!.status()).toBe(200);
    expect(resp!.ok()).toBe(true);
    expect(resp!.url()).toBe(`${baseURL}/fx/landed`);
  });

  test('history_traversal_returns_response', async ({ page }) => {
    await page.goto('/fx/landed');
    await page.goto('/fx/api/users');
    const back = await page.goBack();
    const fwd = await page.goForward();
    expect(back).not.toBeNull();
    expect(fwd).not.toBeNull();
    expect(back!.status()).toBe(200);
    expect(back!.url().endsWith('/fx/landed')).toBe(true);
    expect(fwd!.status()).toBe(200);
    expect(fwd!.url().endsWith('/fx/api/users')).toBe(true);
  });
});
