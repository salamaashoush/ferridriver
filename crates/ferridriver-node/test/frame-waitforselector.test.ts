/**
 * Fix #14 — Frame.waitForSelector overload + state return type.
 *
 * Playwright signature (cloned at
 * /tmp/playwright/packages/playwright-core/src/client/frame.ts:217):
 *
 *   waitForSelector(selector, options & { state: 'attached' | 'visible' }):
 *     Promise<ElementHandle>
 *   waitForSelector(selector, options?): Promise<ElementHandle | null>
 *
 * The default / `attached` / `visible` states resolve to the matched
 * ElementHandle; `hidden` / `detached` resolve to null (Playwright
 * returns the handle only when the element is present).
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe", "cdp-raw", ...(process.platform === "darwin" ? ["webkit"] : [])];

for (const backend of BACKENDS) {
  describe(`[${backend}] Frame.waitForSelector return`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
      page = await browser.newPage();
    });

    afterAll(async () => {
      await browser.close();
    });

    it("default state returns the matched ElementHandle (not void)", async () => {
      await page.setContent('<div id="t">payload-text</div>');
      const main = page.mainFrame()!;
      const handle = await main.waitForSelector("#t", { timeout: 2000 });
      // Observable effect that only occurs when the handle is the real
      // resolved element: reading its text returns the element content.
      expect(handle).not.toBeNull();
      expect(await handle!.textContent()).toBe("payload-text");
    });

    it("state: 'attached' returns the handle even when display:none", async () => {
      await page.setContent(
        '<div id="hid" style="display:none">hidden-payload</div>',
      );
      const main = page.mainFrame()!;
      const handle = await main.waitForSelector("#hid", {
        state: "attached",
        timeout: 2000,
      });
      expect(handle).not.toBeNull();
      expect(await handle!.getAttribute("id")).toBe("hid");
    });

    it("state: 'visible' returns the handle once it renders", async () => {
      await page.setContent(
        '<div id="late" style="display:none">later-payload</div>' +
          "<script>setTimeout(() => document.getElementById('late').style.display = 'block', 150);</script>",
      );
      const main = page.mainFrame()!;
      const handle = await main.waitForSelector("#late", {
        state: "visible",
        timeout: 3000,
      });
      expect(handle).not.toBeNull();
      expect(await handle!.textContent()).toBe("later-payload");
    });

    it("state: 'hidden' resolves to null for a display:none element", async () => {
      await page.setContent('<div id="h" style="display:none">x</div>');
      const main = page.mainFrame()!;
      const handle = await main.waitForSelector("#h", {
        state: "hidden",
        timeout: 2000,
      });
      expect(handle).toBeNull();
    });

    it("state: 'detached' resolves to null once the element leaves the DOM", async () => {
      await page.setContent(
        '<div id="gone">bye</div>' +
          "<script>setTimeout(() => document.getElementById('gone').remove(), 150);</script>",
      );
      const main = page.mainFrame()!;
      const handle = await main.waitForSelector("#gone", {
        state: "detached",
        timeout: 3000,
      });
      expect(handle).toBeNull();
    });

    it("resolves within a child iframe and returns that frame's element", async () => {
      await page.setContent(
        '<iframe name="child" srcdoc="<div id=inner>inner-payload</div>"></iframe>',
      );
      // Give the iframe a beat to register in the frame tree.
      await page.waitForLoadState();
      const frame = page.frame("child");
      expect(frame).not.toBeNull();
      const handle = await frame!.waitForSelector("#inner", { timeout: 3000 });
      expect(handle).not.toBeNull();
      expect(await handle!.textContent()).toBe("inner-payload");
    });
  });
}
