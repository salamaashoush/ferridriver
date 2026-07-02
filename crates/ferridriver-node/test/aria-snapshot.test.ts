// NAPI coverage for locator.ariaSnapshot({ boxes }) (Playwright 1.60).

import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe"];

for (const backend of BACKENDS) {
  describe(`ariaSnapshot boxes [${backend}]`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
      page = await browser.newPage();
    });

    afterAll(async () => {
      await browser.close();
    });

    it("ariaSnapshot({ boxes }) annotates boxes; default does not", async () => {
      await page.setContent("<button>Boxed</button>");
      const withBoxes = await page.locator("body").ariaSnapshot({ boxes: true });
      const without = await page.locator("body").ariaSnapshot();
      expect(withBoxes).toContain("[box=");
      expect(without).not.toContain("[box=");
    });
  });
}
