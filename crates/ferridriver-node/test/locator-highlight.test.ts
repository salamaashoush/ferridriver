/**
 * Locator.highlight / hideHighlight + the returned Disposable.
 *
 *   locator.highlight(options?: { style?: string | Record<string, string | number> }): Promise<Disposable>
 *   locator.hideHighlight(): Promise<void>
 *
 * Each highlighted locator gets one box in the engine's glass pane. The
 * pane's shadow root is closed outside Playwright's own test mode, so the
 * boxes are counted through the entries the engine rendered.
 * dispose() and hideHighlight() remove that locator's box and leave the
 * others; dispose() is idempotent.
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe", "cdp-raw", ...(process.platform === "darwin" ? ["webkit"] : [])];

const boxCount = (page: Page) =>
  page.evaluate("window.__fd?._injected?._highlight?._renderedEntries?.length ?? 0") as Promise<number>;

// The engine renders highlights on the next animation frame.
async function expectBoxes(page: Page, n: number) {
  const deadline = Date.now() + 5000;
  let seen = await boxCount(page);
  while (seen !== n && Date.now() < deadline) {
    await new Promise((r) => setTimeout(r, 50));
    seen = await boxCount(page);
  }
  expect(seen).toBe(n);
}

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
      await expectBoxes(page, 0);

      const disposable = await page.locator("#b").highlight();
      await expectBoxes(page, 1);

      await disposable.dispose();
      await expectBoxes(page, 0);

      // dispose() is idempotent — second call is a no-op, not an error.
      await disposable.dispose();
      await expectBoxes(page, 0);
    });

    it("hideHighlight removes only that locator's box", async () => {
      await page.setContent("<button id='b'>Target</button><a id='a' href='#'>Other</a>");
      await page.locator("#b").highlight();
      await page.locator("#a").highlight();
      await expectBoxes(page, 2);

      await page.locator("#b").hideHighlight();
      await expectBoxes(page, 1);

      await page.hideHighlight();
      await expectBoxes(page, 0);
    });

    it("accepts a style string", async () => {
      await page.setContent("<button id='b'>Target</button>");
      const disposable = await page.locator("#b").highlight({ style: "outline: 2px solid red" });
      await expectBoxes(page, 1);
      await disposable.dispose();
    });

    it("accepts a style record (string + number values)", async () => {
      await page.setContent("<button id='b'>Target</button>");
      const disposable = await page
        .locator("#b")
        .highlight({ style: { outlineColor: "red", zIndex: 7 } });
      await expectBoxes(page, 1);
      await disposable.dispose();
      await expectBoxes(page, 0);
    });
  });
}
