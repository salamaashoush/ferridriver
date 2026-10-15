// The read option bags through the binding: a read waits for its element,
// gives up at its own timeout, honours `strict` on page reads, and stops
// on a signal.
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe", "cdp-raw", ...(process.platform === "darwin" ? ["webkit"] : [])];

const failure = (work: Promise<unknown>) =>
  work.then(
    (value) => {
      throw new Error(`expected a rejection, got ${JSON.stringify(value)}`);
    },
    (e: Error) => e,
  );

for (const backend of BACKENDS) {
  describe(`[${backend}] reads`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
      page = await browser.newPage();
    });

    afterAll(async () => {
      await browser.close();
    });

    it("waits for an element added later", async () => {
      await page.setContent("<main></main>");
      await page.evaluate(() => {
        setTimeout(() => document.body.insertAdjacentHTML("beforeend", '<p id="late" title="t">late</p>'), 300);
      });
      const [text, title] = await Promise.all([
        page.locator("#late").textContent(),
        page.getAttribute("#late", "title"),
      ]);
      expect(text).toBe("late");
      expect(title).toBe("t");
    });

    it("times out at the timeout it was given", async () => {
      await page.setContent("<main></main>");
      const error = await failure(page.locator("#none").textContent({ timeout: 200 }));
      expect(error.message).toContain("Timeout 200ms exceeded");
      expect(error.message).toContain("waiting for locator('#none')");
      expect((await failure(page.isChecked("#none", { timeout: 100 }))).message).toContain("Timeout 100ms");
    });

    it("takes the first match unless strict", async () => {
      await page.setContent("<ul><li>one</li><li>two</li></ul>");
      expect(await page.innerHTML("li")).toBe("one");
      expect((await failure(page.innerHTML("li", { strict: true }))).message).toContain("strict mode violation");
      expect((await failure(page.locator("li").innerHTML())).message).toContain("strict mode violation");
    });

    it("stops on a signal", async () => {
      await page.setContent("<main></main>");
      const controller = new AbortController();
      const read = failure(page.locator("#none").inputValue({ signal: controller.signal, timeout: 0 }));
      setTimeout(() => controller.abort("stop reading"), 200);
      expect((await read).message).toContain("stop reading");
    });
  });
}
