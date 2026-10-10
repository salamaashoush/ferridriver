// A fulfilled document's iframes load: their requests are intercepted
// with the page's, from the iframe's own browsing context, and still
// have to reach the page's handlers or continue.
import { test, expect } from '@ferridriver/test';

const ORIGIN = 'http://127.0.0.1:47831/route-iframe';

test('a routed iframe inside a fulfilled page reaches its handler', async ({ page }) => {
  await page.route('**/route-iframe/empty.html', r => r.fulfill({ body: '<iframe src="a.html"></iframe>', contentType: 'text/html' }));
  await page.route('**/route-iframe/a.html', r => r.fulfill({ body: '<p>from the handler</p>', contentType: 'text/html' }));
  await page.goto(`${ORIGIN}/empty.html`, { timeout: 5000 });
  await expect(page.frameLocator('iframe').locator('p')).toHaveText('from the handler');
});

test('an unrouted iframe inside a fulfilled page continues', async ({ page }) => {
  await page.route('**/route-iframe/empty.html', r => r.fulfill({ body: '<iframe src="/fx/landed"></iframe>', contentType: 'text/html' }));
  await page.goto(`${ORIGIN}/empty.html`, { timeout: 5000 });
  await expect.poll(() => page.frames().length).toBe(2);
  expect(page.frames()[1].url()).toContain('/fx/landed');
});
