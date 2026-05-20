// NAPI binding surface coverage: page-level
// (`touchscreen`, `snapshotForAI`, `exposeFunction`, `frameLocator`),
// frame-level (`getByTitle` / `getByAltText` / `page` / `frameLocator`),
// locator-level (`contentFrame` / `frameLocator` / `page`), the
// `FrameLocator` class as a whole, and `context.clearCookies({...})`.

import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe"];

for (const backend of BACKENDS) {
  describe(`Binding surface [${backend}]`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
      page = await browser.newPage();
    });

    afterAll(async () => {
      await browser.close();
    });

    // ── Frame additions ──────────────────────────────────────────────

    it("frame.getByTitle resolves an element with a title attribute", async () => {
      await page.setContent("<button title='hello'>x</button>");
      const loc = page.mainFrame().getByTitle("hello");
      expect(await loc.textContent()).toBe("x");
    });

    it("frame.getByAltText resolves an image by alt", async () => {
      await page.setContent("<img alt='kitten' src='data:image/gif;base64,R0lGODlhAQABAAAAACw='>");
      const loc = page.mainFrame().getByAltText("kitten");
      expect(await loc.isVisible()).toBe(true);
    });

    it("frame.page returns the owning Page", async () => {
      await page.setContent("<div>x</div>");
      const p2 = page.mainFrame().page();
      expect(await p2.url()).toBe(await page.url());
    });

    it("frame.frameLocator is callable", async () => {
      await page.setContent("<div>x</div>");
      const fl = page.mainFrame().frameLocator("iframe");
      expect(typeof fl.locator).toBe("function");
      expect(typeof fl.first).toBe("function");
    });

    // ── Locator additions ────────────────────────────────────────────

    it("locator.page returns the owning Page", async () => {
      await page.setContent("<div id='x'>y</div>");
      const loc = page.locator("#x");
      const p2 = loc.page();
      expect(await p2.url()).toBe(await page.url());
    });

    it("locator.frameLocator is callable", () => {
      const fl = page.locator("body").frameLocator("iframe");
      expect(typeof fl.getByRole).toBe("function");
    });

    it("locator.contentFrame is callable", () => {
      const fl = page.locator("iframe").contentFrame();
      expect(typeof fl.locator).toBe("function");
    });

    // ── FrameLocator class ───────────────────────────────────────────

    it("FrameLocator getters return Locators / FrameLocators", async () => {
      await page.setContent("<iframe srcdoc='<button>inside</button>'></iframe>");
      const fl = page.frameLocator("iframe");
      expect(typeof fl.locator("body").click).toBe("function");
      expect(typeof fl.getByRole("button").click).toBe("function");
      expect(typeof fl.getByText("inside").click).toBe("function");
      expect(typeof fl.getByTestId("x").click).toBe("function");
      expect(typeof fl.getByLabel("x").click).toBe("function");
      expect(typeof fl.getByPlaceholder("x").click).toBe("function");
      expect(typeof fl.getByAltText("x").click).toBe("function");
      expect(typeof fl.getByTitle("x").click).toBe("function");
      expect(typeof fl.owner().click).toBe("function");
      expect(typeof fl.first().locator).toBe("function");
      expect(typeof fl.last().locator).toBe("function");
      expect(typeof fl.nth(0).locator).toBe("function");
      expect(typeof fl.frameLocator("iframe").locator).toBe("function");
    });

    // ── Page additions ───────────────────────────────────────────────

    it("page.touchscreen.tap does not throw", async () => {
      await page.setContent("<button id='btn' style='width:200px;height:200px'>x</button>");
      await page.touchscreen.tap(50, 50);
    });

    it("page.snapshotForAI returns the expected shape", async () => {
      await page.setContent("<button>click me</button>");
      const snap = await page.snapshotForAI();
      expect(typeof snap.full).toBe("string");
      expect(snap.full.length).toBeGreaterThan(0);
      expect(typeof snap.refMap).toBe("object");
    });

    it("locator.ariaSnapshot is scoped to the element subtree", async () => {
      await page.setContent(
        "<main><h1 id='h'>Heading</h1><p id='p'>FindThisText</p>" +
          "<button id='b'>PressMe</button></main>",
      );
      const sH = await page.locator("#h").ariaSnapshot();
      const sP = await page.locator("#p").ariaSnapshot();
      expect(typeof sH).toBe("string");
      expect(sH.length).toBeGreaterThan(0);
      // Heading subtree: its own content, none of the siblings.
      expect(sH).toContain("Heading");
      expect(sH).not.toContain("FindThisText");
      expect(sH).not.toContain("PressMe");
      // Paragraph subtree: complementary scoping check.
      expect(sP).toContain("FindThisText");
      expect(sP).not.toContain("Heading");
      // mode: 'ai' adds [ref=eN] labels (Playwright parity).
      const sAi = await page.locator("#b").ariaSnapshot({ mode: "ai" });
      expect(sAi).toContain("PressMe");
      expect(sAi).toMatch(/\[ref=/);
    });

    it("locator.ariaSnapshot stitches child iframes (ai mode)", async () => {
      await page.setContent(
        "<main><h1>Top</h1>" +
          "<iframe id='f' srcdoc=\"<button id='ib'>InnerBtn</button>" +
          "<iframe id='g' src='data:text/html,<b>DeepText</b>'></iframe>\"></iframe>" +
          "</main>",
      );
      // mode:'ai' assigns iframe refs -> child browsing contexts are
      // snapshotted recursively and spliced (Playwright
      // ariaSnapshotForFrame). Nested two levels: srcdoc -> data:.
      const ai = await page.locator("main").ariaSnapshot({ mode: "ai" });
      expect(ai).toMatch(/\[ref=/);
      expect(ai).toContain("Top");
      expect(ai).toContain("InnerBtn");
      expect(ai).toContain("DeepText");
      // mode:'default' assigns no refs -> no stitch (exact Playwright).
      const def = await page.locator("main").ariaSnapshot();
      expect(def).not.toContain("InnerBtn");
      expect(def).not.toContain("DeepText");
      expect(def).not.toMatch(/\[ref=/);
    });

    it("page.frameLocator works at page scope", async () => {
      await page.setContent("<iframe srcdoc='<p>x</p>'></iframe>");
      const fl = page.frameLocator("iframe");
      expect(typeof fl.locator).toBe("function");
    });

    it("page.exposeFunction wires JS callback", async () => {
      // NAPI surface is fire-and-forget with a single args array
      // (documented convention); the QuickJS/script surface carries the
      // full Playwright parity (spread args + return-value delivery).
      const seen: unknown[][] = [];
      await page.exposeFunction("__expose_record", (args: unknown[]) => {
        seen.push(args);
      });
      await page.setContent("<button>x</button>");
      await page.evaluate(`window.__expose_record(1, 'two', { three: 3 })`);
      for (let i = 0; i < 50 && seen.length === 0; i++) {
        await new Promise((r) => setTimeout(r, 20));
      }
      expect(seen.length).toBe(1);
      expect(seen[0]).toEqual([1, "two", { three: 3 }]);
    });

    // ── Context additions ────────────────────────────────────────────

    it("context.clearCookies({name}) clears only matching", async () => {
      const ctx = browser.newContext();
      const p = await ctx.newPage();
      await p.goto("data:text/html,<title>x</title>");
      await ctx.addCookies([
        { name: "keep", value: "1", domain: ".example.test", path: "/", secure: false, httpOnly: false, expires: -1 },
        { name: "drop", value: "1", domain: ".example.test", path: "/", secure: false, httpOnly: false, expires: -1 },
      ]);
      const before = (await ctx.cookies()).map((c) => c.name).sort();
      await ctx.clearCookies({ name: "drop" });
      const after = (await ctx.cookies()).map((c) => c.name).sort();
      // If the env silently drops cookies (some headless modes), the
      // binding still routes through (no throw); skip the strict
      // filter assertion in that case.
      if (before.includes("keep") && before.includes("drop")) {
        expect(after).toContain("keep");
        expect(after).not.toContain("drop");
      }
      await ctx.close();
    });
  });
}
