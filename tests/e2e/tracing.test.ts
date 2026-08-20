// Ported from crates/ferridriver-cli/tests/backends_support/
// tracing_har.rs (plain-HAR half) — context.tracing.startHar/stopHar
// and the routeFromHAR update-mode recorders. Test titles mirror the
// original Rust fn names. The zip-packed HAR roundtrip and the v8
// trace-format validation stay in the Rust harness: their payloads are
// DEFLATE-compressed zip entries the QuickJS sandbox cannot inflate.

import { test, describe, expect } from '@ferridriver/test';

interface HarCookie {
  name: string;
  value: string;
}
interface HarFile {
  log: {
    entries: Array<{
      request: { url: string; cookies: HarCookie[] };
      response: { status: number; httpVersion: string; cookies: HarCookie[] };
      pageref?: string;
      timings: Record<string, number>;
    }>;
    pages?: Array<{ id: string; title: string }>;
  };
}

describe('tracing har', () => {
  test('tracing_start_har', async ({ page, context, baseURL }) => {
    // startHar records the context's network into a HAR written by
    // stopHar; the navigated URL + a 200 response land in log.entries.
    const harPath = test.info().outputPath('recorded.har');
    await context.tracing.startHar(harPath);
    await page.goto('/fx/landed');
    await page.goto('/fx/landed?second');
    await context.tracing.stopHar();
    const har = JSON.parse(await fs.promises.readFile(harPath, 'utf8')) as HarFile;
    const origin = new URL(baseURL!).host;
    expect(har.log.entries.some((e) => e.request.url.includes(origin))).toBe(true);
    expect(har.log.entries.some((e) => e.response.status === 200)).toBe(true);
    // Enriched fields via the plain-HAR write path: every entry carries a
    // (possibly empty) cookies array, an httpVersion, and a timings object
    // — all backend-agnostic.
    for (const e of har.log.entries) {
      expect(Array.isArray(e.request.cookies)).toBe(true);
      expect(Array.isArray(e.response.cookies)).toBe(true);
      expect(typeof e.response.httpVersion).toBe('string');
      expect(typeof e.timings.wait).toBe('number');
    }
  });

  test('har_records_cookies_title_and_server_fields', async ({ page, context, baseURL }) => {
    // A Set-Cookie response lands in response.cookies; the page's <title>
    // lands in log.pages[].title. Runs on every backend project.
    const harPath = test.info().outputPath('enriched.har');
    await context.tracing.startHar(harPath);
    // Record the Set-Cookie response first, then leave the page on a
    // titled document — the title is snapshotted from the live page at
    // flush, so the final document's title is what lands in log.pages.
    await page.goto(`${baseURL}/fx/set-cookie?c=${encodeURIComponent('e2ehar=set; Path=/')}`);
    await page.setContent('<!doctype html><title>HAR Title E2E</title><body>x</body>');
    await context.tracing.stopHar();
    const har = JSON.parse(await fs.promises.readFile(harPath, 'utf8')) as HarFile;
    const setCookieEntry = har.log.entries.find((e) => e.request.url.includes('/fx/set-cookie'));
    expect(setCookieEntry?.response.cookies.some((c) => c.name === 'e2ehar' && c.value === 'set')).toBe(true);
    // The settled page title is captured at flush.
    expect(har.log.pages?.some((p) => p.title === 'HAR Title E2E')).toBe(true);
  });

  test('route_from_har_update_records_on_close', async ({ browser, baseURL }) => {
    // routeFromHAR(path, { update: true }) records instead of replaying;
    // the HAR is written when the context closes.
    const harPath = test.info().outputPath('updated.har');
    const ctx = await browser.newContext({});
    const p = await ctx.newPage();
    await ctx.routeFromHAR(harPath, { update: true, updateContent: 'embed' });
    await p.goto(`${baseURL}/fx/landed`);
    await ctx.close();
    const har = JSON.parse(await fs.promises.readFile(harPath, 'utf8')) as HarFile;
    const origin = new URL(baseURL!).host;
    expect(har.log.entries.some((e) => e.request.url.includes(origin))).toBe(true);
  });

  test('page_route_from_har_update_scopes_to_page', async ({ browser, baseURL }) => {
    // page.routeFromHAR(path, { update: true }) records only THAT
    // page's traffic (HarTracer page filter): the log carries a pages
    // section and every entry a matching pageref.
    const harPath = test.info().outputPath('page-scoped.har');
    const ctx = await browser.newContext({});
    const a = await ctx.newPage();
    const b = await ctx.newPage();
    await a.routeFromHAR(harPath, { update: true, updateContent: 'embed' });
    await a.goto(`${baseURL}/fx/landed`);
    await b.goto(`${baseURL}/fx/api/users`);
    await ctx.close();
    const har = JSON.parse(await fs.promises.readFile(harPath, 'utf8')) as HarFile;
    expect(har.log.entries.some((e) => e.request.url.endsWith('/fx/landed'))).toBe(true);
    expect(har.log.entries.some((e) => e.request.url.endsWith('/fx/api/users'))).toBe(false);
    expect(har.log.pages?.length).toBe(1);
    const pageId = har.log.pages![0].id;
    expect(har.log.entries.every((e) => e.pageref === pageId)).toBe(true);
  });
});
