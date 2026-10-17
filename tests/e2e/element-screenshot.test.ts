// Element and clipped captures go through the same screenshotter as a
// page capture (Playwright's `screenshotter.ts`), so every option a page
// screenshot honours, an element screenshot honours too, and the rect
// math follows the viewport/document split upstream uses.

import { test, describe, expect } from '@ferridriver/test';
import type { Browser, Page } from '@ferridriver/test';
import { decodePng } from './helpers/png';

const RED = [255, 0, 0, 255];
const GREEN = [0, 255, 0, 255];
const BLUE = [0, 0, 255, 255];
const MAGENTA = [255, 0, 255, 255];

async function withPage(browser: Browser, body: (page: Page) => Promise<void>, deviceScaleFactor = 1) {
  const context = await browser.newContext({ viewport: { width: 300, height: 200 }, deviceScaleFactor });
  try {
    await body(await context.newPage());
  } finally {
    await context.close();
  }
}

const reset = '<style>html,body{margin:0;padding:0;background:#fff}</style>';

describe('element screenshot', () => {
  test('captures the element at its css size, or at device pixels by default', async ({ browser }) => {
    await withPage(browser, async page => {
      await page.setContent(`${reset}<div id="box" style="margin:10px;width:40px;height:30px;background:#f00"></div>`);
      // `browsingContext.captureScreenshot` has no scale parameter, and
      // Playwright's BiDi backend ignores the option; ferridriver
      // resamples the Firefox capture to CSS pixels instead.
      const cssSize = [40, 30];
      const css = decodePng(await page.locator('#box').screenshot({ scale: 'css' }));
      expect([css.width, css.height]).toEqual(cssSize);
      expect(css.pixel(20, 15)).toEqual(RED);
      const device = decodePng(await page.locator('#box').screenshot());
      expect([device.width, device.height]).toEqual([80, 60]);
      const handle = decodePng(await (await page.$('#box'))!.screenshot({ scale: 'css' }));
      expect([handle.width, handle.height]).toEqual(cssSize);
    }, 2);
  });

  test('masks, styles and pauses animations inside the element capture', async ({ browser }) => {
    await withPage(browser, async page => {
      await page.setContent(
        `${reset}<div id="box" style="width:60px;height:40px;background:#f00">` +
          '<div id="inner" style="width:20px;height:20px;background:#00f"></div>' +
          '<div id="fade" style="width:20px;height:10px;background:#0f0;animation:fade 100s linear forwards"></div>' +
          '</div><style>@keyframes fade{from{background:#00f}to{background:#0f0}}</style>',
      );
      const box = page.locator('#box');
      const masked = decodePng(
        await box.screenshot({ mask: [page.locator('#inner')], maskColor: '#ff00ff', scale: 'css', animations: 'disabled' }),
      );
      expect(masked.pixel(10, 10)).toEqual(MAGENTA);
      expect(masked.pixel(50, 10)).toEqual(RED);
      // A finite animation is finished, so it shows its final frame.
      expect(masked.pixel(10, 25)).toEqual(GREEN);
      const styled = decodePng(await box.screenshot({ style: '#box { background: #00f !important }', scale: 'css' }));
      expect(styled.pixel(50, 10)).toEqual(BLUE);
      const after = decodePng(await box.screenshot({ scale: 'css' }));
      expect(after.pixel(50, 10)).toEqual(RED);
      expect(after.pixel(10, 10)).toEqual(BLUE);
    });
  });

  test('scrolls a below-the-fold element into view before capturing it', async ({ browser }) => {
    await withPage(browser, async page => {
      await page.setContent(`${reset}<div style="height:1000px"></div><div id="low" style="width:30px;height:30px;background:#0f0"></div>`);
      const low = decodePng(await page.locator('#low').screenshot({ scale: 'css' }));
      expect([low.width, low.height]).toEqual([30, 30]);
      expect(low.pixel(15, 15)).toEqual(GREEN);
    });
  });

  test('captures and measures an element inside an iframe in main-frame coordinates', async ({ browser }) => {
    await withPage(browser, async page => {
      await page.setContent(
        `${reset}<iframe style="position:absolute;left:50px;top:40px;width:120px;height:100px;border:5px solid #000" ` +
          `srcdoc="<style>body{margin:0}</style><div id=target style='margin:10px;width:20px;height:20px;background:#00f'></div>"></iframe>`,
      );
      const target = page.frameLocator('iframe').locator('#target');
      await expect(target).toBeVisible();
      expect(await target.boundingBox()).toEqual({ x: 65, y: 55, width: 20, height: 20 });
      const shot = decodePng(await target.screenshot({ scale: 'css' }));
      expect([shot.width, shot.height]).toEqual([20, 20]);
      expect(shot.pixel(10, 10)).toEqual(BLUE);
    });
  });

  test('encodes jpeg and webp, with quality only for those', async ({ browser }) => {
    await withPage(browser, async page => {
      await page.setContent(`${reset}<div id="box" style="width:50px;height:50px;background:linear-gradient(#f00,#00f)"></div>`);
      const box = page.locator('#box');
      const jpeg = await box.screenshot({ type: 'jpeg', quality: 50 });
      expect([jpeg[0], jpeg[1]]).toEqual([0xff, 0xd8]);
      const webp = await box.screenshot({ type: 'webp' });
      expect(String.fromCharCode(...webp.slice(0, 4), ...webp.slice(8, 12))).toBe('RIFFWEBP');
      const lossy = await box.screenshot({ type: 'webp', quality: 10 });
      expect(String.fromCharCode(...lossy.slice(8, 12))).toBe('WEBP');
      const error = await box.screenshot({ type: 'png', quality: 50 }).catch(e => String(e));
      expect(error).toContain('options.quality is unsupported for the png screenshots');
    });
  });

  test('waits for a hidden element and times out with the screenshot timeout', async ({ browser }) => {
    await withPage(browser, async page => {
      await page.setContent(`${reset}<div id="later" style="display:none;width:20px;height:20px;background:#0f0"></div>`);
      const shot = page.locator('#later').screenshot({ scale: 'css', timeout: 5000 });
      await page.evaluate(() => setTimeout(() => (document.getElementById('later')!.style.display = 'block'), 200));
      expect(decodePng(await shot).pixel(10, 10)).toEqual(GREEN);
      await page.evaluate(() => (document.getElementById('later')!.style.display = 'none'));
      const error = await page.locator('#later').screenshot({ timeout: 300 }).catch(e => String(e));
      expect(error).toContain('300ms');
    });
  });
});

describe('page screenshot rects', () => {
  test('a clip is relative to the viewport on a scrolled page', async ({ browser }) => {
    await withPage(browser, async page => {
      await page.setContent(
        `${reset}<div style="height:500px;background:#f00"></div><div style="height:100px;background:#0f0"></div>` +
          '<div style="height:1000px;background:#00f"></div>',
      );
      await page.evaluate(() => window.scrollTo(0, 500));
      const clipped = decodePng(await page.screenshot({ clip: { x: 0, y: 0, width: 50, height: 50 }, scale: 'css' }));
      expect([clipped.width, clipped.height]).toEqual([50, 50]);
      expect(clipped.pixel(25, 25)).toEqual(GREEN);
    });
  });

  test('a full-page clip is relative to the document and trimmed to it', async ({ browser }) => {
    await withPage(browser, async page => {
      await page.setContent(
        `${reset}<div style="height:700px;background:#f00"></div><div style="height:50px;background:#0f0"></div>`,
      );
      const clipped = decodePng(
        await page.screenshot({ fullPage: true, clip: { x: 10, y: 710, width: 40, height: 400 }, scale: 'css' }),
      );
      expect([clipped.width, clipped.height]).toEqual([40, 40]);
      expect(clipped.pixel(20, 20)).toEqual(GREEN);
      const outside = await page.screenshot({ clip: { x: 400, y: 0, width: 10, height: 10 } }).catch(e => String(e));
      expect(outside).toContain('Clipped area is either empty or outside the resulting image');
    });
  });
});
