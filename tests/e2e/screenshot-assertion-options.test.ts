// `toHaveScreenshot`'s capture options, on a PAGE subject.
//
// The page half of the matcher used to call `page.screenshot()` bare, so
// animations, caret, masks, stylePath and clip were silently dropped
// there while the locator half honoured all of them. These assert the
// options through the page path.
//
// Baselines are NOT committed: the first call writes one and the second
// compares against it, the same contract snapshot-path.test.ts relies on.

import { test, describe, expect } from '@ferridriver/test';

/** Big-endian u32 at `at` — PNG stores width/height at bytes 16 and 20. */
function u32(bytes: number[] | Uint8Array, at: number): number {
  return (bytes[at] << 24) | (bytes[at + 1] << 16) | (bytes[at + 2] << 8) | bytes[at + 3];
}

describe('toHaveScreenshot capture options', () => {
  test('a mask covers the region it names on a page subject', async ({ page }) => {
    await page.setContent(
      '<style>body{margin:0;background:#ffffff}#box{width:80px;height:80px;background:#123456}</style>' +
        '<div id="box"></div>',
    );

    // Control: the change IS visible without a mask, so a passing masked
    // comparison below cannot be a no-op.
    const before = await page.screenshot();
    await page.evaluate(() => {
      (document.getElementById('box') as HTMLElement).style.background = '#abcdef';
    });
    const after = await page.screenshot();
    expect(Array.from(before).join(',') === Array.from(after).join(',')).toBe(false);

    // With the box masked, the same change leaves the capture identical:
    // the mask paints it a constant colour before the shutter.
    await expect(page).toHaveScreenshot('masked.png', { mask: ['#box'], animations: 'disabled' });
    await page.evaluate(() => {
      (document.getElementById('box') as HTMLElement).style.background = '#00ff00';
    });
    await expect(page).toHaveScreenshot('masked.png', { mask: ['#box'], animations: 'disabled' });
  });

  test('fullPage captures past the viewport', async ({ page }, testInfo) => {
    await page.setViewportSize({ width: 400, height: 300 });
    await page.setContent('<style>body{margin:0}</style><div style="height:1200px;background:#334455"></div>');

    await expect(page).toHaveScreenshot('full.png', { fullPage: true, animations: 'disabled' });

    const written = await fs.readFileBytes(testInfo.snapshotPath('full.png', { kind: 'screenshot' }));
    expect(Array.from(written.slice(0, 4))).toEqual([0x89, 0x50, 0x4e, 0x47]);
    // A viewport-only capture would be 300 device pixels tall at most;
    // the document is 1200 CSS pixels.
    expect(u32(written, 20) > 400).toBe(true);
  });

  test('stylePath is applied to a page subject', async ({ page }, testInfo) => {
    const sheet = `${testInfo.outputDir}/mask.css`;
    await fs.writeFile(sheet, '#box { visibility: hidden !important; }');
    await page.setContent(
      '<style>body{margin:0;background:#ffffff}#box{width:80px;height:80px;background:#123456}</style>' +
        '<div id="box"></div>',
    );

    // The stylesheet hides the only painted element, so the capture is a
    // blank page — and stays one when the element's colour changes.
    await expect(page).toHaveScreenshot('styled.png', { stylePath: sheet, animations: 'disabled' });
    await page.evaluate(() => {
      (document.getElementById('box') as HTMLElement).style.background = '#ff0000';
    });
    await expect(page).toHaveScreenshot('styled.png', { stylePath: sheet, animations: 'disabled' });
  });
});
