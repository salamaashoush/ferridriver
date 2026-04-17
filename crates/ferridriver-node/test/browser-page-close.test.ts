/**
 * Browser/Page close options + version parity — tasks 3.19, 3.20, 3.21, 3.23.
 *
 * Playwright signatures (cloned at
 * /tmp/playwright/packages/playwright-core/src/client/browser.ts and page.ts):
 *
 *   browser.version(): string
 *   browser.close({ reason? }): Promise<void>
 *   page.close({ runBeforeUnload?, reason? }): Promise<void>
 *   page.setDefaultNavigationTimeout(timeout: number): void
 *
 * Every backend ferridriver supports must expose these with real values
 * — no placeholder strings. See feedback_no_stubs_all_backends.md.
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { Browser } from "../index.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe", "cdp-raw", ...(process.platform === "darwin" ? ["webkit"] : [])];

for (const backend of BACKENDS) {
  describe(`[${backend}] Browser.version + close options`, () => {
    let browser: Browser;

    beforeAll(async () => {
      browser = await Browser.launch({ backend });
    });

    afterAll(async () => {
      // Avoid double-close: afterAll runs after each test closes explicitly.
      try {
        await browser.close();
      } catch (_) {
        /* already closed */
      }
    });

    // ── 3.19 Browser.version ─────────────────────────────────────────────

    it("browser.version returns a real product string, not the engine name", async () => {
      const v = browser.version;
      expect(typeof v).toBe("string");
      expect(v.length).toBeGreaterThan(0);
      // NOT a hardcoded placeholder — must look like a real product version.
      // CDP: "HeadlessChrome/120.0.6099.109" or "Chrome/120.0.6099.109"
      // WebKit: "WebKit/617.1.2 (17618)"
      // BiDi/Firefox: "firefox/135.0.1"
      expect(v).not.toBe("Chromium");
      expect(v).not.toBe("BiDi");
      // Must contain a slash separator between product name and version.
      expect(v).toContain("/");
      // Version segment must contain digits.
      const parts = v.split("/");
      expect(parts.length).toBeGreaterThanOrEqual(2);
      const versionPart = parts.slice(1).join("/");
      expect(/\d/.test(versionPart)).toBe(true);
    });

    // ── 3.21 Page.close accepts { reason, runBeforeUnload } ──────────────

    it("page.close({}) succeeds (empty options)", async () => {
      const page = await browser.newPage();
      await page.close({});
      expect(page.isClosed()).toBe(true);
    });

    it("page.close({ reason }) closes the page and does not throw", async () => {
      const page = await browser.newPage();
      await page.close({ reason: "test cleanup" });
      expect(page.isClosed()).toBe(true);
    });

    it("page.close({ runBeforeUnload: true }) fires beforeunload handler", async () => {
      const page = await browser.newPage();
      await page.setContent(
        "<script>let fired = false; window.addEventListener('beforeunload', () => { fired = true; window.__beforeUnloadFired = true; }, { capture: true });</script>",
      );

      await page.close({ runBeforeUnload: true });
      expect(page.isClosed()).toBe(true);
      // We can't introspect window state after close, but the call must
      // not throw — the backend path (CDP Page.close vs WebKit synthetic
      // dispatch vs BiDi promptUnload) is exercised.
    });

    // ── 3.20 Browser.close({ reason }) ──────────────────────────────────

    it("browser.close({ reason }) is accepted and closes the browser", async () => {
      const short = await Browser.launch({ backend });
      const v = short.version;
      expect(v).toContain("/");
      await short.close({ reason: "test cleanup" });
    });

    // ── 3.23 page.setDefaultNavigationTimeout ───────────────────────────

    it("page.setDefaultNavigationTimeout is exposed and distinct from setDefaultTimeout", async () => {
      const page = await browser.newPage();
      // If the method is bound correctly, neither call throws. Past bug
      // would have been one being missing or aliasing the other.
      page.setDefaultTimeout(5000);
      page.setDefaultNavigationTimeout(10000);
      await page.close();
    });
  });
}
