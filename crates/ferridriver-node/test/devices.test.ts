// `devices` — Playwright's registry as a module-level value, vendored at
// a pinned version (crates/ferridriver/src/devices/VENDOR.md).

import { expect, test } from "bun:test";
import { devices } from "../index.js";
import { launchForBackend } from "./_helpers";

test("the registry is the whole vendored table", () => {
  expect(Object.keys(devices).length).toBe(207);
  expect(devices["Nokia 3310"]).toBeUndefined();
  expect(devices["Desktop Safari"].defaultBrowserType).toBe("webkit");
});

test("a descriptor carries every key a spread needs", () => {
  const d = devices["iPhone 15"];
  expect(d.userAgent).toContain("iPhone");
  expect(d.viewport).toEqual({ width: 393, height: 659 });
  expect(d.screen).toEqual({ width: 393, height: 852 });
  expect(d.deviceScaleFactor).toBe(3);
  expect(d.isMobile).toBe(true);
  expect(d.hasTouch).toBe(true);
});

test("a descriptor spread configures the context", async () => {
  const browser = await launchForBackend("cdp-pipe");
  try {
    const device = devices["Desktop Edge"];
    const ctx = await browser.newContext({ ...device });
    try {
      const page = await ctx.newPage();
      const seen = (await page.evaluate(`({
        agent: navigator.userAgent,
        width: window.innerWidth,
        screenWidth: window.screen.width,
      })`)) as { agent: string; width: number; screenWidth: number };
      expect(seen.agent).toBe(device.userAgent);
      expect(seen.width).toBe(device.viewport.width);
      expect(seen.screenWidth).toBe(device.screen!.width);
    } finally {
      await ctx.close();
    }
  } finally {
    await browser.close();
  }
});
