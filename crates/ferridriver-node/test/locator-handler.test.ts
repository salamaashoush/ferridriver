/**
 * NAPI parity tests for `page.addLocatorHandler` / `page.removeLocatorHandler`.
 *
 * Mirrors Playwright `client/page.ts:397` addLocatorHandler(locator, handler,
 * { times?, noWaitAfter? }). Rule 9: each test observes a DOM effect that only
 * occurs when the handler actually fired -- a fixed overlay covers the target
 * button so a click cannot land until the handler removes the overlay.
 */
import { describe, it, expect, beforeAll, afterAll, beforeEach } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : (["cdp-pipe", "cdp-raw"] as const);

const FIXTURE = `
<button id="target" onclick="window.__clicked=true">Click me</button>
<div id="overlay" style="position:fixed;inset:0;z-index:9999;background:rgba(0,0,0,0.5)">blocking</div>
<script>window.__handlerRuns=0;</script>`;

function dataUrl(html: string): string {
  return "data:text/html," + encodeURIComponent(html);
}

for (const backend of BACKENDS) {
  describe(`[${backend}] page.addLocatorHandler`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
      page = await browser.newPage();
    }, 30_000);

    afterAll(async () => {
      await browser?.close();
    });

    beforeEach(async () => {
      // The page is shared across tests; clear any handler a prior test
      // registered (they all watch #overlay) so it can't dismiss this test's
      // overlay before this test's own handler runs.
      page.removeLocatorHandler(page.locator("#overlay", undefined));
      await page.goto(dataUrl(FIXTURE), null);
      await page.waitForSelector("#target", null);
    });

    it("runs the handler to dismiss a blocking overlay so the click lands", async () => {
      let runs = 0;
      await page.addLocatorHandler(page.locator("#overlay", undefined), async () => {
        runs++;
        await page.evaluate("document.getElementById('overlay').remove()", undefined);
      });
      await page.locator("#target", undefined).click({ timeout: 8000 });
      expect(await page.evaluate("window.__clicked === true", undefined)).toBe(true);
      expect(runs).toBeGreaterThanOrEqual(1);
      expect(await page.locator("#overlay", undefined).isVisible()).toBe(false);
    });

    it("auto-removes after `times` invocations", async () => {
      let runs = 0;
      await page.addLocatorHandler(
        page.locator("#overlay", undefined),
        async () => {
          runs++;
          await page.evaluate("document.getElementById('overlay').remove()", undefined);
        },
        { times: 1 },
      );
      await page.locator("#target", undefined).click({ timeout: 8000 });
      expect(runs).toBe(1);

      // Re-add the overlay and click again. The handler is exhausted
      // (times:1 already consumed), so it must NOT fire a second time --
      // runs stays 1.
      await page.evaluate(
        "const d=document.createElement('div');d.id='overlay';d.style.cssText='position:fixed;inset:0;z-index:9999';document.body.appendChild(d);",
        undefined,
      );
      await page.locator("#target", undefined).click({ timeout: 2000 });
      expect(runs).toBe(1);
    });

    it("removeLocatorHandler stops the handler from firing", async () => {
      let runs = 0;
      const overlay = page.locator("#overlay", undefined);
      await page.addLocatorHandler(overlay, async () => {
        runs++;
        await page.evaluate("document.getElementById('overlay').remove()", undefined);
      });
      page.removeLocatorHandler(overlay);
      // With the handler removed, a click must not invoke it (runs stays 0).
      await page.locator("#target", undefined).click({ timeout: 2000 });
      expect(runs).toBe(0);
    });
  });
}
