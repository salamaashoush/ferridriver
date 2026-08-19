// Step definitions bound to the merged `test` chain, so a step body
// destructures the fixtures those chains contribute — the shape a
// playwright-bdd suite already has.

import { createBdd } from 'playwright-bdd';
import { expect } from '@ferridriver/test';
import { test } from '../fixtures';

const { Given, When, Then } = createBdd(test);

Given('the catalog page is open', async ({ page, apiOrigin }) => {
  // The document itself comes from the router, so the page's origin is
  // the mocked one and its own fetch is same-origin.
  await page.goto(`${apiOrigin}/`);
});

When('the catalog loads', async ({ page }) => {
  await page.evaluate(async () => {
    const res = await fetch('/catalog');
    const body = await res.json();
    document.getElementById('items')!.innerHTML = body.items
      .map((i: string) => `<li>${i}</li>`)
      .join('');
  });
});

Then('it shows {int} items', async ({ page }, count: number) => {
  await expect(page.locator('#items li')).toHaveCount(count);
});

Then('the router served {string}', async ({ api }, path: string) => {
  (expect(api) as any).toHaveServed(path);
});
