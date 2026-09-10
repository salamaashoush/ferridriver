import { test, expect } from '@ferridriver/test';

test('snapshots', async ({ page }) => {
  await page.goto('data:text/html,<title>Snap</title><main><h1>Header</h1><button id=b>Save</button></main>');
  await expect(page.locator('h1')).toMatchSnapshot('header-text');
  await expect('literal value').toMatchSnapshot('literal');
  await expect(page.locator('main')).toHaveScreenshot('main-shot');
  await expect(page).toHaveScreenshot('page-shot', { maxDiffPixels: 25 });
  await expect(page.locator('main')).toMatchAriaSnapshot(`
    - heading "Header" [level=1]
    - button "Save"
  `);
});
