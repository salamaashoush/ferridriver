// A target already inside the viewport but under a `position: sticky`
// header is not scrolled by the precise reveal, so every try would hit
// the header. Playwright's `_retryPointerAction`
// (/tmp/playwright/packages/playwright-core/src/server/dom.ts) counts its
// tries across the ones a covering element sends back and scrolls each
// with the next alignment (end, center, start), which uncovers it.

import { describe, expect, test } from '@ferridriver/test';

const PAGE = `
  <style>
    body { margin: 0; }
    header { position: sticky; top: 0; height: 120px; background: #333; z-index: 30; }
    .spacer { height: 1600px; }
  </style>
  <header>top bar</header>
  <div class="spacer"></div>
  <button id="target" onclick="window.clicked = (window.clicked || 0) + 1"
          onmouseenter="window.hovered = true">Add period</button>
  <div class="spacer"></div>
  <script>window.clicked = 0; window.hovered = false;</script>
`;

async function underTheHeader(page: import('@ferridriver/test').Page) {
  await page.setContent(PAGE);
  // Scroll so the button sits 40px from the top: in the viewport, and
  // under the 120px header.
  await page.evaluate(() => {
    const target = document.getElementById('target')!;
    window.scrollBy(0, target.getBoundingClientRect().top - 40);
  });
  const top = await page.evaluate(() => document.getElementById('target')!.getBoundingClientRect().top);
  expect(top).toBeLessThan(120);
}

describe('a pointer action under a sticky header', () => {
  test('click reveals the target with the next scroll alignment', async ({ page }) => {
    await underTheHeader(page);
    await page.getByRole('button', { name: 'Add period' }).click({ timeout: 5000 });
    expect(await page.evaluate(() => (window as any).clicked)).toBe(1);
  });

  test('hover reveals the target the same way', async ({ page }) => {
    await underTheHeader(page);
    await page.getByRole('button', { name: 'Add period' }).hover({ timeout: 5000 });
    expect(await page.evaluate(() => (window as any).hovered)).toBe(true);
  });
});
