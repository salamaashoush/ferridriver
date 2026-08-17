/**
 * `selectors.register` and `selectors.setTestIdAttribute` over NAPI.
 *
 * Playwright: `packages/playwright-core/src/client/selectors.ts` —
 * `selectors.register(name, script, options)` and
 * `selectors.setTestIdAttribute(attributeName)`. Both rules live in
 * `ferridriver::selectors`; this proves the binding reaches them and
 * that a registered engine really answers a locator query.
 *
 * The registry is process-global (ferridriver's workers share one
 * process), so the test-id attribute is restored at the end.
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { selectors, type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : (["cdp-pipe", "cdp-raw"] as const);

const TAG_ENGINE = () => ({
  queryAll(root: Element | Document, selector: string) {
    return Array.from(root.querySelectorAll(selector));
  },
});

// A document is injected with the engines registered by the time it was
// created, so registration has to precede the page — Playwright says
// the same ("Selectors must be registered before creating the page").
await selectors.register("tagname", TAG_ENGINE);

for (const backend of BACKENDS) {
  describe(`[${backend}] selectors.register / setTestIdAttribute`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
      page = await browser.newPage();
    }, 30_000);

    afterAll(async () => {
      selectors.setTestIdAttribute("data-testid");
      await browser?.close();
    });

    it("a registered engine answers a locator query", async () => {
      await page.goto(
        "data:text/html,<h1>one</h1><h2>two</h2><h1>three</h1>",
        null,
      );

      // Counts differ per tag, so a stub that matched everything (or
      // nothing) fails both ways.
      expect(await page.locator("tagname=h1").count()).toBe(2);
      expect(await page.locator("tagname=h2").count()).toBe(1);
      expect(await page.locator("tagname=h2").textContent()).toBe("two");
      // Chains like any other engine.
      expect(await page.locator("tagname=h1").nth(1).textContent()).toBe(
        "three",
      );
    });

    it("re-registering the same name with another script is refused", async () => {
      await expect(
        selectors.register("tagname", () => ({
          queryAll: () => [],
        })),
      ).rejects.toThrow(/already registered/);
    });

    it("setTestIdAttribute changes what getByTestId matches", async () => {
      await page.goto(
        'data:text/html,<div data-testid="a">A</div><div data-qa="b">B</div>',
        null,
      );

      expect(await page.getByTestId("a").count()).toBe(1);
      expect(await page.getByTestId("b").count()).toBe(0);

      selectors.setTestIdAttribute("data-qa");
      expect(await page.getByTestId("b").count()).toBe(1);
      expect(await page.getByTestId("b").textContent()).toBe("B");
      expect(await page.getByTestId("a").count()).toBe(0);

      selectors.setTestIdAttribute("data-testid");
      expect(await page.getByTestId("a").count()).toBe(1);
    });
  });
}
