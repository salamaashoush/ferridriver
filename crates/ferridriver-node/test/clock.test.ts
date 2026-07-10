// NAPI coverage for context.clock / page.clock (Playwright Clock).

import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe"];

for (const backend of BACKENDS) {
  describe(`Clock [${backend}]`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
      page = await browser.newPage();
    });

    afterAll(async () => {
      await browser.close();
    });

    it("install + pauseAt + runFor drive Date.now and timers", async () => {
      const clock = page.clock;
      await clock.install({ time: new Date("2024-02-02T10:00:00Z") });
      await page.goto("data:text/html,<body>clock</body>");
      await clock.pauseAt("2024-02-02T10:00:05Z");
      expect(Number(await page.evaluate("Date.now()"))).toBe(1706868005000);

      await page.evaluate("window.__fired = 0; setTimeout(() => { window.__fired = Date.now(); }, 2000)");
      await clock.runFor("05");
      expect(Number(await page.evaluate("window.__fired"))).toBe(1706868007000);
      expect(Number(await page.evaluate("Date.now()"))).toBe(1706868010000);
    });

    it("paused time survives navigation (log replay)", async () => {
      await page.goto("data:text/html,<body>two</body>");
      expect(Number(await page.evaluate("Date.now()"))).toBe(1706868010000);
    });

    it("setFixedTime freezes Date.now; fastForward jumps; bad ticks reject", async () => {
      const clock = page.clock;
      await clock.setFixedTime(1234567890000);
      expect(Number(await page.evaluate("Date.now()"))).toBe(1234567890000);
      await clock.setSystemTime(1706868010000);
      await clock.fastForward("01:00");
      await expect(clock.runFor("1:00")).rejects.toThrow(/mm:ss/);
      await expect(clock.pauseAt("not a date")).rejects.toThrow(/Invalid date/);
    });
  });
}
