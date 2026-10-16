// page.requests(): the last 100 requests, kept across navigations.
// Ported from Playwright's page-network-request.spec.ts ("page.requests").
import { test, expect } from '@ferridriver/test';

const ORIGIN = 'http://127.0.0.1:47831';

test('lists the requests of the page across navigations', async ({ page }) => {
  await page.goto(`${ORIGIN}/empty.html`);
  await page.evaluate(() => fetch('/fx/landed?first'));
  await page.goto(`${ORIGIN}/fx/landed`);
  await page.evaluate(() => fetch('/fx/landed?second'));
  const listed = async () => (await page.requests()).map(request => request.url());
  await expect.poll(listed).toContain(`${ORIGIN}/fx/landed?second`);
  const urls = await listed();
  expect(urls).toContain(`${ORIGIN}/empty.html`);
  expect(urls).toContain(`${ORIGIN}/fx/landed?first`);
  expect(urls).toContain(`${ORIGIN}/fx/landed`);
  expect(urls.indexOf(`${ORIGIN}/fx/landed?first`)).toBeLessThan(urls.indexOf(`${ORIGIN}/fx/landed?second`));
  const second = (await page.requests()).find(request => request.url().endsWith('?second'))!;
  expect(second.frame()!.page()).toBe(page);
  expect((await second.response())!.status()).toBe(200);
});

test('keeps only the last 100', async ({ page }) => {
  await page.goto(`${ORIGIN}/empty.html`);
  await page.evaluate(async () => {
    for (let i = 0; i < 120; i++) await fetch(`/fx/landed?n=${i}`);
  });
  const listed = async () => (await page.requests()).map(request => request.url());
  await expect.poll(listed).toContain(`${ORIGIN}/fx/landed?n=119`);
  const urls = await listed();
  expect(urls.length).toBe(100);
  expect(urls).not.toContain(`${ORIGIN}/fx/landed?n=0`);
});
