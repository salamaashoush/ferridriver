/**
 * NAPI parity tests for the retained-observation surface:
 * `page.consoleMessages()` / `page.clearConsoleMessages()` /
 * `page.pageErrors()` / `page.clearPageErrors()` / `page.requestGC()`
 * and `locator.describe()`.
 *
 * Playwright sources:
 * `/tmp/playwright/packages/playwright-core/src/client/page.ts`
 * (consoleMessages / pageErrors / requestGC) and `client/locator.ts`
 * (describe). The default filter is `since-navigation`; `'all'` spans
 * the page lifetime.
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : (["cdp-pipe", "cdp-raw"] as const);

async function pollUntil<T>(
  fn: () => Promise<T>,
  pred: (v: T) => boolean,
  timeoutMs = 5_000,
): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  let last = await fn();
  while (!pred(last) && Date.now() < deadline) {
    await new Promise((r) => setTimeout(r, 50));
    last = await fn();
  }
  return last;
}

for (const backend of BACKENDS) {
  describe(`[${backend}] page.consoleMessages / pageErrors / requestGC`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
      page = await browser.newPage();
      await page.goto("data:text/html,<h1>x</h1>", null);
    }, 30_000);

    afterAll(async () => {
      await browser?.close();
    });

    it("consoleMessages: since-navigation window vs filter:'all'", async () => {
      await page.clearConsoleMessages();
      await page.evaluate(() => console.log("before-nav-msg"));
      await pollUntil(
        () => page.consoleMessages({ filter: "all" }),
        (m) => m.some((x) => x.text().includes("before-nav-msg")),
      );
      await page.reload(null);
      await page.evaluate(() => console.log("after-nav-msg"));
      const since = await pollUntil(
        () => page.consoleMessages(),
        (m) => m.some((x) => x.text().includes("after-nav-msg")),
      );
      expect(since.some((m) => m.text().includes("after-nav-msg"))).toBe(true);
      expect(since.some((m) => m.text().includes("before-nav-msg"))).toBe(
        false,
      );
      const all = await page.consoleMessages({ filter: "all" });
      expect(all.some((m) => m.text().includes("before-nav-msg"))).toBe(true);
      expect(all.some((m) => m.text().includes("after-nav-msg"))).toBe(true);
      await page.clearConsoleMessages();
      expect((await page.consoleMessages({ filter: "all" })).length).toBe(0);
    });

    it("pageErrors: retained as native Error; clearPageErrors empties", async () => {
      await page.clearPageErrors();
      await page.evaluate(() => {
        setTimeout(() => {
          throw new Error("retained-boom");
        }, 5);
      });
      const errs = await pollUntil(
        () => page.pageErrors(),
        (e) => e.some((x) => x.message.includes("retained-boom")),
      );
      const hit = errs.find((e) => e.message.includes("retained-boom"));
      expect(hit).toBeDefined();
      expect(hit instanceof Error).toBe(true);
      expect(hit!.name).toBe("Error");
      await page.clearPageErrors();
      expect((await page.pageErrors({ filter: "all" })).length).toBe(0);
    });

    it("requestGC collects an unreachable WeakRef referent", async () => {
      await page.evaluate(() => {
        (globalThis as any).objectToDestroy = { hello: "world" };
        (globalThis as any).weakRef = new WeakRef(
          (globalThis as any).objectToDestroy,
        );
      });
      await page.requestGC();
      expect(
        await page.evaluate(() =>
          (globalThis as any).weakRef.deref() ? "live" : "collected",
        ),
      ).toBe("live");
      await page.evaluate(() => {
        (globalThis as any).objectToDestroy = null;
      });
      let after = "live";
      for (let i = 0; i < 10 && after === "live"; i++) {
        await page.requestGC();
        after = (await page.evaluate(() =>
          (globalThis as any).weakRef.deref() ? "live" : "collected",
        )) as string;
      }
      expect(after).toBe("collected");
    });

    it("locator.describe keeps matching and acting", async () => {
      await page.goto(
        "data:text/html,<button id='go' onclick='window.__describedClick=1'>Go</button>",
        null,
      );
      const described = page.locator("#go").describe("the go button");
      expect(await described.count()).toBe(1);
      await described.click(null);
      expect(
        await page.evaluate(() => (window as any).__describedClick === 1),
      ).toBe(true);
    });
  });
}
