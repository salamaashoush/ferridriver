// Cookie methods, timing and security details on the request context,
// from Playwright's `client/fetch.ts` (`cookies` / `addCookies` /
// `clearCookies`, `APIResponse.timing` / `securityDetails`) and
// `server/fetch.ts` (`GlobalAPIRequestContext`, which owns a cookie
// store, and `BrowserContextAPIRequestContext`, which is the context's).
import { test, describe, expect } from '@ferridriver/test';
import type { APIRequestContext } from '@ferridriver/test';

const ORIGIN = 'http://127.0.0.1:47831';
const OTHER = 'http://localhost:47831';

const sent = async (request: APIRequestContext, url: string) =>
  String(((await (await request.get(url)).json()) as Record<string, string>).cookie ?? '');

const names = async (request: APIRequestContext, urls?: string | string[]) =>
  (await request.cookies(urls)).map(c => c.name).sort();

describe('standalone request cookies', () => {
  test('a cookie added by url is stored with what the url stands for, and sent', async ({ request }) => {
    await request.clearCookies();
    await request.addCookies([{ name: 'token', value: 'abc', url: `${ORIGIN}/fx/landed` }]);
    expect(await request.cookies()).toEqual([
      { name: 'token', value: 'abc', domain: '127.0.0.1', path: '/fx/', secure: false, httpOnly: false, expires: -1, sameSite: 'Lax' },
    ]);
    expect(await sent(request, `${ORIGIN}/fx/echo-headers`)).toBe('token=abc');
    // Another origin's requests do not carry it.
    expect(await sent(request, `${OTHER}/fx/echo-headers`)).toBe('');
  });

  test('cookies(urls) keeps what a request to one of them would carry', async ({ request }) => {
    await request.clearCookies();
    await request.addCookies([
      { name: 'everywhere', value: '1', domain: '127.0.0.1', path: '/' },
      { name: 'scoped', value: '1', domain: '127.0.0.1', path: '/fx/only' },
      { name: 'elsewhere', value: '1', domain: 'example.com', path: '/' },
      { name: 'locked', value: '1', domain: 'example.com', path: '/', secure: true },
    ]);
    expect(await names(request)).toEqual(['elsewhere', 'everywhere', 'locked', 'scoped']);
    expect(await names(request, `${ORIGIN}/fx/landed`)).toEqual(['everywhere']);
    expect(await names(request, `${ORIGIN}/fx/only/page`)).toEqual(['everywhere', 'scoped']);
    expect(await names(request, 'http://example.com/')).toEqual(['elsewhere']);
    expect(await names(request, ['https://example.com/', `${ORIGIN}/`])).toEqual(['elsewhere', 'everywhere', 'locked']);
    expect(await names(request, 'https://acme.test/')).toEqual([]);
  });

  test('a response cookie is listed, and clearCookies takes a filter', async ({ request }) => {
    await request.clearCookies();
    await request.get(`${ORIGIN}/fx/http-client/set`);
    await request.addCookies([
      { name: 'keep', value: '1', domain: '127.0.0.1', path: '/' },
      { name: 'other', value: '1', domain: 'example.com', path: '/' },
    ]);
    expect(await names(request)).toEqual(['keep', 'other', 'sid']);
    expect((await request.cookies()).find(c => c.name === 'sid')?.value).toBe('abc');

    await request.clearCookies({ name: 'sid' });
    expect(await names(request)).toEqual(['keep', 'other']);
    expect(await sent(request, `${ORIGIN}/fx/echo-headers`)).toBe('keep=1');
    await request.clearCookies({ domain: /example\./ });
    expect(await names(request)).toEqual(['keep']);
    await request.clearCookies();
    expect(await request.cookies()).toEqual([]);
    expect(await sent(request, `${ORIGIN}/fx/echo-headers`)).toBe('');
  });

  test('a cookie needs a url or a domain and path', async ({ request }) => {
    const message = (work: Promise<unknown>) => work.then(() => 'accepted', (e: Error) => e.message);
    expect(await message(request.addCookies([{ name: 'a', value: '1' }]))).toContain(
      'Cookie should have a url or a domain/path pair',
    );
    expect(await message(request.addCookies([{ name: 'a', value: '1', url: ORIGIN, domain: '127.0.0.1' }]))).toContain(
      'Cookie should have either url or domain',
    );
    expect(await message(request.cookies('not a url'))).toContain('Invalid URL');
  });
});

describe('context request cookies', () => {
  test('the request context reads and writes the browser context cookies', async ({ page, context }) => {
    await page.goto(`${ORIGIN}/fx/landed`);
    await context.request.addCookies([{ name: 'viaRequest', value: 'yes', url: `${ORIGIN}/fx/landed` }]);
    expect(String(await page.evaluate('document.cookie'))).toContain('viaRequest=yes');
    // The url stands for its host and the directory of its path.
    const stored = (await context.cookies()).find(c => c.name === 'viaRequest')!;
    expect([stored.domain, stored.path, stored.secure]).toEqual(['127.0.0.1', '/fx/', false]);

    await page.evaluate("document.cookie = 'viaPage=yes; path=/'");
    expect(await names(context.request)).toEqual(['viaPage', 'viaRequest']);
    expect(await names(page.request, `${OTHER}/`)).toEqual([]);

    await context.request.clearCookies({ name: 'viaRequest' });
    expect((await context.cookies()).map(c => c.name)).toEqual(['viaPage']);
    await context.request.clearCookies();
    expect(await context.cookies()).toEqual([]);
  });

  test('context.addCookies resolves a url to its domain and path', async ({ page, context }) => {
    await page.goto(`${ORIGIN}/fx/landed`);
    await context.addCookies([
      { name: 'dir', value: '1', url: `${ORIGIN}/fx/only/page.html` },
      { name: 'root', value: '1', url: ORIGIN },
    ]);
    const stored = Object.fromEntries((await context.cookies()).map(c => [c.name, [c.domain, c.path]]));
    expect(stored).toEqual({ dir: ['127.0.0.1', '/fx/only/'], root: ['127.0.0.1', '/'] });
    expect(String(await page.evaluate('document.cookie'))).toBe('root=1');

    const message = (work: Promise<unknown>) => work.then(() => 'accepted', (e: Error) => e.message);
    expect(await message(context.addCookies([{ name: 'a', value: '1', url: ORIGIN, domain: '127.0.0.1' }]))).toContain(
      'Cookie should have either url or domain',
    );
    expect(await message(context.addCookies([{ name: 'a', value: '1', url: 'about:blank' }]))).toContain(
      'Blank page can not have cookie "a"',
    );
  });

  test('context.cookies(urls) filters as the request context does', async ({ page, context }) => {
    await page.goto(`${ORIGIN}/fx/landed`);
    await context.addCookies([
      { name: 'here', value: '1', domain: '127.0.0.1', path: '/' },
      { name: 'there', value: '1', domain: 'localhost', path: '/' },
      { name: 'deep', value: '1', domain: '127.0.0.1', path: '/fx/only' },
    ]);
    const of = async (urls?: string | string[]) => (await context.cookies(urls)).map(c => c.name).sort();
    expect(await of()).toEqual(['deep', 'here', 'there']);
    expect(await of(`${ORIGIN}/fx/landed`)).toEqual(['here']);
    expect(await of(`${ORIGIN}/fx/only/x`)).toEqual(['deep', 'here']);
    expect(await of([`${OTHER}/`, `${ORIGIN}/`])).toEqual(['here', 'there']);
  });
});

describe('response timing and security details', () => {
  test('timing reports the phases of the request', async ({ request }) => {
    const before = Date.now();
    const response = await request.get(`${ORIGIN}/fx/slow?ms=40`);
    const after = Date.now();
    const t = response.timing();
    expect(t.startTime).toBeGreaterThanOrEqual(before);
    expect(t.startTime).toBeLessThanOrEqual(after);
    expect(t.responseStart).toBeGreaterThan(0);
    expect(t.responseEnd).toBeGreaterThanOrEqual(t.responseStart);
    expect(t.startTime + t.responseEnd).toBeLessThanOrEqual(after + 1);
    expect(t.requestStart).toBeGreaterThanOrEqual(0);
    expect(t.requestStart).toBeLessThanOrEqual(t.responseStart);
    expect(t.secureConnectionStart).toBe(-1);
    // An address literal is never looked up.
    expect([t.domainLookupStart, t.domainLookupEnd]).toEqual([-1, -1]);
    // A connection is either made for this request or was already there.
    if (t.connectEnd === -1) {
      expect([t.connectStart, t.requestStart]).toEqual([-1, 0]);
    } else {
      expect(t.connectStart).toBe(0);
      expect(t.requestStart).toBe(t.connectEnd);
    }
  });

  test('a page response reports its certificate and server address', async ({ browser, browserName, request, baseURL }) => {
    const { tls } = (await (await request.get(`${baseURL}/fx/endpoints`)).json()) as { tls: string };
    const context = await browser.newContext({ ignoreHTTPSErrors: true });
    try {
      const page = await context.newPage();
      const response = (await page.goto(`${tls}/secure`))!;
      // What each browser's protocol reports, as Playwright reports it:
      // WebDriver BiDi carries no certificate and WebKit no issuer.
      const details = await response.securityDetails();
      if (browserName === 'firefox') {
        expect(details).toBeNull();
      } else {
        expect(details!.subjectName).toBe('rcgen self signed cert');
        expect(details!.issuer).toBe(browserName === 'webkit' ? undefined : 'rcgen self signed cert');
        const now = Date.now() / 1000;
        expect(details!.validFrom!).toBeLessThan(now);
        expect(details!.validTo!).toBeGreaterThan(now);
        expect(Object.keys(details!).every(key => !key.includes('_'))).toBe(true);
      }
      // WebDriver BiDi reports no server address, as in Playwright.
      const address = await response.serverAddr();
      if (browserName === 'firefox') {
        expect(address).toBeNull();
      } else {
        expect(address!.ipAddress).toBe('127.0.0.1');
        expect(address!.port).toBe(Number(new URL(tls).port));
      }
      expect(await response.finished()).toBeNull();
      expect(response.fromServiceWorker()).toBe(false);
      const plain = (await page.goto(`${ORIGIN}/fx/landed`))!;
      expect(await plain.securityDetails()).toBeNull();
    } finally {
      await context.close();
    }
    // Without the option the same certificate is refused.
    const strict = await browser.newContext();
    try {
      const refused = await (await strict.newPage()).goto(`${tls}/secure`).then(() => 'loaded', () => 'refused');
      expect(refused).toBe('refused');
    } finally {
      await strict.close();
    }
  });

  test('security details describe the server certificate', async ({ request, baseURL }) => {
    const { tls } = (await (await request.get(`${baseURL}/fx/endpoints`)).json()) as { tls: string };
    const secure = await request.get(`${tls}/secure`, { ignoreHTTPSErrors: true });
    const details = (await secure.securityDetails())!;
    expect(details.subjectName).toBe('rcgen self signed cert');
    expect(details.issuer).toBe('rcgen self signed cert');
    const now = Date.now() / 1000;
    expect(details.validFrom!).toBeLessThan(now);
    expect(details.validTo!).toBeGreaterThan(now);
    // A TLS connection is new to this request, so its phases are known.
    const t = secure.timing();
    expect(t.connectEnd).toBeGreaterThan(0);
    expect(t.requestStart).toBe(t.connectEnd);

    const plain = await request.get(`${ORIGIN}/fx/landed`);
    expect(await plain.securityDetails()).toBeNull();
  });
});

describe('challenge after a redirect', () => {
  test('a challenge from another origin gets that origin its own credentials', async ({ browser }) => {
    const context = await browser.newContext({
      httpCredentials: [
        { username: 'first', password: 'one', origin: ORIGIN },
        { username: 'second', password: 'two', origin: OTHER },
      ],
    });
    try {
      const target = encodeURIComponent(`${OTHER}/fx/auth?expect=second:two`);
      const redirected = await context.request.get(`${ORIGIN}/fx/set-cookie-redirect?loc=${target}`);
      expect(redirected.url()).toBe(`${OTHER}/fx/auth?expect=second:two`);
      expect([redirected.status(), await redirected.text()]).toEqual([200, 'AUTHED']);

      // The first origin's credentials are never offered to the second.
      const wrong = encodeURIComponent(`${OTHER}/fx/auth?expect=first:one`);
      const refused = await context.request.get(`${ORIGIN}/fx/set-cookie-redirect?loc=${wrong}`);
      expect(refused.status()).toBe(401);
    } finally {
      await context.close();
    }
  });
});
