// APIResponse header surface + dispose (Playwright: RawHeaders in
// client/network.ts:931 — headers() lowercases names and joins repeats
// with ', ' ('\n' for set-cookie), headersArray() stays verbatim).
//
// `/fx/multi-cookie` replies with two Set-Cookie headers, so every
// assertion here distinguishes "kept both" from "kept one".

import { test, describe, expect } from '@ferridriver/test';

describe('api response headers', () => {
  test('headers_lowercases_and_joins_repeated_set_cookie', async ({ request, baseURL }) => {
    const resp = await request.get(`${baseURL}/fx/multi-cookie`);
    const headers = resp.headers();
    // Both values survived, joined the way Playwright joins set-cookie.
    expect(headers['set-cookie']).toBe('a=1; Path=/\nb=2; Path=/');
    // Names are lowercased regardless of the casing on the wire.
    expect(headers['content-type']).toContain('text/plain');
  });

  test('headers_array_keeps_every_duplicate_separately', async ({ request, baseURL }) => {
    const resp = await request.get(`${baseURL}/fx/multi-cookie`);
    const setCookies = resp.headersArray().filter((h) => h.name.toLowerCase() === 'set-cookie');
    expect(setCookies.length).toBe(2);
    expect(setCookies[0].value).toBe('a=1; Path=/');
    expect(setCookies[1].value).toBe('b=2; Path=/');
  });

  test('header_returns_the_combined_value_case_insensitively', async ({ request, baseURL }) => {
    const resp = await request.get(`${baseURL}/fx/multi-cookie`);
    expect(resp.header('set-cookie')).toBe('a=1; Path=/\nb=2; Path=/');
    expect(resp.header('SET-COOKIE')).toBe('a=1; Path=/\nb=2; Path=/');
    expect(resp.header('x-not-sent')).toBeNull();
  });

  test('dispose_releases_the_body_but_keeps_metadata', async ({ request, baseURL }) => {
    const resp = await request.get(`${baseURL}/fx/landed`);
    expect(await resp.text()).toBe('landed');

    await resp.dispose();

    let message = '';
    try {
      await resp.text();
    } catch (e) {
      message = String((e as Error)?.message ?? e);
    }
    expect(message).toContain('disposed');
    // Status/URL/headers stay readable after dispose, as in Playwright.
    expect(resp.status()).toBe(200);
    expect(resp.url()).toContain('/fx/landed');
    expect(resp.headers()['content-type']).toContain('text/plain');
  });
});
