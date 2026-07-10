// NAPI coverage for the raw CDPSession surface (Playwright
// browser.newBrowserCDPSession / browserContext.newCDPSession).

import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe"];

for (const backend of BACKENDS) {
  const chromium = backend.startsWith("cdp");
  describe(`CDPSession [${backend}]`, () => {
    let browser: Browser;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
    });

    afterAll(async () => {
      await browser.close();
    });

    it("page session: send, protocol events, detach", async () => {
      const page = await browser.newPage();
      const ctx = page.context();
      if (!chromium) {
        await expect(ctx.newCDPSession(page)).rejects.toThrow(/Chromium/);
        return;
      }
      const session = await ctx.newCDPSession(page);
      const result = await session.send("Runtime.evaluate", { expression: "6 * 7", returnByValue: true });
      expect(result.result.value).toBe(42);

      await session.send("Page.enable");
      const loadFired = new Promise<boolean>((resolve) => {
        session.once("Page.loadEventFired", (params: any) => resolve(typeof params.timestamp === "number"));
      });
      await page.goto("data:text/html,<title>cdp-napi</title>");
      expect(await loadFired).toBe(true);

      await session.detach();
      await expect(session.send("Runtime.evaluate", { expression: "1" })).rejects.toThrow(/detached/);
      await expect(session.detach()).rejects.toThrow(/detached/);
    });

    it("browser session: Browser.getVersion", async () => {
      if (!chromium) {
        await expect(browser.newBrowserCDPSession()).rejects.toThrow(/Chromium/);
        return;
      }
      const session = await browser.newBrowserCDPSession();
      const version = await session.send("Browser.getVersion");
      expect(String(version.product)).toContain("Chrome");
      await session.detach();
    });

    it("off removes a listener by identity", async () => {
      if (!chromium) return;
      const page = await browser.newPage();
      const session = await page.context().newCDPSession(page);
      await session.send("Page.enable");
      let calls = 0;
      const listener = () => {
        calls += 1;
      };
      session.on("Page.loadEventFired", listener);
      session.off("Page.loadEventFired", listener);
      await page.goto("data:text/html,<title>off</title>");
      await page.goto("data:text/html,<title>off2</title>");
      expect(calls).toBe(0);
      await session.detach();
    });
  });
}
