// NAPI coverage for page.localStorage / page.sessionStorage WebStorage
// accessors (Playwright 1.61), cross-checked against window.localStorage.

import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe"];

for (const backend of BACKENDS) {
  describe(`WebStorage [${backend}]`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
      page = await browser.newPage();
    });

    afterAll(async () => {
      await browser.close();
    });

    it("localStorage / sessionStorage round-trip against real storage", async () => {
      const server = Bun.serve({
        port: 0,
        fetch: () =>
          new Response("<!doctype html><body>web-storage</body>", {
            headers: { "content-type": "text/html" },
          }),
      });
      try {
        await page.goto(`http://localhost:${server.port}/store`);
        await page.localStorage.setItem("token", "abc");
        await page.localStorage.setItem("user", "sam");
        await page.sessionStorage.setItem("sid", "sess-1");

        expect(await page.localStorage.getItem("token")).toBe("abc");
        expect(await page.localStorage.getItem("nope")).toBeNull();

        const names = (await page.localStorage.items()).map((i) => i.name).sort();
        expect(names).toEqual(["token", "user"]);

        expect(await page.evaluate(() => window.localStorage.getItem("token"))).toBe("abc");
        expect(await page.evaluate(() => window.sessionStorage.getItem("sid"))).toBe("sess-1");

        await page.localStorage.removeItem("user");
        expect((await page.localStorage.items()).map((i) => i.name)).toEqual(["token"]);

        await page.localStorage.clear();
        expect((await page.localStorage.items()).length).toBe(0);
      } finally {
        server.stop(true);
      }
    });
  });
}
