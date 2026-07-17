// Ported from crates/ferridriver-cli/tests/backends_support/
// {script_emul_storage,storage_state,web_storage}.rs — context-level
// emulation (userAgent, viewport, geolocation, offline), cookies,
// localStorage, storageState export/import, the WebStorage accessors,
// and the markdown snapshot. Test titles mirror the original Rust fn
// names.

import { test, describe, expect } from '@ferridriver/test';
import type { Cookie } from '@ferridriver/test';
import { dataUrl } from './helpers/html';

describe('emulation and storage', () => {
  test('script_user_agent', async ({ browser }) => {
    // userAgent is a context-level option: a fresh context with the
    // override set reports it from navigator.userAgent. CDP applies it
    // via Network.setUserAgentOverride, BiDi via
    // emulation.setUserAgentOverride, WebKit via the per-page attach
    // overrides.
    const ctx = await browser.newContext({ userAgent: 'TestBot/1.0' });
    try {
      const p = await ctx.newPage();
      await p.goto(dataUrl('<body>ua</body>'));
      expect(await p.evaluate('navigator.userAgent')).toBe('TestBot/1.0');
    } finally {
      await ctx.close();
    }
  });

  test('script_viewport', async ({ page }) => {
    await page.goto(dataUrl('<body></body>'));
    await page.setViewportSize({ width: 375, height: 812 });
    expect(await page.evaluate('window.innerWidth')).toBe(375);
    expect(await page.evaluate('window.innerHeight')).toBe(812);
  });

  test('script_geolocation', async ({ page, context }) => {
    await page.goto(dataUrl('<body></body>'));
    await context.setGeolocation({ latitude: 37.7749, longitude: -122.4194, accuracy: 1.0 });
    expect(await page.evaluate('typeof navigator.geolocation')).toBe('object');
  });

  test('script_offline', async ({ page, context }) => {
    // CDP: Network.emulateNetworkConditions; BiDi:
    // emulation.setNetworkConditions with networkConditions
    // {type:'offline'} | null; WebKit: per-page emulation replay.
    await page.goto(dataUrl('<body></body>'));
    await context.setOffline(true);
    expect(await page.evaluate('navigator.onLine')).toBe(false);
    await context.setOffline(false);
    expect(await page.evaluate('navigator.onLine')).toBe(true);
  });

  test('script_cookies', async ({ page, context }) => {
    await page.goto('/fx/landed');
    await context.addCookies([
      { name: 'k', value: 'v', domain: '127.0.0.1', path: '/', secure: false, httpOnly: false, sameSite: 'Lax' },
    ]);
    const cookies = await context.cookies();
    const found = cookies.find((c: Cookie) => c.name === 'k');
    expect(found?.value).toBe('v');
    await context.deleteCookie('k');
    const after = await context.cookies();
    expect(after.filter((c: Cookie) => c.name === 'k').length).toBe(0);
  });

  test('script_localstorage', async ({ page }) => {
    // localStorage lives in the page — drive it through page.evaluate
    // on a real origin (data:/about: origins have opaque storage).
    await page.goto('/fx/landed');
    await page.evaluate("localStorage.setItem('lk', 'lv')");
    expect(await page.evaluate("localStorage.getItem('lk')")).toBe('lv');
    expect((await page.evaluate('localStorage.length')) as number).toBeGreaterThanOrEqual(1);
  });

  test('script_storage_state', async ({ page, context, baseURL }) => {
    // Playwright: context.storageState(): Promise<{ cookies, origins }>
    // — set a cookie + a localStorage entry on a real origin, export,
    // and assert BOTH appear with the camelCase wire shape.
    await page.goto('/fx/landed');
    await context.addCookies([
      { name: 'ssk', value: 'ssv', domain: '127.0.0.1', path: '/', secure: false, httpOnly: false, sameSite: 'Lax' },
    ]);
    await page.evaluate("localStorage.setItem('sk', 'sv')");
    const state = await context.storageState();
    const cookie = state.cookies.find((c) => c.name === 'ssk');
    const origin = state.origins.find((o) => o.origin === new URL(baseURL!).origin);
    const item = origin ? origin.localStorage.find((kv) => kv.name === 'sk') : null;
    expect(cookie?.value).toBe('ssv');
    expect(origin != null).toBe(true);
    expect(item?.value).toBe('sv');
  });

  test('context_set_storage_state', async ({ page, context }) => {
    // context.setStorageState clears existing cookies and applies the
    // new state: a pre-seeded cookie is gone, the state's cookie is
    // present. (Cookies work on the data: origin where localStorage is
    // opaque.)
    await page.goto(dataUrl('<body>storage</body>'));
    await context.addCookies([{ name: 'stale', value: 'yes', domain: 'example.com', path: '/' }]);
    await context.setStorageState({
      cookies: [{ name: 'seeded', value: 'fromState', domain: 'example.com', path: '/' }],
      origins: [],
    });
    const names = (await context.cookies()).map((c: Cookie) => c.name);
    expect(names.includes('stale')).toBe(false);
    expect(names.includes('seeded')).toBe(true);
  });

  test('web_storage', async ({ page }) => {
    // page.localStorage / page.sessionStorage driver-side accessors:
    // setItem -> getItem -> items -> removeItem -> clear, cross-checked
    // against the live DOM API on a real origin.
    await page.goto('/fx/landed');
    await page.localStorage.setItem('token', 'abc');
    await page.localStorage.setItem('user', 'sam');
    await page.sessionStorage.setItem('sid', 'sess-1');

    expect(await page.localStorage.getItem('token')).toBe('abc');
    expect((await page.localStorage.getItem('nope')) == null).toBe(true);
    const items = await page.localStorage.items();
    expect(items.map((i) => i.name).sort()).toEqual(['token', 'user']);
    expect(items.find((i) => i.name === 'token')?.value).toBe('abc');
    // Cross-check against the live DOM API to prove we hit real storage.
    expect(await page.evaluate(() => window.localStorage.getItem('token'))).toBe('abc');
    expect(await page.evaluate(() => window.sessionStorage.getItem('sid'))).toBe('sess-1');

    await page.localStorage.removeItem('user');
    expect((await page.localStorage.items()).map((i) => i.name)).toEqual(['token']);

    await page.localStorage.clear();
    expect((await page.localStorage.items()).length).toBe(0);
  });

  test('script_markdown', async ({ page }) => {
    await page.goto(dataUrl('<h1>Title</h1><p>Hello world</p><ul><li>Item 1</li><li>Item 2</li></ul>'));
    const md = await page.markdown();
    expect(md.includes('# Title')).toBe(true);
    expect(md.includes('Hello world')).toBe(true);
    expect(md.includes('- Item')).toBe(true);
  });

  test('script_markdown_links', async ({ page }) => {
    await page.goto(dataUrl("<p>Visit <a href='https://example.com'>Example</a></p>"));
    const md = await page.markdown();
    expect(md.includes('[Example](https://example.com)')).toBe(true);
  });
});
