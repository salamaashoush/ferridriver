// `page.waitForURL` and `frame.waitForURL` take a `(url: URL) => boolean |
// Promise<boolean>` as Playwright's do.

import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import type { Browser, Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS = process.env.FERRIDRIVER_BACKEND ? [process.env.FERRIDRIVER_BACKEND] : ["cdp-pipe", "webkit"];

async function landed(browser: Browser): Promise<Page> {
  const page = await browser.newPage();
  await page.route("http://example.test/**", (route) =>
    route.fulfill({ status: 200, contentType: "text/html", body: "<p>landed</p>" }),
  );
  await page.goto("http://example.test/start");
  return page;
}

function pushSoon(page: Page, path: string): Promise<unknown> {
  return page.evaluate(`setTimeout(() => history.pushState({}, '', ${JSON.stringify(path)}), 100)`);
}

for (const backend of BACKENDS) {
  describe(`waitForURL with a predicate [${backend}]`, () => {
    let browser: Browser;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
    });

    afterAll(async () => {
      await browser.close();
    });

    it("calls it with a URL and resolves when it returns true", async () => {
      const page = await landed(browser);
      await pushSoon(page, "/target?x=1");
      const kinds = new Set<string>();
      await page.waitForURL(
        (url) => {
          kinds.add(url instanceof URL ? "URL" : typeof url);
          return url.pathname === "/target" && url.searchParams.get("x") === "1";
        },
        { timeout: 5000 },
      );
      expect(page.url()).toBe("http://example.test/target?x=1");
      expect([...kinds]).toEqual(["URL"]);
    });

    it("awaits a predicate that answers with a promise, on a frame too", async () => {
      const page = await landed(browser);
      await pushSoon(page, "/later");
      await page.mainFrame().waitForURL(async (url) => {
        await Bun.sleep(5);
        return url.pathname === "/later";
      });
      expect(page.url()).toBe("http://example.test/later");
    });

    it("times out at the timeout asked for, and rejects with the predicate's error", async () => {
      const page = await landed(browser);
      const started = Date.now();
      await expect(page.waitForURL(() => false, { timeout: 500 })).rejects.toThrow(/500/);
      expect(Date.now() - started).toBeLessThan(3000);
      await expect(
        page.waitForURL(() => {
          throw new Error("sashoush predicate failed");
        }),
      ).rejects.toThrow(/sashoush predicate failed/);
    });
  });
}
