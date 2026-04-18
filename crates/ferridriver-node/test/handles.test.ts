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

    it("JSHandle.asElement returns null in phase C (placeholder)", async () => {
      const eh = await page.querySelector("button#primary");
      expect(eh).not.toBeNull();
      const jh = eh!.asJsHandle();
      // Phase C: the JSHandle layer can't yet distinguish DOM from
      // non-DOM remotes without the phase-D remote-type inspection.
      // Phase D changes this test to expect a non-null result for
      // element-typed remotes.
      expect(jh.asElement()).toBeNull();
      await eh!.dispose();
    });

    // ── Phase D: evaluate(fn, arg) + evaluateHandle ──

    it("page.evaluateWithArg runs fn with primitive arg", async () => {
      const result = await page.evaluateWithArg("x => x + 1", 41);
      expect(result).toBe(42);
    });

    it("page.evaluateWithArg accepts no arg", async () => {
      const result = await page.evaluateWithArg("() => 7", null);
      expect(result).toBe(7);
    });

    it("page.evaluateWithArg runs fn with object arg", async () => {
      const result = await page.evaluateWithArg("o => o.x + o.y", { x: 3, y: 4 });
      expect(result).toBe(7);
    });

    it("page.evaluateWithArg runs fn with array arg (roundtrip via isomorphic wire)", async () => {
      const result = await page.evaluateWithArg(
        "a => a.reduce((s, n) => s + n, 0)",
        [1, 2, 3, 4]
      );
      expect(result).toBe(10);
    });

    it("page.evaluateWithArgWire surfaces rich types (Date) via wire shape", async () => {
      // Date has no JSON form; evaluateWithArgWire returns the
      // isomorphic wire shape so callers can pluck the ISO string.
      const wire = await page.evaluateWithArgWire(
        "() => new Date('2024-06-01T00:00:00.000Z')",
        null
      );
      expect(wire).toEqual({ d: "2024-06-01T00:00:00.000Z" });
    });

    it("page.evaluateHandleWithArg returns a live JSHandle", async () => {
      const handle = await page.evaluateHandleWithArg("() => document.body", null);
      expect(handle.isDisposed).toBe(false);
      await handle.dispose();
      expect(handle.isDisposed).toBe(true);
    });

    it("handle.evaluateWithArg runs fn with the handle as `this`/arg", async () => {
      const handle = await page.evaluateHandleWithArg("() => document.body", null);
      const tagName = await handle.evaluateWithArg("el => el.tagName", null);
      expect(tagName).toBe("BODY");
      await handle.dispose();
    });

    it("ElementHandle.evaluateWithArg delegates through the JSHandle path", async () => {
      const eh = await page.querySelector("button#primary");
      expect(eh).not.toBeNull();
      const tag = await eh!.evaluateWithArg("el => el.tagName", null);
      expect(tag).toBe("BUTTON");
      await eh!.dispose();
    });

    it("evaluate on a disposed handle raises", async () => {
      const eh = await page.querySelector("button#primary");
      expect(eh).not.toBeNull();
      const jh = eh!.asJsHandle();
      await eh!.dispose();
      // Both the ElementHandle and its JSHandle companion should
      // refuse subsequent evaluate calls with the disposed error —
      // Playwright contract.
      let threw = false;
      try {
        await jh.evaluateWithArg("el => el.tagName", null);
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
