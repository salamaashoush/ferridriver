// Playwright's device registry, vendored at a pinned version
// (crates/ferridriver/src/devices/VENDOR.md). The table is exported by
// the framework module; a descriptor is used by spreading it, so what
// matters is that every key it carries reaches the browser.

import { test, describe, expect, devices } from '@ferridriver/test';

describe('device descriptors', () => {
  test('the registry is the whole vendored table', async ({}) => {
    expect(Object.keys(devices).length).toBe(207);
    expect(devices['Nokia 3310']).toBe(undefined);
    expect(devices['Desktop Safari'].defaultBrowserType).toBe('webkit');
    expect(devices['Desktop Firefox'].defaultBrowserType).toBe('firefox');
    expect(devices['Desktop Chrome'].defaultBrowserType).toBe('chromium');
  });

  test('a descriptor spread configures viewport, screen, agent and scale', async ({ browser }) => {
    // Desktop Edge: no touch, no mobile — the keys every backend
    // emulates, so the whole descriptor is asserted rather than a
    // subset. `screen` differs from `viewport` here (1920x1080 vs
    // 1280x720), which is what makes it observable at all.
    const device = devices['Desktop Edge'];
    expect(device.screen!.width).toBe(1920);

    const ctx = await browser.newContext({ ...device });
    try {
      const page = await ctx.newPage();
      const seen = (await page.evaluate(() => ({
        agent: navigator.userAgent,
        width: window.innerWidth,
        height: window.innerHeight,
        dpr: window.devicePixelRatio,
        screenWidth: window.screen.width,
        screenHeight: window.screen.height,
      }))) as {
        agent: string;
        width: number;
        height: number;
        dpr: number;
        screenWidth: number;
        screenHeight: number;
      };

      expect(seen.agent).toBe(device.userAgent);
      expect([seen.width, seen.height]).toEqual([device.viewport.width, device.viewport.height]);
      expect(Math.abs(seen.dpr - device.deviceScaleFactor)).toBeLessThan(0.01);
      expect([seen.screenWidth, seen.screenHeight]).toEqual([device.screen!.width, device.screen!.height]);
    } finally {
      await ctx.close();
    }
  });

  test('a screen larger than the viewport does not resize the viewport', async ({ browser }) => {
    // The screen and the viewport travel in one override on CDP; sending
    // the screen as a second command replaced the viewport with it.
    const ctx = await browser.newContext({
      viewport: { width: 800, height: 600 },
      screen: { width: 1920, height: 1080 },
      deviceScaleFactor: 2,
    });
    try {
      const page = await ctx.newPage();
      const seen = (await page.evaluate(() => ({
        width: window.innerWidth,
        height: window.innerHeight,
        dpr: window.devicePixelRatio,
        screenWidth: window.screen.width,
      }))) as { width: number; height: number; dpr: number; screenWidth: number };

      expect([seen.width, seen.height]).toEqual([800, 600]);
      expect(seen.screenWidth).toBe(1920);
      expect(Math.abs(seen.dpr - 2)).toBeLessThan(0.01);
    } finally {
      await ctx.close();
    }
  });

  test('a mobile descriptor emulates touch where the engine can', async ({ browser, browserName }) => {
    const device = devices['iPhone 15'];
    const ctx = await browser.newContext({ ...device });
    try {
      if (browserName === 'firefox') {
        // `hasTouch` has no BiDi wire and Playwright's own BiDi backend
        // does not emulate it either — the typed refusal naming the
        // option IS the behaviour here.
        let msg = '';
        try {
          await ctx.newPage();
        } catch (e) {
          msg = String((e as Error).message ?? e);
        }
        expect(msg.includes('hasTouch')).toBe(true);
        return;
      }
      const page = await ctx.newPage();
      // A mobile context lays a page with no viewport meta out at the
      // 980px fallback width, so the descriptor's viewport is only
      // observable from a document that opts into the device width —
      // which is what a mobile page does.
      await page.setContent('<meta name="viewport" content="width=device-width"><body>device</body>');
      const seen = (await page.evaluate(() => ({
        touch: 'ontouchstart' in window || navigator.maxTouchPoints > 0,
        agent: navigator.userAgent,
        width: window.innerWidth,
        dpr: window.devicePixelRatio,
      }))) as { touch: boolean; agent: string; width: number; dpr: number };

      expect(seen.touch).toBe(true);
      expect(seen.agent).toBe(device.userAgent);
      expect(seen.width).toBe(device.viewport.width);
      expect(Math.abs(seen.dpr - device.deviceScaleFactor)).toBeLessThan(0.01);
    } finally {
      await ctx.close();
    }
  });
});
