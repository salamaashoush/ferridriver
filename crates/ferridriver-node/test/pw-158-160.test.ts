// NAPI coverage for the Playwright 1.58-1.60 API subset:
// locator.description() (1.58), getByRole({ description }) (1.60),
// context.setStorageState() (1.59), locator.ariaSnapshot({ boxes }) (1.60).

import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe"];

for (const backend of BACKENDS) {
  describe(`pw 1.58-1.60 [${backend}]`, () => {
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

    it("ariaSnapshot({ boxes }) annotates boxes; default does not", async () => {
      await page.setContent("<button>Boxed</button>");
      const withBoxes = await page.locator("body").ariaSnapshot({ boxes: true });
      const without = await page.locator("body").ariaSnapshot();
      expect(withBoxes).toContain("[box=");
      expect(without).not.toContain("[box=");
    });

    it("context.setStorageState clears existing and applies new cookies", async () => {
      const context = browser.defaultContext();
      await context.addCookies([
        { name: "stale", value: "yes", domain: "example.com", path: "/", secure: false, httpOnly: false },
      ]);
      await context.setStorageState({
        cookies: [{ name: "seeded", value: "fromState", domain: "example.com", path: "/" }],
        origins: [],
      });
      const names = (await context.cookies()).map((c) => c.name);
      expect(names).not.toContain("stale");
      expect(names).toContain("seeded");
    });
  });
}
