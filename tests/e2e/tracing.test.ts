// Ported from crates/ferridriver-cli/tests/backends_support/
// tracing_har.rs (plain-HAR half) — context.tracing.startHar/stopHar
// and the routeFromHAR update-mode recorders. Test titles mirror the
// original Rust fn names. The zip-packed HAR roundtrip and the v8
// trace-format validation stay in the Rust harness: their payloads are
// DEFLATE-compressed zip entries the QuickJS sandbox cannot inflate.

import { test, describe, expect } from '@ferridriver/test';

interface HarFile {
  log: {
    entries: Array<{ request: { url: string }; response: { status: number }; pageref?: string }>;
    pages?: Array<{ id: string }>;
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
    const har = JSON.parse(await fs.readFile(harPath)) as HarFile;
    const origin = new URL(baseURL!).host;
    expect(har.log.entries.some((e) => e.request.url.includes(origin))).toBe(true);
    expect(har.log.entries.some((e) => e.response.status === 200)).toBe(true);
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
    const har = JSON.parse(await fs.readFile(harPath)) as HarFile;
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
    const har = JSON.parse(await fs.readFile(harPath)) as HarFile;
    expect(har.log.entries.some((e) => e.request.url.endsWith('/fx/landed'))).toBe(true);
    expect(har.log.entries.some((e) => e.request.url.endsWith('/fx/api/users'))).toBe(false);
    expect(har.log.pages?.length).toBe(1);
    const pageId = har.log.pages![0].id;
    expect(har.log.entries.every((e) => e.pageref === pageId)).toBe(true);
  });
});
