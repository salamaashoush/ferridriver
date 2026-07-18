// Context-bound HTTP client (Playwright: `page.request` /
// `context.request` share the browser context's cookie jar —
// client/browserContext.ts:76, server/fetch.ts:649). Every test
// observes a cookie or header crossing the browser<->client boundary,
// which only happens when the bridge is real.

import { test, describe, expect } from '@ferridriver/test';

describe('context-bound request', () => {
  test('page_request_sends_browser_cookies', async ({ page, context, baseURL }) => {
    await page.goto(`${baseURL}/fx/landed`);
    await context.addCookies([{ name: 'ctxsid', value: 'from-browser', domain: '127.0.0.1', path: '/' }]);
    const resp = await page.request.get(`${baseURL}/fx/echo-headers`);
    expect(resp.status()).toBe(200);
    const headers = (await resp.json()) as Record<string, string>;
    expect(String(headers.cookie ?? '')).toContain('ctxsid=from-browser');
  });

  test('document_cookie_reaches_page_request', async ({ page, baseURL }) => {
    await page.goto(`${baseURL}/fx/landed`);
    await page.evaluate("document.cookie = 'fromdoc=yes; path=/'");
    const resp = await page.request.get(`${baseURL}/fx/echo-headers`);
    const headers = (await resp.json()) as Record<string, string>;
    expect(String(headers.cookie ?? '')).toContain('fromdoc=yes');
  });

  test('response_set_cookie_lands_in_browser', async ({ page, context, baseURL }) => {
    await page.goto(`${baseURL}/fx/landed`);
    const cookie = encodeURIComponent('apisid=set-by-request; Path=/');
    const resp = await context.request.get(`${baseURL}/fx/set-cookie?c=${cookie}`);
    expect(resp.status()).toBe(200);
    const cookies = await context.cookies();
    const stored = cookies.find((c) => c.name === 'apisid');
    expect(stored?.value).toBe('set-by-request');
    // Browser-visible: the document reads the cookie the API client set.
    const docCookie = await page.evaluate('document.cookie');
    expect(String(docCookie)).toContain('apisid=set-by-request');
  });

  test('redirect_hop_set_cookie_is_captured', async ({ page, context, baseURL }) => {
    await page.goto(`${baseURL}/fx/landed`);
    const cookie = encodeURIComponent('hopsid=from-hop; Path=/');
    const resp = await page.request.get(`${baseURL}/fx/set-cookie-redirect?c=${cookie}`);
    // Followed through to the landing page...
    expect(resp.status()).toBe(200);
    expect(await resp.text()).toBe('landed');
    // ...and the intermediate 302's Set-Cookie reached the browser.
    const cookies = await context.cookies();
    expect(cookies.find((c) => c.name === 'hopsid')?.value).toBe('from-hop');
  });

  test('page_and_context_request_share_one_jar', async ({ page, context, baseURL }) => {
    await page.goto(`${baseURL}/fx/landed`);
    const cookie = encodeURIComponent('sharedsid=one-jar; Path=/');
    await context.request.get(`${baseURL}/fx/set-cookie?c=${cookie}`);
    // A cookie set through context.request is sent by page.request.
    const resp = await page.request.get(`${baseURL}/fx/echo-headers`);
    const headers = (await resp.json()) as Record<string, string>;
    expect(String(headers.cookie ?? '')).toContain('sharedsid=one-jar');
  });

  test('explicit_cookie_header_is_not_overridden', async ({ page, context, baseURL }) => {
    await page.goto(`${baseURL}/fx/landed`);
    await context.addCookies([{ name: 'jarred', value: '1', domain: '127.0.0.1', path: '/' }]);
    const resp = await page.request.get(`${baseURL}/fx/echo-headers`, {
      headers: { cookie: 'manual=only' },
    });
    const headers = (await resp.json()) as Record<string, string>;
    expect(String(headers.cookie)).toBe('manual=only');
  });

  test('request_option_headers_object', async ({ page, baseURL }) => {
    await page.goto(`${baseURL}/fx/landed`);
    const resp = await page.request.get(`${baseURL}/fx/echo-headers`, {
      headers: { 'x-custom-probe': 'header-took-effect' },
    });
    const headers = (await resp.json()) as Record<string, string>;
    expect(headers['x-custom-probe']).toBe('header-took-effect');
  });

  test('request_option_params_object', async ({ page, baseURL }) => {
    await page.goto(`${baseURL}/fx/landed`);
    // string | number | boolean scalars, per Playwright's params type.
    const resp = await page.request.get(`${baseURL}/_api/echo`, {
      params: { q: 'find me', n: 7, flag: true },
    });
    const echoed = (await resp.json()) as Record<string, any>;
    const url = String(echoed.url);
    expect(url).toContain('q=find+me');
    expect(url).toContain('n=7');
    expect(url).toContain('flag=true');
  });

  test('request_option_form_urlencoded', async ({ page, baseURL }) => {
    await page.goto(`${baseURL}/fx/landed`);
    const resp = await page.request.post(`${baseURL}/_api/echo`, {
      form: { user: 'ferri driver', count: 2 },
    });
    const echoed = (await resp.json()) as Record<string, any>;
    expect(String(echoed.headers['content-type'])).toContain('application/x-www-form-urlencoded');
    expect(String(echoed.data)).toContain('user=ferri+driver');
    expect(String(echoed.data)).toContain('count=2');
  });

  test('request_option_json_body', async ({ page, baseURL }) => {
    await page.goto(`${baseURL}/fx/landed`);
    const resp = await page.request.post(`${baseURL}/_api/echo`, {
      json: { nested: { ok: true }, n: 3 },
    });
    const echoed = (await resp.json()) as Record<string, any>;
    expect(String(echoed.headers['content-type'])).toContain('application/json');
    expect(echoed.json).toEqual({ nested: { ok: true }, n: 3 });
  });

  test('request_option_data_object_sent_as_json', async ({ page, baseURL }) => {
    await page.goto(`${baseURL}/fx/landed`);
    // Playwright routes a serializable `data` value as a JSON body.
    const resp = await page.request.post(`${baseURL}/_api/echo`, {
      data: { via: 'data-option' },
    });
    const echoed = (await resp.json()) as Record<string, any>;
    expect(String(echoed.headers['content-type'])).toContain('application/json');
    expect(echoed.json).toEqual({ via: 'data-option' });
  });

  test('request_option_fail_on_status_code', async ({ page, baseURL }) => {
    await page.goto(`${baseURL}/fx/landed`);
    // Without the option a 404 resolves normally...
    const soft = await page.request.get(`${baseURL}/fx/nonexistent-route`);
    expect(soft.status()).toBe(404);
    // ...with it the same request rejects.
    let message = '';
    try {
      await page.request.get(`${baseURL}/fx/nonexistent-route`, { failOnStatusCode: true });
    } catch (e) {
      message = String((e as Error).message ?? e);
    }
    expect(message).toContain('404');
  });

  test('request_option_max_redirects', async ({ page, baseURL }) => {
    await page.goto(`${baseURL}/fx/landed`);
    // 0 = do not follow: the 302 comes back as-is.
    const raw = await page.request.get(`${baseURL}/fx/redirect`, { maxRedirects: 0 });
    expect(raw.status()).toBe(302);
    // A chain longer than the cap rejects.
    let message = '';
    try {
      await page.request.get(`${baseURL}/fx/redirect/5`, { maxRedirects: 2 });
    } catch (e) {
      message = String((e as Error).message ?? e);
    }
    expect(message).toContain('too many redirects');
  });

  test('request_option_timeout', async ({ page, baseURL }) => {
    await page.goto(`${baseURL}/fx/landed`);
    // /fx/download-hang dribbles a 1MB body for ~30s; a 500ms budget
    // must abort the request rather than hang.
    let message = '';
    try {
      await page.request.get(`${baseURL}/fx/download-hang`, { timeout: 500 });
    } catch (e) {
      message = String((e as Error).message ?? e);
    }
    expect(message.length).toBeGreaterThan(0);
  });

  test('base_url_comes_from_context_options', async ({ browser, baseURL }) => {
    const ctx = await browser.newContext({ baseURL: baseURL! });
    try {
      const resp = await ctx.request.get('/fx/landed');
      expect(resp.status()).toBe(200);
      expect(await resp.text()).toBe('landed');
    } finally {
      await ctx.close();
    }
  });

  test('extra_http_headers_are_read_live', async ({ page, context, baseURL }) => {
    await page.goto(`${baseURL}/fx/landed`);
    // Mutating the option AFTER the client exists must be visible on the
    // next request (Playwright reads context defaults per request).
    const client = context.request;
    await context.setExtraHTTPHeaders({ 'x-live-header': 'picked-up' });
    const resp = await client.get(`${baseURL}/fx/echo-headers`);
    const headers = (await resp.json()) as Record<string, string>;
    expect(headers['x-live-header']).toBe('picked-up');
  });

  test('expired_cookie_is_not_sent', async ({ page, context, baseURL }) => {
    await page.goto(`${baseURL}/fx/landed`);
    // Max-Age=0 expires immediately; the browser drops it, and so must
    // the bridge's request-side matcher.
    const gone = encodeURIComponent('gone=1; Path=/; Max-Age=0');
    const kept = encodeURIComponent('kept=1; Path=/');
    await page.request.get(`${baseURL}/fx/set-cookie?c=${gone}&c=${kept}`);
    const resp = await page.request.get(`${baseURL}/fx/echo-headers`);
    const headers = (await resp.json()) as Record<string, string>;
    const cookieHeader = String(headers.cookie ?? '');
    expect(cookieHeader).toContain('kept=1');
    expect(cookieHeader).not.toContain('gone=1');
  });
});

describe('request option bags', () => {
  test('multipart_option_sends_a_form_data_body', async ({ context, baseURL }) => {
    const resp = await context.request.post(`${baseURL}/fx/echo-request`, {
      multipart: {
        field: 'plain',
        count: 7,
        file: { name: 'note.txt', mimeType: 'text/plain', buffer: 'file-bytes' },
      },
    });
    expect(resp.status()).toBe(200);
    const echoed = (await resp.json()) as { method: string; headers: Record<string, string>; body: string };
    expect(echoed.method).toBe('POST');
    expect(String(echoed.headers['content-type'] ?? '')).toContain('multipart/form-data; boundary=');
    expect(echoed.body).toContain('name="field"');
    expect(echoed.body).toContain('plain');
    expect(echoed.body).toContain('7');
    expect(echoed.body).toContain('filename="note.txt"');
    expect(echoed.body).toContain('Content-Type: text/plain');
    expect(echoed.body).toContain('file-bytes');
  });

  test('fetch_accepts_a_page_request_and_replays_it', async ({ page, context, baseURL }) => {
    // Capture a real page-network Request, then replay it through the
    // API client: its method, headers and body must ride along.
    await page.goto(`${baseURL}/fx/landed`);
    const [captured] = await Promise.all([
      page.waitForRequest((r) => r.url().includes('/fx/echo-request')),
      page.evaluate(
        `fetch('/fx/echo-request', { method: 'PUT', headers: { 'x-from-page': 'yes' }, body: 'page-body' })`,
      ),
    ]);

    const replayed = await context.request.fetch(captured);
    const echoed = (await replayed.json()) as { method: string; headers: Record<string, string>; body: string };
    expect(echoed.method).toBe('PUT');
    expect(echoed.headers['x-from-page']).toBe('yes');
    // Whatever body the capture carried is what gets replayed. Firefox
    // (BiDi) does not surface post data for a page-initiated fetch, so
    // `postData()` is null there and the replay is body-less — which is
    // exactly what must NOT announce a stale content-length.
    expect(echoed.body).toBe(captured.postData() ?? '');
  });

  test('fetch_options_override_the_request_they_replay', async ({ page, context, baseURL }) => {
    await page.goto(`${baseURL}/fx/landed`);
    const [captured] = await Promise.all([
      page.waitForRequest((r) => r.url().includes('/fx/echo-request')),
      page.evaluate(
        `fetch('/fx/echo-request', { method: 'PUT', headers: { 'x-from-page': 'yes' }, body: 'page-body' })`,
      ),
    ]);

    const replayed = await context.request.fetch(captured, {
      method: 'POST',
      headers: { 'x-from-page': 'overridden' },
      data: 'new-body',
    });
    const echoed = (await replayed.json()) as { method: string; headers: Record<string, string>; body: string };
    expect(echoed.method).toBe('POST');
    expect(echoed.headers['x-from-page']).toBe('overridden');
    expect(echoed.body).toBe('new-body');
  });

  test('dispose_leaves_the_shared_context_client_usable', async ({ context, baseURL }) => {
    // Playwright's dispose() releases the caller's handle; the browser
    // context that vended it keeps working.
    await context.request.dispose();
    const resp = await context.request.get(`${baseURL}/fx/landed`);
    expect(await resp.text()).toBe('landed');
  });
});
