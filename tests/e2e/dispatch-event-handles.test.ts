// `dispatchEvent`'s `eventInit` is an evaluation argument: a JSHandle in
// it reaches the page as the live object it names, as Playwright's
// `serializeArgument(eventInit)` sends it
// (/tmp/playwright/packages/playwright-core/src/client/frame.ts). Each
// assertion reads the object back off the event the page received, which
// only the live handle can produce.

import { test, describe, expect } from '@ferridriver/test';

const PAGE = `
  <div id="target">target</div>
  <div id="other">other</div>
  <script>
    window.seen = [];
    document.getElementById('target').addEventListener('mouseover', (event) => {
      window.seen.push({ type: event.type, related: event.relatedTarget && event.relatedTarget.id });
    });
  </script>
`;

describe('dispatchEvent with a handle in eventInit', () => {
  test('locator.dispatchEvent passes an element handle as relatedTarget', async ({ page }) => {
    await page.setContent(PAGE);
    const other = await page.locator('#other').elementHandle();
    await page.locator('#target').dispatchEvent('mouseover', { relatedTarget: other });
    expect(await page.evaluate(() => (window as any).seen)).toEqual([{ type: 'mouseover', related: 'other' }]);
  });

  test('page.dispatchEvent passes a JSHandle as relatedTarget', async ({ page }) => {
    await page.setContent(PAGE);
    const other = await page.evaluateHandle(() => document.getElementById('other'));
    await page.dispatchEvent('#target', 'mouseover', { relatedTarget: other });
    expect(await page.evaluate(() => (window as any).seen)).toEqual([{ type: 'mouseover', related: 'other' }]);
  });

  test('frame.dispatchEvent and elementHandle.dispatchEvent take the same argument', async ({ page }) => {
    await page.setContent(PAGE);
    const other = await page.locator('#other').elementHandle();
    await page.mainFrame().dispatchEvent('#target', 'mouseover', { relatedTarget: other });
    const target = await page.locator('#target').elementHandle();
    await target!.dispatchEvent('mouseover', { relatedTarget: other });
    expect(await page.evaluate(() => (window as any).seen)).toEqual([
      { type: 'mouseover', related: 'other' },
      { type: 'mouseover', related: 'other' },
    ]);
  });

  test('a plain eventInit still merges over the bubbling defaults', async ({ page }) => {
    await page.setContent(PAGE);
    const seen = page.evaluate(
      () =>
        new Promise((resolve) =>
          document.body.addEventListener('custom', (event) => resolve({ bubbles: event.bubbles, cancelable: event.cancelable }), {
            once: true,
          }),
        ),
    );
    await page.locator('#target').dispatchEvent('custom', { cancelable: false });
    expect(await seen).toEqual({ bubbles: true, cancelable: false });
  });
});
