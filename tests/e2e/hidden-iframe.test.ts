// An element inside an iframe that is itself hidden is hidden: Playwright
// 1.64 threads the frame's own visibility through every visibility check.

import { test, expect } from '@ferridriver/test';
import { setBody } from './helpers/html';

const frame = (style: string) =>
  `<iframe style="${style}" srcdoc="<button id=b>inside</button>"></iframe>`;

test('an element in a hidden iframe is hidden, and visible once the iframe shows', async ({ page }) => {
  await setBody(page, frame('visibility:hidden'));
  const button = page.frameLocator('iframe').locator('#b');
  await expect(button).toBeAttached();
  expect(await button.isVisible()).toBe(false);
  await expect(button).toBeHidden();
  const clicked = await button.click({ timeout: 500 }).then(() => 'clicked', e => String(e));
  expect(clicked).toContain('500ms');
  await page.evaluate(() => (document.querySelector('iframe')!.style.visibility = 'visible'));
  await expect(button).toBeVisible();
  await button.click();
});

test('an iframe nested inside a hidden one hides its content too', async ({ page }) => {
  await setBody(
    page,
    `<iframe style="display:block" srcdoc="${frame('display:none').replace(/"/g, '&quot;')}"></iframe>`,
  );
  const button = page.frameLocator('iframe').frameLocator('iframe').locator('#b');
  await expect(button).toBeAttached();
  await expect(button).toBeHidden();
});
