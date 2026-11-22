// A goto the server redirects to another site lands there. WebKit swaps
// to a new web process for the other site, and the load it started in
// the old process ends as "Frame load interrupted"; that ending belongs
// to the navigation the swap continues, not a failure of it.

import { describe, expect, test } from '@ferridriver/test';

describe('a goto redirected to another site', () => {
  test('lands on the other site', async ({ page }) => {
    const response = await page.goto('http://127.0.0.1:47831/fx/http-client/hop-offhost');
    expect(page.url()).toBe('http://localhost:47831/fx/http-client/landed');
    expect(response?.status()).toBe(200);
    expect(await page.locator('body').textContent()).toBe('LANDED');
  });

  test('lands again from a page already on the first site', async ({ page }) => {
    await page.goto('http://127.0.0.1:47831/fx/landed');
    await page.goto('http://127.0.0.1:47831/fx/http-client/hop-offhost#keep');
    await expect(page).toHaveURL('http://localhost:47831/fx/http-client/landed#keep');
  });
});
