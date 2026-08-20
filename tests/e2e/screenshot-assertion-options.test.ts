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

    const written = await fs.promises.readFile(testInfo.snapshotPath('full.png', { kind: 'screenshot' }));
    expect(Array.from(written.slice(0, 4))).toEqual([0x89, 0x50, 0x4e, 0x47]);
    // A viewport-only capture would be 300 device pixels tall at most;
    // the document is 1200 CSS pixels.
    expect(u32(written, 20) > 400).toBe(true);
  });

  test('a mask takes a Locator, not just a CSS string', async ({ page }) => {
    // A Locator can carry any engine. `getByRole` is not a CSS selector,
    // so a mask resolved with `document.querySelectorAll` matched nothing
    // and the capture came back unmasked with no error.
    await page.setContent(
      '<style>body{margin:0;background:#fff}button{width:90px;height:40px;background:#123456;border:0;color:#fff}</style>' +
        '<button>secret</button>',
    );
    const target = page.getByRole('button', { name: 'secret' });

    // Control: the change IS visible unmasked.
    const before = await page.screenshot();
    await page.evaluate(() => {
      (document.querySelector('button') as HTMLElement).style.background = '#abcdef';
    });
    const after = await page.screenshot();
    expect(Array.from(before).join(',') === Array.from(after).join(',')).toBe(false);

    await expect(page).toHaveScreenshot('role-masked.png', { mask: [target], animations: 'disabled' });
    await page.evaluate(() => {
      (document.querySelector('button') as HTMLElement).style.background = '#00ff00';
    });
    await expect(page).toHaveScreenshot('role-masked.png', { mask: [target], animations: 'disabled' });
  });

  test('a mask covers a masked element CHILD, not just its own background', async ({ page }) => {
    // The overlay is painted over the element's box. A CSS
    // `background`/`color` rule on the element alone leaves a child with
    // its own background painting straight through the mask.
    await page.setContent(
      '<style>body{margin:0;background:#fff}#box{width:100px;height:100px;background:#111}' +
        '#kid{width:60px;height:60px;background:#00ff00}</style>' +
        '<div id="box"><div id="kid"></div></div>',
    );

    await expect(page).toHaveScreenshot('child-masked.png', { mask: [page.locator('#box')], animations: 'disabled' });
    await page.evaluate(() => {
      (document.getElementById('kid') as HTMLElement).style.background = '#ff0000';
    });
    await expect(page).toHaveScreenshot('child-masked.png', { mask: [page.locator('#box')], animations: 'disabled' });
  });

  test('maskColor paints the mask, and a different colour is a different capture', async ({ page }, testInfo) => {
    await page.setContent(
      '<style>body{margin:0;background:#ffffff}#box{width:80px;height:80px;background:#123456}</style>' +
        '<div id="box"></div>',
    );

    await expect(page).toHaveScreenshot('colour.png', {
      mask: [page.locator('#box')],
      maskColor: '#00ff00',
      animations: 'disabled',
    });
    const green = await fs.promises.readFile(testInfo.snapshotPath('colour.png', { kind: 'screenshot' }));

    await expect(page).toHaveScreenshot('other-colour.png', {
      mask: [page.locator('#box')],
      maskColor: '#ff0000',
      animations: 'disabled',
    });
    const red = await fs.promises.readFile(testInfo.snapshotPath('other-colour.png', { kind: 'screenshot' }));

    // Same page, same mask, different colour: the option reached the paint.
    expect(Array.from(green).join(',') === Array.from(red).join(',')).toBe(false);
  });

  test('omitBackground keeps the capture transparent', async ({ page, browserName }, testInfo) => {
    // No page background at all, so with `omitBackground` the PNG has to
    // carry alpha: colour type 6 (truecolour + alpha) rather than 2.
    await page.setContent('<style>html,body{margin:0;background:transparent}</style><body></body>');

    if (browserName === 'firefox') {
      // No BiDi command exposes the transparent-background override, and
      // Playwright's own BiDi `setBackgroundColor` throws `Not
      // implemented` (bidiPage.ts:506). The refusal is the behaviour, so
      // assert it rather than skipping the case.
      let message = '';
      try {
        await expect(page).toHaveScreenshot('clear.png', { omitBackground: true, animations: 'disabled' });
      } catch (e) {
        message = String((e as Error).message ?? e);
      }
      expect(message.includes('omitBackground')).toBe(true);
      return;
    }

    await expect(page).toHaveScreenshot('clear.png', { omitBackground: true, animations: 'disabled' });

    const written = await fs.promises.readFile(testInfo.snapshotPath('clear.png', { kind: 'screenshot' }));
    // IHDR: width@16, height@20, bit depth@24, colour type@25.
    expect(written[25]).toBe(6);
  });

  test('timeout bounds the assertion', async ({ page }) => {
    await page.setContent('<div style="width:20px;height:20px;background:#654321"></div>');
    // A baseline that will never match: the assertion polls until its own
    // timeout, and `timeout` is what decides when it gives up.
    await expect(page).toHaveScreenshot('bounded.png', { animations: 'disabled' });
    await page.setContent('<div style="width:20px;height:20px;background:#111111"></div>');

    const started = Date.now();
    let failed = false;
    try {
      await expect(page).toHaveScreenshot('bounded.png', { timeout: 900, animations: 'disabled' });
    } catch {
      failed = true;
    }
    const elapsed = Date.now() - started;

    expect(failed).toBe(true);
    // The assertion default is 5s; giving up this early can only be the
    // 900ms that was asked for.
    expect(elapsed < 4000).toBe(true);
  });

  test('stylePath is applied to a page subject', async ({ page }, testInfo) => {
    const sheet = `${testInfo.outputDir}/mask.css`;
    await fs.promises.writeFile(sheet, '#box { visibility: hidden !important; }');
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
