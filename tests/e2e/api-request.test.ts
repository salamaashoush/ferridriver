// Ported from crates/ferridriver-cli/tests/backends_support/
// api_response.rs — the browser-independent HTTP client (`request`
// fixture). Test titles mirror the original Rust fn names.

import { test, describe, expect } from '@ferridriver/test';

describe('api request', () => {
  test('api_response_server_addr', async ({ request, baseURL }) => {
    // apiResponse.serverAddr() reports the resolved peer address — a
    // protocol-visible effect that only holds when the value is captured
    // end-to-end.
    const expectedPort = Number(new URL(baseURL!).port);
    const resp = await request.get(`${baseURL}/fx/landed`);
    const addr = await resp.serverAddr();
    expect(resp.status()).toBe(200);
    expect(addr).not.toBeNull();
    expect(addr!.ipAddress).toBe('127.0.0.1');
    expect(addr!.port).toBe(expectedPort);
  });

  test('request_new_context_takes_playwright_options', async ({ request, baseURL }) => {
    const api = await request.newContext({
      baseURL: `${baseURL}/fx/`,
      extraHTTPHeaders: { 'x-acme': 'one' },
      userAgent: 'acme-agent/1.0',
      failOnStatusCode: true,
      maxRedirects: 0,
      storageState: {
        cookies: [{ name: 'sid', value: 'seeded', domain: '127.0.0.1', path: '/', expires: -1, httpOnly: false, secure: false, sameSite: 'Lax' }],
        origins: [{ origin: 'https://example.com', localStorage: [{ name: 'k', value: 'v' }] }],
      },
    });
    const headers = await (await api.get('echo-headers')).json() as Record<string, string>;
    expect(headers['x-acme']).toBe('one');
    expect(headers['user-agent']).toBe('acme-agent/1.0');
    expect(await (await api.get('http-client/echo')).text()).toContain('sid=seeded');
    const failed = await api.get('status/404').then(() => 'resolved', (e: Error) => e.message);
    expect(failed).toContain('404');
    const hop = await api.get('http-client/redirect/1');
    expect(hop.status()).toBeGreaterThanOrEqual(300);
    expect(hop.status()).toBeLessThan(400);
    const state = await api.storageState();
    expect(state.cookies.map(cookie => cookie.name)).toContain('sid');
    expect(state.origins).toEqual([{ origin: 'https://example.com', localStorage: [{ name: 'k', value: 'v' }] }]);
    const refused = await request.newContext({ proxy: { server: 'http://proxy.example.com' } } as never)
      .then(() => 'made', (e: Error) => e.message);
    expect(refused).toContain('not supported');
  });
});
