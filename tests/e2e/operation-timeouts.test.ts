import { test, expect } from '@ferridriver/test';

// An operation's timeout bounds every protocol command it sends, the way
// Playwright races each call against its progress deadline. A command has
// no shorter deadline of its own: Chromium answers Page.navigate only once
// the response headers arrive, so a server that holds them past 30s is a
// navigation a 60s goto must still finish.
test('a goto outlasts a server that answers after 35 seconds', async ({ page }) => {
  test.setTimeout(90_000);
  const response = await page.goto('/fx/slow?ms=35000', { timeout: 60_000, waitUntil: 'commit' });
  expect(await response!.text()).toBe('slow');
});

test('a goto without a timeout waits for the server as long as it takes', async ({ page }) => {
  test.setTimeout(90_000);
  const response = await page.goto('/fx/slow?ms=35000', { timeout: 0 });
  expect(await response!.text()).toBe('slow');
});

test('a goto fails on its own timeout while the server holds the response', async ({ page }) => {
  const started = Date.now();
  const error = await page.goto('/fx/slow?ms=20000', { timeout: 1000 }).catch((e: Error) => e);
  expect(String(error)).toContain('Timeout 1000ms exceeded');
  expect(Date.now() - started).toBeLessThan(5000);
});

test('reload and history steps with no timeout wait for their load', async ({ page, baseURL }) => {
  await page.goto('/fx/landed');
  await page.goto('/fx/slow?ms=100');
  expect((await page.reload({ timeout: 0 }))!.ok()).toBe(true);
  await page.goBack({ timeout: 0 });
  expect(page.url()).toBe(`${baseURL}/fx/landed`);
  await page.goForward({ timeout: 0 });
  expect(page.url()).toBe(`${baseURL}/fx/slow?ms=100`);
});

test('waitForURL with no timeout waits for a URL that comes later', async ({ page }) => {
  await page.goto('/fx/landed');
  await page.evaluate(() => {
    setTimeout(() => history.pushState({}, '', '/later'), 300);
  });
  await page.waitForURL('**/later', { timeout: 0 });
  expect(page.url()).toMatch(/\/later$/);
});

test('waitForNavigation with no timeout waits for a navigation that comes later', async ({ page }) => {
  await page.goto('/fx/landed');
  const navigation = page.waitForNavigation({ timeout: 0 });
  await page.evaluate(() => {
    setTimeout(() => {
      location.href = '/fx/slow?ms=100';
    }, 300);
  });
  await navigation;
  expect(page.url()).toMatch(/\/fx\/slow\?ms=100$/);
});
