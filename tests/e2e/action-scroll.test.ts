// Ported from Playwright's tests/page/page-click-scroll.spec.ts: the
// `scroll: 'none'` action option leaves the page where it is.
import { test, expect } from '@ferridriver/test';

test('does not scroll when scroll is "none"', async ({ page }) => {
  await page.setContent(`
    <div style="height: 2000px;"></div>
    <button onclick="window._clicked=true">click me</button>
  `);
  const error = await page.locator('button').click({ scroll: 'none', timeout: 2000 }).catch((e: Error) => e) as Error;
  expect(error.message).toContain('element is outside of the viewport');
  expect(await page.evaluate('window._clicked')).toBeFalsy();
  expect(await page.evaluate(() => window.scrollY)).toBe(0);
});

test('clicks an in-viewport element when scroll is "none"', async ({ page }) => {
  await page.setContent(`
    <button onclick="window._clicked=true">click me</button>
    <div style="height: 2000px;"></div>
  `);
  await page.locator('button').click({ scroll: 'none', timeout: 2000 });
  expect(await page.evaluate('window._clicked')).toBe(true);
  expect(await page.evaluate(() => window.scrollY)).toBe(0);
});

test('does not scroll a nested container when scroll is "none"', async ({ page }) => {
  await page.setContent(`
    <div style="height: 100px; width: 100px; overflow-y: scroll;">
      <div style="height: 50px;">A</div>
      <div style="height: 50px;">B</div>
      <button style="height: 50px; width: 100px;" onclick="window._clicked=true">C</button>
    </div>
  `);
  const button = page.locator('button');
  const error = await button.click({ scroll: 'none', timeout: 2000 }).catch((e: Error) => e);
  expect(error).toBeTruthy();
  expect(await page.evaluate('window._clicked')).toBeFalsy();
  await button.click({ timeout: 2000 });
  expect(await page.evaluate('window._clicked')).toBe(true);
});

test('does not scroll on hover when scroll is "none"', async ({ page }) => {
  await page.setContent(`
    <div style="height: 2000px;"></div>
    <div onmouseover="window._hovered=true" style="width: 50px; height: 50px;">hover me</div>
  `);
  const error = await page.locator('div >> text=hover me').hover({ scroll: 'none', timeout: 2000 }).catch((e: Error) => e) as Error;
  expect(error.message).toContain('element is outside of the viewport');
  expect(await page.evaluate('window._hovered')).toBeFalsy();
  expect(await page.evaluate(() => window.scrollY)).toBe(0);
});

test('rejects an unknown scroll mode', async ({ page }) => {
  await page.setContent('<button>b</button>');
  const error = await page.locator('button').click({ scroll: 'sideways' as 'none' }).catch((e: Error) => e) as Error;
  expect(String(error)).toContain('sideways');
});
