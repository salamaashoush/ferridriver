// A route sees every request the page makes, including one it would
// otherwise answer from its in-memory cache of the document's resources.

import { test, expect } from '@ferridriver/test';

const ORIGIN = 'http://127.0.0.1:47831';

test('a route sees a stylesheet the page loaded before the route existed', async ({ page, browserName }) => {
  test.skip(browserName === 'firefox', 'Firefox reuses a stylesheet its document already parsed without a request, whatever the cache behaviour');
  await page.goto(`${ORIGIN}/empty.html`);
  const sheet = `${ORIGIN}/frames/tag.css`;
  const load = (href: string) =>
    page.evaluate(
      (href: string) =>
        new Promise<null>((done) => {
          const link = document.createElement('link');
          link.rel = 'stylesheet';
          link.href = href;
          link.onload = link.onerror = () => done(null);
          document.head.append(link);
        }),
      href,
    );
  await load(sheet);
  let routed = 0;
  await page.route('**/frames/tag.css', (route) => {
    routed += 1;
    return route.continue();
  });
  await load(sheet);
  expect(routed).toBe(1);
});

test('a context route sees every fetch of a response the browser may cache', async ({ page, context }) => {
  let routed = 0;
  await context.route('**/fx/cached*', (route) => {
    routed += 1;
    return route.continue();
  });
  await page.goto(`${ORIGIN}/empty.html`);
  for (let round = 1; round <= 3; round++) {
    expect(await page.evaluate(() => fetch('/fx/cached').then((response) => response.text()))).toBe('cached');
    expect(routed).toBe(round);
  }
});
