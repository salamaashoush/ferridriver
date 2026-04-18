// Phase-C JSHandle + ElementHandle lifecycle tests. Rule 9: prove on every
// backend (cdp-pipe, cdp-raw, webkit) that:
//   - page.querySelector(sel) mints an ElementHandle (or null when the
//     selector matches no element),
//   - handle.dispose() latches handle.isDisposed → true,
//   - handle.dispose() is idempotent (second call returns successfully,
//     without raising and without a second backend round-trip observable
//     at the test layer),
//   - handle.asJSHandle() yields a JSHandle whose dispose flag is shared
//     with the originating ElementHandle,
//   - the backend's release call reaches the page — for WebKit the
//     `window.__wr` Map size drops after dispose, which is the
//     protocol-side effect that proves Op::ReleaseRef ran end-to-end.
//
// CDP's `Runtime.releaseObject` and BiDi's `script.disown` don't expose
// a page-side effect the test can observe directly; phase-D will add the
// use-after-dispose path that fails with "No object with id ..." on CDP
// and "invalid argument" on BiDi. For now we verify the dispose call
// completes without error on every backend (the backend error would
// surface synchronously via our `release_handle` dispatch).
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { Browser, type Page } from "../index.js";
import { createServer, type Server } from "node:http";

let testServer: Server;
let testUrl: string;

beforeAll(async () => {
  testServer = createServer((_req, res) => {
    res.writeHead(200, { "Content-Type": "text/html" });
    res.end(
      `<!DOCTYPE html><html><head><title>Handles</title></head>` +
        `<body>` +
        `<button id="primary">Primary</button>` +
        `<a href="/about">About</a>` +
        `<div class="needle">match</div>` +
        `</body></html>`
    );
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
  describe(`[${backend}] JSHandle / ElementHandle lifecycle`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await Browser.launch({ backend });
      page = await browser.newPageWithUrl(testUrl);
    });

    afterAll(async () => {
      await browser.close();
    });

    it("querySelector returns an ElementHandle for a matching selector", async () => {
      const handle = await page.querySelector("button#primary");
      expect(handle).not.toBeNull();
      expect(handle!.isDisposed).toBe(false);
      await handle!.dispose();
    });

    it("querySelector returns null for a missing selector", async () => {
      const handle = await page.querySelector("button#does-not-exist");
      expect(handle).toBeNull();
    });

    it("$ alias is equivalent to querySelector", async () => {
      const handle = await page.$("div.needle");
      expect(handle).not.toBeNull();
      await handle!.dispose();
    });

    it("dispose() latches isDisposed to true", async () => {
      const handle = await page.querySelector("button#primary");
      expect(handle).not.toBeNull();
      expect(handle!.isDisposed).toBe(false);
      await handle!.dispose();
      expect(handle!.isDisposed).toBe(true);
    });

    it("dispose() is idempotent (second call succeeds)", async () => {
      const handle = await page.querySelector("button#primary");
      expect(handle).not.toBeNull();
      await handle!.dispose();
      // Second dispose() must not reject — idempotence is a Playwright
      // contract and is implemented via the shared AtomicBool flag.
      await handle!.dispose();
      expect(handle!.isDisposed).toBe(true);
    });

    it("asJSHandle() shares the dispose flag with the ElementHandle", async () => {
      const eh = await page.querySelector("button#primary");
      expect(eh).not.toBeNull();
      const jh = eh!.asJsHandle();
      expect(jh.isDisposed).toBe(false);
      await eh!.dispose();
      // Dispose on ElementHandle releases the same remote the sibling
      // JSHandle points at. Both flags are backed by the same
      // Arc<AtomicBool> in core, so the observation latches through.
      expect(jh.isDisposed).toBe(true);
      // A second dispose through the companion JSHandle is a no-op.
      await jh.dispose();
    });

    it("JSHandle.asElement returns an ElementHandle for DOM-node remotes", async () => {
      const eh = await page.querySelector("button#primary");
      expect(eh).not.toBeNull();
      const jh = eh!.asJsHandle();
      const promoted = await jh.asElement();
      expect(promoted).not.toBeNull();
      await eh!.dispose();
    });

    it("JSHandle.asElement returns null for non-DOM remotes", async () => {
      const jh = await page.evaluateHandle("() => ({ not: 'a dom node' })", null);
      const promoted = await jh.asElement();
      expect(promoted).toBeNull();
      await jh.dispose();
    });

    // ── Phase D: evaluate(fn, arg) + evaluateHandle ──

    it("page.evaluate runs fn with primitive arg", async () => {
      const result = await page.evaluate((x: number) => x + 1, 41);
      expect(result).toBe(42);
    });

    it("page.evaluate accepts no arg", async () => {
      const result = await page.evaluate(() => 7);
      expect(result).toBe(7);
    });

    it("page.evaluate runs fn with object arg", async () => {
      const result = await page.evaluate((o: { x: number; y: number }) => o.x + o.y, { x: 3, y: 4 });
      expect(result).toBe(7);
    });

    it("page.evaluate runs fn with array arg (roundtrip via isomorphic wire)", async () => {
      const result = await page.evaluate(
        (a: number[]) => a.reduce((s, n) => s + n, 0),
        [1, 2, 3, 4]
      );
      expect(result).toBe(10);
    });

    it("page.evaluate rehydrates rich types (Date) to native JS", async () => {
      // Mirrors Playwright's `parseResult` — Date arrives as a real
      // `Date` instance, not the wire `{d: iso}` shape.
      const d = (await page.evaluate(() => new Date("2024-06-01T00:00:00.000Z"))) as Date;
      expect(d instanceof Date).toBe(true);
      expect(d.toISOString()).toBe("2024-06-01T00:00:00.000Z");
    });

    it("page.evaluate accepts a string expression (Playwright parity)", async () => {
      // `typeof pageFunction === 'function'` is false for strings, so
      // the backend evaluates as expression — matches Playwright's
      // `evaluateExpression({ isFunction: false })` path.
      const result = await page.evaluate("1 + 1");
      expect(result).toBe(2);
    });

    it("page.evaluateHandle returns a live JSHandle", async () => {
      const handle = await page.evaluateHandle(() => document.body);
      expect(handle.isDisposed).toBe(false);
      await handle.dispose();
      expect(handle.isDisposed).toBe(true);
    });

    it("handle.evaluate runs fn with the handle as `this`/arg", async () => {
      const handle = await page.evaluateHandle(() => document.body);
      const tagName = await handle.evaluate((el: Element) => el.tagName);
      expect(tagName).toBe("BODY");
      await handle.dispose();
    });

    it("ElementHandle.evaluate delegates through the JSHandle path", async () => {
      const eh = await page.querySelector("button#primary");
      expect(eh).not.toBeNull();
      const tag = await eh!.evaluate((el: Element) => el.tagName);
      expect(tag).toBe("BUTTON");
      await eh!.dispose();
    });

    // ── Phase E: ElementHandle action methods ──

    it("ElementHandle reads: innerHTML / innerText / textContent / getAttribute", async () => {
      await page.goto(
        `data:text/html,<a id="link" href="/x" data-k="v">hello <b>world</b></a>`
      );
      const eh = await page.querySelector("a#link");
      expect(eh).not.toBeNull();
      expect(await eh!.innerHtml()).toContain("<b>world</b>");
      expect(await eh!.innerText()).toBe("hello world");
      expect(await eh!.textContent()).toBe("hello world");
      expect(await eh!.getAttribute("href")).toBe("/x");
      expect(await eh!.getAttribute("data-k")).toBe("v");
      expect(await eh!.getAttribute("missing")).toBeNull();
      await eh!.dispose();
    });

    it("ElementHandle inputValue for <input>", async () => {
      await page.goto(
        `data:text/html,<input id="i" value="hello" />`
      );
      const eh = await page.querySelector("#i");
      expect(eh).not.toBeNull();
      expect(await eh!.inputValue()).toBe("hello");
      await eh!.dispose();
    });

    it("ElementHandle state predicates (visible/hidden/disabled/enabled)", async () => {
      await page.goto(
        `data:text/html,<button id="v">visible</button><button id="d" disabled>disabled</button><button id="h" style="display:none">hidden</button>`
      );
      const v = await page.querySelector("#v");
      const d = await page.querySelector("#d");
      const h = await page.querySelector("#h");
      expect(v).not.toBeNull();
      expect(d).not.toBeNull();
      expect(h).not.toBeNull();

      expect(await v!.isVisible()).toBe(true);
      expect(await v!.isHidden()).toBe(false);
      expect(await v!.isEnabled()).toBe(true);
      expect(await v!.isDisabled()).toBe(false);

      expect(await d!.isDisabled()).toBe(true);
      expect(await d!.isEnabled()).toBe(false);

      expect(await h!.isVisible()).toBe(false);
      expect(await h!.isHidden()).toBe(true);

      await v!.dispose();
      await d!.dispose();
      await h!.dispose();
    });

    it("ElementHandle isChecked on input and aria-checked", async () => {
      await page.goto(
        `data:text/html,<input type="checkbox" id="c1" checked><input type="checkbox" id="c2"><div id="c3" role="checkbox" aria-checked="true"></div>`
      );
      const c1 = await page.querySelector("#c1");
      const c2 = await page.querySelector("#c2");
      const c3 = await page.querySelector("#c3");
      expect(await c1!.isChecked()).toBe(true);
      expect(await c2!.isChecked()).toBe(false);
      expect(await c3!.isChecked()).toBe(true);
      await c1!.dispose();
      await c2!.dispose();
      await c3!.dispose();
    });

    it("ElementHandle isEditable on input vs disabled vs readonly vs contenteditable", async () => {
      await page.goto(
        `data:text/html,<input id="i" /><input id="d" disabled /><input id="r" readonly /><div id="e" contenteditable="true"></div>`
      );
      expect(await (await page.querySelector("#i"))!.isEditable()).toBe(true);
      expect(await (await page.querySelector("#d"))!.isEditable()).toBe(false);
      expect(await (await page.querySelector("#r"))!.isEditable()).toBe(false);
      expect(await (await page.querySelector("#e"))!.isEditable()).toBe(true);
    });

    it("ElementHandle boundingBox returns a rect or null", async () => {
      await page.goto(
        `data:text/html,<button id="b" style="position:absolute;left:10px;top:20px;width:50px;height:30px;">b</button>`
      );
      const b = await page.querySelector("#b");
      expect(b).not.toBeNull();
      const box = await b!.boundingBox();
      expect(box).not.toBeNull();
      expect(box!.width).toBeGreaterThan(0);
      expect(box!.height).toBeGreaterThan(0);
      await b!.dispose();
    });

    it("ElementHandle.click fires native click handler", async () => {
      await page.goto(
        `data:text/html,<button id="b" onclick="document.title='clicked'">b</button>`
      );
      const b = await page.querySelector("#b");
      expect(b).not.toBeNull();
      await b!.click();
      // Give the event loop a tick to settle the title update.
      await new Promise((r) => setTimeout(r, 50));
      expect(await page.title()).toBe("clicked");
      await b!.dispose();
    });

    it("ElementHandle.focus updates document.activeElement", async () => {
      await page.goto(`data:text/html,<input id="i" />`);
      const i = await page.querySelector("#i");
      expect(i).not.toBeNull();
      await i!.focus();
      const active = await page.evaluate(
        "document.activeElement && document.activeElement.id"
      );
      expect(active).toBe("i");
      await i!.dispose();
    });

    // ── Disposed-handle error path (kept at the end of E) ──

    it("evaluate on a disposed handle raises", async () => {
      // Navigate back to the baseline URL — earlier Phase E tests
      // took the page to various data: URLs without a #primary button.
      await page.goto(testUrl);
      const eh = await page.querySelector("button#primary");
      // The baseline testServer response doesn't carry a #primary
      // button — use `a` (first link) as a stable target instead.
      const handle = eh ?? (await page.querySelector("a"));
      expect(handle).not.toBeNull();
      const jh = handle!.asJsHandle();
      await handle!.dispose();
      // Both the ElementHandle and its JSHandle companion should
      // refuse subsequent evaluate calls with the disposed error —
      // Playwright contract.
      let threw = false;
      try {
        await jh.evaluate("el => el.tagName", null);
      } catch (e: any) {
        threw = true;
        expect(String(e.message)).toContain("disposed");
      }
      expect(threw).toBe(true);
    });

    if (backend === "webkit") {
      it("WebKit Op::ReleaseRef observably shrinks window.__wr", async () => {
        // WebKit keeps all live element handles in a per-page `window.__wr`
        // Map. If Op::ReleaseRef reached the host, the Map's size decreases
        // by exactly one after dispose. That's the only page-observable
        // side effect of the release path — and it proves the IPC op
        // round-tripped the full host/Rust boundary, not just a Rust-side
        // flag flip.
        const fresh = await browser.newPageWithUrl(testUrl);
        try {
          const sizeBefore = Number(
            (await fresh.evaluate("window.__wr ? window.__wr.size : 0")) ?? 0
          );
          const handle = await fresh.querySelector("button#primary");
          expect(handle).not.toBeNull();
          const sizeDuring = Number(
            (await fresh.evaluate("window.__wr ? window.__wr.size : 0")) ?? 0
          );
          expect(sizeDuring).toBe(sizeBefore + 1);
          await handle!.dispose();
          const sizeAfter = Number(
            (await fresh.evaluate("window.__wr ? window.__wr.size : 0")) ?? 0
          );
          expect(sizeAfter).toBe(sizeBefore);
        } finally {
          await fresh.close();
        }
      });
    }
  });
}
