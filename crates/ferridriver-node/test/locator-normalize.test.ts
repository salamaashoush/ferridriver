/**
 * Locator.normalize() parity with Playwright.
 *
 * Playwright: `locator.normalize(): Promise<Locator>`
 * (packages/playwright-core/src/client/locator.ts:269) calls
 * `frame.resolveSelector` -> `injected.generateSelectorSimple(node)` and
 * returns a NEW Locator built from the canonical recorder selector for
 * the matched element.
 *
 * Observable effect (Rule 9): a text locator normalizes to the generated
 * `internal:testid` form (clearly different from the input selector) and
 * still resolves to the same single element, so an action through the
 * normalized locator hits the same node.
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";
import { createServer, type Server } from "node:http";

let testServer: Server;
let testUrl: string;

const PAGE = `<!DOCTYPE html>
<html>
  <body>
    <button data-testid="save-btn" onclick="this.dataset.hit='1'">Save</button>
    <button>Cancel</button>
  </body>
</html>`;

beforeAll(async () => {
  testServer = createServer((_req, res) => {
    res.writeHead(200, { "Content-Type": "text/html" });
    res.end(PAGE);
  });
  await new Promise<void>((resolve) => {
    testServer.listen(0, "127.0.0.1", () => {
      const addr = testServer.address() as any;
      testUrl = `http://127.0.0.1:${addr.port}`;
      resolve();
    });
  });
});

afterAll(() => {
  testServer?.close();
});

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : (() => {
      const b = ["cdp-pipe", "cdp-raw"];
      if (process.platform === "darwin") b.push("webkit");
      return b;
    })();

for (const backend of BACKENDS) {
  describe(`[${backend}] Locator.normalize`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
      page = await browser.newPageWithUrl(testUrl);
    });

    afterAll(async () => {
      await browser.close();
    });

    it("resolves a text locator to the canonical testid selector and targets the same node", async () => {
      const orig = page.getByText("Save");
      const norm = await orig.normalize();

      // The normalized selector is a brand-new canonical form, not a
      // pass-through of the original text selector.
      expect(norm.selector).not.toBe(orig.selector);
      expect(norm.selector).toContain("save-btn");

      // It still resolves to exactly one element.
      expect(await norm.count()).toBe(1);

      // Acting through the normalized locator hits the same Save button.
      await norm.click();
      const hit = await page.evaluate(
        "document.querySelector('[data-testid=save-btn]').dataset.hit",
      );
      expect(hit).toBe("1");
    });
  });
}
