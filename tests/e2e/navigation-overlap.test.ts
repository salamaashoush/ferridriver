// A navigation started while another is still in flight cancels the
// older one. That cancellation, and the requests of the older document,
// belong to the navigation that was cancelled and not the new one.
import { test, expect } from '@ferridriver/test';

const ORIGIN = 'http://127.0.0.1:47831';
const HOST = `${ORIGIN}/frames/one-frame.html`;

test('goto lands on its own document while another navigation is pending', async ({ page }) => {
  for (let round = 0; round < 5; round++) {
    await page.goto(`${ORIGIN}/empty.html`);
    await page.evaluate((url: string) => { location.href = url; }, `${ORIGIN}/fx/slow?ms=300`);
    const response = await page.goto(HOST);
    expect(response!.url()).toBe(HOST);
    expect(response!.status()).toBe(200);
    expect(page.url()).toBe(HOST);
    expect(await page.frame('child')!.title()).toBe('Inner Frame');
  }
});

test('goto lands on its own document while the last one is still loading', async ({ page }) => {
  for (let round = 0; round < 3; round++) {
    await page.goto(`${ORIGIN}/empty.html`);
    const key = `overlap-${round}-${Date.now()}`;
    await Promise.all([
      page.waitForNavigation({ waitUntil: 'commit' }),
      page.evaluate((url: string) => { location.href = url; }, `${ORIGIN}/fx/control/page/${key}`),
    ]);
    const release = page.request.get(`${ORIGIN}/fx/control/release/${key}`);
    const response = await page.goto(HOST);
    await release;
    expect(response!.url()).toBe(HOST);
    expect(page.url()).toBe(HOST);
    expect(await page.frame('child')!.title()).toBe('Inner Frame');
  }
});

test('reload and history answer with the page document, not a frame inside it', async ({ page }) => {
  await page.goto(`${ORIGIN}/empty.html`);
  await page.goto(HOST);
  expect((await page.reload())!.url()).toBe(HOST);
  expect((await page.goBack())!.url()).toBe(`${ORIGIN}/empty.html`);
  expect((await page.goForward())!.url()).toBe(HOST);
});
