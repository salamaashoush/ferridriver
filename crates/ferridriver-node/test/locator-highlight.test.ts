/**
 * Fix #7 — Locator.highlight / hideHighlight + the returned Disposable.
 *
 * Playwright signatures (cloned at
 * /tmp/playwright/packages/playwright-core/src/client/locator.ts):
 *
 *   locator.highlight(options?: { style?: string | Record<string, string | number> }): Promise<Disposable>
 *   locator.hideHighlight(): Promise<void>
 *
 * The highlight overlay installs a `<x-pw-glass>` popover element on
 * documentElement; its presence is a real effect of addHighlight running.
 * The returned Disposable's dispose() tears the overlay down, as does
 * hideHighlight(). dispose() is idempotent.
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe", "cdp-raw", ...(process.platform === "darwin" ? ["webkit"] : [])];

const glassCount = (page: Page) =>
  page.evaluate("document.querySelectorAll('x-pw-glass').length") as Promise<number>;

for (const backend of BACKENDS) {
  describe(`[${backend}] locator highlight`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
      page = await browser.newPage();
    });

    afterAll(async () => {
      await browser.close();
    });

    it("highlight installs the overlay, dispose() removes it", async () => {
      await page.setContent("<button id='b'>Target</button>");
      expect(await glassCount(page)).toBe(0);

      const disposable = await page.locator("#b").highlight();
      expect(await glassCount(page)).toBe(1);

      await disposable.dispose();
      expect(await glassCount(page)).toBe(0);

      // dispose() is idempotent — second call is a no-op, not an error.
      await disposable.dispose();
      expect(await glassCount(page)).toBe(0);
    });

    it("hideHighlight removes the overlay", async () => {
      await page.setContent("<button id='b'>Target</button>");
      await page.locator("#b").highlight();
      expect(await glassCount(page)).toBe(1);

      await page.locator("#b").hideHighlight();
      expect(await glassCount(page)).toBe(0);
    });

    it("accepts a style string", async () => {
      await page.setContent("<button id='b'>Target</button>");
      const disposable = await page.locator("#b").highlight({ style: "outline: 2px solid red" });
      expect(await glassCount(page)).toBe(1);
      await disposable.dispose();
    });

    it("accepts a style record (string + number values)", async () => {
      await page.setContent("<button id='b'>Target</button>");
      const disposable = await page
        .locator("#b")
        .highlight({ style: { outlineColor: "red", zIndex: 7 } });
      expect(await glassCount(page)).toBe(1);
      await disposable.dispose();
      expect(await glassCount(page)).toBe(0);
    });
  });
}
