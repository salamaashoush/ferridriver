// NAPI coverage for context/browser lifecycle-observation events
// (Playwright 1.60): BrowserContext mirrors page-level
// framenavigated / pageload / pageclose, and browser emits 'context'
// on newContext(). Exercised via both waitForEvent and once.

import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe"];

for (const backend of BACKENDS) {
  describe(`context lifecycle events [${backend}]`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
      page = await browser.newPage();
    });

    afterAll(async () => {
      await browser.close();
    });

    it("context.waitForEvent('framenavigated') resolves a Frame", async () => {
      const context = browser.defaultContext();
      const [frame] = await Promise.all([
        context.waitForEvent("framenavigated", 5000),
        page.goto("data:text/html,<title>ctx-navmark</title>"),
      ]);
      expect(typeof (frame as any).url).toBe("function");
      expect((frame as any).url()).toContain("ctx-navmark");
    });

    it("context.waitForEvent('pageload') resolves a Page", async () => {
      const context = browser.defaultContext();
      const [p] = await Promise.all([
        context.waitForEvent("pageload", 5000),
        page.goto("data:text/html,<title>ctx-loadmark</title>"),
      ]);
      expect(typeof (p as any).url).toBe("function");
      expect((p as any).url()).toContain("ctx-loadmark");
    });

    it("context.once('framenavigated') delivers a Frame to the listener", async () => {
      const context = browser.defaultContext();
      const got = new Promise<string>((resolve) => {
        context.once("framenavigated", (frame: any) => resolve(frame.url()));
      });
      await page.goto("data:text/html,<title>once-nav</title>");
      expect(await got).toContain("once-nav");
    });

    it("context.waitForEvent('pageclose') resolves the closed Page", async () => {
      const context = browser.defaultContext();
      const newPage = await context.newPage();
      const [closed] = await Promise.all([
        context.waitForEvent("pageclose", 5000),
        newPage.close(),
      ]);
      expect((closed as any).isClosed()).toBe(true);
    });

    it("browser.waitForEvent('context') resolves the new BrowserContext", async () => {
      // newContext() is synchronous, so arm the wait and let it subscribe
      // (a real async tick) before triggering — otherwise the emit can
      // outrun the lazily-polled waitForEvent future on a multi-thread
      // runtime.
      const waitP = browser.waitForEvent("context", 5000);
      await page.evaluate(() => 1);
      browser.newContext();
      const bcx = await waitP;
      expect(typeof (bcx as any).newPage).toBe("function");
      expect(typeof (bcx as any).cookies).toBe("function");
    });

    it("browser.once('context') delivers the context to the listener", async () => {
      const got = new Promise<boolean>((resolve) => {
        browser.once("context", (bcx: any) => resolve(typeof bcx.newPage === "function"));
      });
      browser.newContext();
      expect(await got).toBe(true);
    });
  });
}
