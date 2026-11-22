// A wait on the main frame follows it to another site. WebKit moves a
// cross-site navigation into a new process, whose main frame comes with
// a new id; Playwright's main `Frame` stays itself through that, so a
// `waitForURL` armed before the move still sees where the page lands,
// here by `history.replaceState` once the new document has loaded.

import { describe, expect, test } from '@ferridriver/test';

describe('waitForURL across a cross-site move', () => {
  test('sees the URL the moved page settles on', async ({ page }) => {
    await page.route('**/fx/handoff', (route) =>
      route.fulfill({
        contentType: 'text/html',
        body: `<h1>handoff</h1><script>
          addEventListener('load', () => setTimeout(() => history.replaceState(null, '', '/fx/today'), 200));
        </script>`,
      }),
    );
    await page.goto('http://127.0.0.1:47831/fx/landed');
    await page.evaluate(() => setTimeout(() => (location.href = 'http://localhost:47831/fx/handoff#token'), 300));
    await page.waitForURL('http://localhost:47831/fx/today', { timeout: 10_000 });
    await expect(page.getByRole('heading', { level: 1 })).toHaveText('handoff');
  });
});

describe('a navigation with a fragment', () => {
  // Playwright's `stripFragmentFromUrl`: WebKit and BiDi report a
  // navigation's URL with its fragment, which no request carries.
  test('is routed, requested and answered without it', async ({ page }) => {
    const routed: string[] = [];
    await page.route('**/fx/landed', (route) => {
      routed.push(route.request().url());
      return route.continue();
    });
    const request = page.waitForRequest('**/fx/landed');
    const response = await page.goto('http://localhost:47831/fx/landed#part');
    expect(routed).toEqual(['http://localhost:47831/fx/landed']);
    expect((await request).url()).toBe('http://localhost:47831/fx/landed');
    expect(response?.url()).toBe('http://localhost:47831/fx/landed');
    expect(page.url()).toBe('http://localhost:47831/fx/landed#part');
  });
});
