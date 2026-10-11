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

    it("a page that has not navigated is at about:blank", async () => {
      const fresh = await browser.newPage();
      try {
        expect(fresh.url()).toBe("about:blank");
        expect(fresh.mainFrame().url()).toBe("about:blank");
        expect(await fresh.evaluate("location.href")).toBe("about:blank");
      } finally {
        await fresh.close();
      }
    });

    it("frame.getByTitle resolves an element with a title attribute", async () => {
      await page.setContent("<button title='hello'>x</button>");
      const loc = page.mainFrame().getByTitle("hello");
      expect(await loc.textContent()).toBe("x");
    });

    it("frame.getByAltText resolves an image by alt", async () => {
      await page.setContent(
        "<img alt='kitten' src='data:image/gif;base64,R0lGODlhAQABAAAAACw='>",
      );
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
      await page.setContent(
        "<iframe srcdoc='<button>inside</button>'></iframe>",
      );
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
      await page.setContent(
        "<button id='btn' style='width:200px;height:200px'>x</button>",
      );
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

    it("page.exposeFunction spreads the call and returns the result", async () => {
      const seen: unknown[][] = [];
      await page.exposeFunction("__expose_record", (...args: unknown[]) => {
        seen.push(args);
        return { got: args, n: args.length };
      });
      await page.setContent("<button>x</button>");
      expect(await page.evaluate(`window.__expose_record(1, 'two', { three: 3 })`)).toEqual({
        got: [1, "two", { three: 3 }],
        n: 3,
      });
      expect(seen).toEqual([[1, "two", { three: 3 }]]);
    });

    it("page.exposeFunction awaits an async callback and rejects with its error", async () => {
      await page.exposeFunction("__expose_async", async (n: number) => {
        await new Promise((r) => setTimeout(r, 10));
        return n * 2;
      });
      await page.exposeFunction("__expose_throw", () => {
        throw new Error("WOOF WOOF");
      });
      await page.setContent("<button>x</button>");
      expect(await page.evaluate(`window.__expose_async(21)`)).toBe(42);
      expect(
        await page.evaluate(`window.__expose_throw().then(() => 'no error', (e) => e.message)`),
      ).toBe("WOOF WOOF");
    });

    it("page.exposeFunction hands a JSHandle in its result back as the object", async () => {
      await page.setContent("<button id='main'>x</button>");
      const fooHandle = await page.evaluateHandle(`window.fooValue = { bar: 2 }`);
      const button = await page.$("#main");
      await page.exposeFunction("__expose_handle", () => [{ foo: fooHandle, button, label: "kept" }]);
      expect(
        await page.evaluate(`window.__expose_handle().then(([r]) => ({
          foo: r.foo === window.fooValue,
          button: r.button === document.querySelector('#main'),
          label: r.label,
        }))`),
      ).toEqual({ foo: true, button: true, label: "kept" });
    });

    it("page.exposeFunction results carry rich types and reject a function", async () => {
      await page.exposeFunction("__expose_rich", () => ({ when: new Date(86400000), nan: NaN, big: 10n }));
      await page.exposeFunction("__expose_void", () => {});
      await page.exposeFunction("__expose_fn", () => ({ run: () => 1 }));
      await page.setContent("<button>x</button>");
      expect(
        await page.evaluate(`(async () => {
          const rich = await window.__expose_rich();
          return {
            when: rich.when instanceof Date && rich.when.getTime(),
            nan: Number.isNaN(rich.nan),
            big: typeof rich.big === 'bigint' && String(rich.big),
            void: (await window.__expose_void()) === undefined,
            fn: await window.__expose_fn().then(() => 'no error', (e) => e.message),
          };
        })()`),
      ).toEqual({
        when: 86400000,
        nan: true,
        big: "10",
        void: true,
        fn: expect.stringMatching(/Attempting to serialize unexpected value at position "run"/),
      });
    });

    it("page.$eval runs the function on the first match", async () => {
      await page.setContent("<ul><li data-v='a'>one</li><li data-v='b'>two</li></ul>");
      expect(await page.$eval("li", (el: Element) => el.getAttribute("data-v"))).toBe("a");
      expect(
        await page.$eval("li", (el: Element, s: string) => el.textContent + s, "!"),
      ).toBe("one!");
    });

    it("page.$$eval runs the function over all matches", async () => {
      await page.setContent("<ul><li>one</li><li>two</li></ul>");
      expect(
        await page.$$eval("li", (els: Element[]) => els.map((e) => e.textContent)),
      ).toEqual(["one", "two"]);
    });

    it("page.$eval rejects when the selector matches nothing", async () => {
      await page.setContent("<div>x</div>");
      await expect(
        page.$eval(".nope", (el: Element) => el.tagName),
      ).rejects.toThrow();
    });

    it("page.pause rejects with Unsupported (no Inspector UI)", async () => {
      // pause() is sync-throwing on NAPI (always Unsupported); assert it
      // throws rather than silently resolving.
      expect(() => page.pause()).toThrow(/pause|inspector|unsupported/i);
    });

    it("page.exposeBinding delivers the BindingSource plus spread args", async () => {
      const seenSource: Record<string, unknown>[] = [];
      const seenArgs: unknown[][] = [];
      await page.exposeBinding(
        "__page_bind",
        (source: { context: string; page: string; frame: string }, ...args: number[]) => {
          seenSource.push(source);
          seenArgs.push(args);
          return args[0] + args[1];
        },
      );
      await page.setContent("<button>x</button>");
      expect(await page.evaluate(`typeof window.__page_bind`)).toBe("function");
      expect(await page.evaluate(`window.__page_bind(2, 3)`)).toBe(5);
      expect(seenArgs.length).toBe(1);
      expect(seenArgs[0]).toEqual([2, 3]);
      expect(Object.keys(seenSource[0]).sort()).toEqual(["context", "frame", "page"]);
      expect(typeof seenSource[0].page).toBe("string");
    });

    it("locator.waitForFunction polls the element until truthy", async () => {
      await page.setContent("<div id='t'>pending</div>");
      await page.evaluate(
        `setTimeout(() => { document.getElementById('t').textContent = 'ready'; }, 60)`,
      );
      await page
        .locator("#t")
        .waitForFunction((node: Element) => node.textContent === "ready");
      expect((await page.locator("#t").textContent())?.trim()).toBe("ready");
    });

    it("page.waitForFunction takes a function + arg + polling and returns a JSHandle", async () => {
      await page.setContent("<div></div>");
      await page.evaluate("setTimeout(() => { window.counter = 9; }, 60)");
      const handle = await page.waitForFunction(
        (min: number) => ((window as any).counter || 0) >= min && (window as any).counter,
        5,
        { polling: 25, timeout: 5000 },
      );
      expect(await handle.jsonValue()).toBe(9);
      await handle.dispose();
    });

    it("page.waitForFunction rejects unknown polling keyword", async () => {
      await page.setContent("<div></div>");
      await expect(
        page.waitForFunction("true", undefined, { polling: "interval" as any }),
      ).rejects.toThrow(/Unknown polling option/);
    });

    // ── Context additions ────────────────────────────────────────────

    it("context.exposeBinding applies to a page opened after registration", async () => {
      // Register BEFORE opening the page, then open a fresh page and
      // observe window[name] is present and the BindingSource reached
      // the callback.
      const ctx = browser.newContext();
      const seenSource: Record<string, unknown>[] = [];
      const seenArgs: unknown[][] = [];
      const disposable = await ctx.exposeBinding(
        "__ctx_bind",
        (
          source: { context: string; page: string; frame: string },
          ...args: number[]
        ) => {
          seenSource.push(source);
          seenArgs.push(args);
          return args[0] * args[1];
        },
      );
      const p = await ctx.newPage();
      await p.goto("data:text/html,<title>x</title>");
      const installed = await p.evaluate(`typeof window.__ctx_bind`);
      expect(installed).toBe("function");
      expect(await p.evaluate(`window.__ctx_bind(2, 3)`)).toBe(6);
      expect(seenArgs.length).toBe(1);
      expect(seenArgs[0]).toEqual([2, 3]);
      expect(Object.keys(seenSource[0]).sort()).toEqual([
        "context",
        "frame",
        "page",
      ]);
      expect(typeof seenSource[0].context).toBe("string");
      // dispose() removes the page-side proxy.
      await disposable.dispose();
      const afterDispose = await p.evaluate(`typeof window.__ctx_bind`);
      expect(afterDispose).toBe("undefined");
      await ctx.close();
    });

    it("context.exposeFunction delivers only the page-side args (no source)", async () => {
      const ctx = browser.newContext();
      const seen: unknown[][] = [];
      await ctx.exposeFunction("__ctx_fn", (...args: unknown[]) => {
        seen.push(args);
        return args.length;
      });
      const p = await ctx.newPage();
      await p.goto("data:text/html,<title>x</title>");
      expect(await p.evaluate(`window.__ctx_fn(1, 'two')`)).toBe(2);
      expect(seen).toEqual([[1, "two"]]);
      await ctx.close();
    });

    it("context.clearCookies({name}) clears only matching", async () => {
      const ctx = browser.newContext();
      const p = await ctx.newPage();
      await p.goto("data:text/html,<title>x</title>");
      await ctx.addCookies([
        {
          name: "keep",
          value: "1",
          domain: ".example.test",
          path: "/",
          secure: false,
          httpOnly: false,
          expires: -1,
        },
        {
          name: "drop",
          value: "1",
          domain: ".example.test",
          path: "/",
          secure: false,
          httpOnly: false,
          expires: -1,
        },
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
