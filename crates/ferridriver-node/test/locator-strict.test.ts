/**
 * Locator strict-mode parity with Playwright.
 *
 * Playwright: every Locator action is strict by default — matching more than
 * one element raises an error whose message contains "strict mode violation".
 * `first()` / `last()` / `nth(i)` opt out because the selector explicitly
 * narrows to a single match, and consumers can also call `.setStrict(false)`.
 *
 * These tests drive a real CDP backend against a local HTTP server (no
 * mocking) so the full path through `selectors::query_all` + FerriError +
 * NAPI error prefix is exercised end-to-end.
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { Browser, type Page } from "../index.js";
import { createServer, type Server } from "node:http";

let testServer: Server;
let testUrl: string;

const MULTI_MATCH_PAGE = `<!DOCTYPE html>
<html>
  <head><title>Strict mode fixture</title></head>
  <body>
    <button class="btn">one</button>
    <button class="btn">two</button>
    <button class="btn">three</button>
    <input id="solo" value="ok" />
  </body>
</html>`;

beforeAll(async () => {
  testServer = createServer((_req, res) => {
    res.writeHead(200, { "Content-Type": "text/html" });
    res.end(MULTI_MATCH_PAGE);
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
  describe(`[${backend}] Locator strict mode`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await Browser.launch({ backend });
      page = await browser.newPageWithUrl(testUrl);
    });

    afterAll(async () => {
      await browser.close();
    });

    it("is strict by default (isStrict getter)", () => {
      const loc = page.locator("button.btn");
      expect(loc.isStrict).toBe(true);
    });

    it("click on a multi-match locator raises a strict mode violation", async () => {
      const loc = page.locator("button.btn");
      let caught: unknown;
      try {
        await loc.click();
      } catch (e) {
        caught = e;
      }
      expect(caught).toBeInstanceOf(Error);
      const msg = (caught as Error).message;
      expect(msg).toContain("strict mode violation");
      expect(msg).toContain("button.btn");
      // Reports the actual match count so callers can disambiguate.
      expect(msg).toMatch(/3 elements/);
    });

    it("count() works on multi-match locators even under strict mode", async () => {
      // count() is not an action that picks a single element; it should
      // succeed without tripping strict mode. Mirrors Playwright.
      const loc = page.locator("button.btn");
      expect(await loc.count()).toBe(3);
    });

    it("first() opts out of strict mode and succeeds", async () => {
      const loc = page.locator("button.btn").first();
      expect(loc.isStrict).toBe(false);
      // Should not throw.
      await loc.click();
    });

    it("last() opts out of strict mode", async () => {
      const loc = page.locator("button.btn").last();
      expect(loc.isStrict).toBe(false);
      await loc.click();
    });

    it("nth(i) opts out of strict mode", async () => {
      const loc = page.locator("button.btn").nth(1);
      expect(loc.isStrict).toBe(false);
      await loc.click();
    });

    it("setStrict(false) suppresses the violation", async () => {
      const loc = page.locator("button.btn").setStrict(false);
      expect(loc.isStrict).toBe(false);
      // Falls back to the first match (Playwright-compatible non-strict behaviour).
      await loc.click();
    });

    it("single-match locator passes strict mode", async () => {
      const loc = page.locator("#solo");
      expect(loc.isStrict).toBe(true);
      // innerText is a content query — still subject to strict mode in Playwright.
      // It should succeed since #solo resolves to exactly one element.
      const txt = await loc.getAttribute("value");
      expect(txt).toBe("ok");
    });

    it("strict flag persists through chaining", () => {
      // chain() inherits strictness. first() resets it; locator() preserves it.
      const base = page.locator("body");
      expect(base.isStrict).toBe(true);
      expect(base.locator("button.btn").isStrict).toBe(true);
      expect(base.locator("button.btn").first().isStrict).toBe(false);
      // Chaining after first() keeps the loosened flag.
      expect(base.locator("button.btn").first().locator("span").isStrict).toBe(false);
    });

    it("setStrict(true) re-enables strict mode after first()", async () => {
      const loc = page.locator("button.btn").first().setStrict(true);
      expect(loc.isStrict).toBe(true);
      // first() selector narrowed to nth=0 so even with strict=true, only
      // one element matches — the click must succeed.
      await loc.click();
    });
  });
}
