// NAPI coverage for accessible-description handling:
// locator.describe()/description() (Playwright 1.58) and
// getByRole(role, { description }) (Playwright 1.60).

import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe"];

for (const backend of BACKENDS) {
  describe(`accessible description [${backend}]`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
      page = await browser.newPage();
    });

    afterAll(async () => {
      await browser.close();
    });

    it("locator.description() round-trips describe(); plain locator is null", () => {
      const described = page.locator("#go").describe("the go button");
      expect(described.description()).toBe("the go button");
      expect(page.locator("#go").description()).toBeNull();
    });

    it("getByRole matches on accessible description", async () => {
      await page.setContent(
        "<button aria-description='primary action'>Save</button>" +
          "<button aria-description='secondary action'>Cancel</button>",
      );
      const primary = page.getByRole("button", { description: "primary action" });
      expect(await primary.count()).toBe(1);
      expect(await primary.textContent()).toBe("Save");
    });
  });
}
