// NAPI coverage for Playwright 1.59-1.61 gap fills:
// webError.location() (1.60), request.existingResponse() (1.59),
// page.localStorage / page.sessionStorage WebStorage (1.61).

import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe"];

for (const backend of BACKENDS) {
  describe(`pw 1.59-1.61 [${backend}]`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
      page = await browser.newPage();
    });

    afterAll(async () => {
      await browser.close();
    });

    it("request.existingResponse() returns the received response without waiting", async () => {
      const resp = await page.goto("data:text/html,<title>existing</title>");
      const req = resp!.request();
      const existing = await req.existingResponse();
      expect(existing).not.toBeNull();
      expect(existing!.url()).toBe(resp!.url());
    });

    it("webError.location() exposes { url, line, column }", async () => {
      const context = browser.defaultContext();
      const [werr] = await Promise.all([
        context.waitForEvent("weberror", 5000),
        page.evaluate(() => {
          setTimeout(() => {
            throw new Error("boom-loc");
          }, 10);
        }),
      ]);
      expect(werr.error().message).toBe("boom-loc");
      const loc = werr.location();
      expect(typeof loc.url).toBe("string");
      expect(typeof loc.line).toBe("number");
      expect(typeof loc.column).toBe("number");
    });

    it("page.localStorage / sessionStorage round-trip against real storage", async () => {
      const server = Bun.serve({
        port: 0,
        fetch: () =>
          new Response("<!doctype html><body>ws</body>", {
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
