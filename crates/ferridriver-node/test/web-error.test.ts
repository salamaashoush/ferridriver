/**
 * NAPI parity tests for the WebError lifecycle handle.
 *
 * `page.waitForEvent('pageerror')` returns a live `WebError` class
 * instance with `error()` -> `{ name, message, stack }` and
 * `page()` -> `Page | null`. Context-level `'weberror'` fan-out is
 * wired via the per-page → per-context bridge installed in
 * `Page::with_context`, so `context.waitForEvent('weberror')` observes
 * errors from every page in the context.
 *
 * Dispatch paths:
 * * CDP — `Runtime.exceptionThrown` → `exceptionToError` → live handle.
 * * BiDi — `log.entryAdded` with `type: 'javascript' + level: 'error'`.
 * * WebKit — host userScript captures `window.onerror` /
 *   `unhandledrejection` and forwards through the existing console IPC
 *   with `level: 'pageerror'`; the Rust drainer splits the payload.
 *
 * Gated to CDP backends here; the Rust integration suite
 * (`tests/backends_support/web_error.rs`) covers all four backends.
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { Browser, type Page, type BrowserContext } from "../index.js";

const BACKENDS = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : (["cdp-pipe", "cdp-raw"] as const);

type WebError = {
  page(): unknown;
  error(): { name: string; message: string; stack: string };
};

for (const backend of BACKENDS) {
  describe(`[${backend}] WebError as first-class handle (§2.13)`, () => {
    let browser: Browser;
    let page: Page;
    let context: BrowserContext;

    beforeAll(async () => {
      browser = await Browser.launch({ backend });
      context = browser.defaultContext();
      page = await browser.newPage();
    }, 30_000);

    afterAll(async () => {
      await browser?.close();
    });

    it("page.waitForEvent('pageerror') surfaces { name, message, stack }", async () => {
      const waiter = page.waitForEvent("pageerror", 5_000);
      await page.goto(
        "data:text/html,<script>throw new Error('boom')</script>",
        null,
      );
      const err = (await waiter) as unknown as WebError;
      const d = err.error();
      expect(d.name).toBe("Error");
      expect(d.message).toBe("boom");
      expect(typeof d.stack).toBe("string");
    });

    it("page() back-reference returns the owning Page", async () => {
      const waiter = page.waitForEvent("pageerror", 5_000);
      await page.goto(
        "data:text/html,<script>throw new Error('ref-test')</script>",
        null,
      );
      const err = (await waiter) as unknown as WebError;
      const p = err.page();
      expect(p).not.toBeNull();
    });

    it("TypeError name survives the CDP 'exception.description' split", async () => {
      const waiter = page.waitForEvent("pageerror", 5_000);
      await page.goto(
        "data:text/html,<script>throw new TypeError('nope')</script>",
        null,
      );
      const err = (await waiter) as unknown as WebError;
      const d = err.error();
      expect(d.name).toBe("TypeError");
      expect(d.message).toBe("nope");
    });

    it("context.waitForEvent('weberror') forwards page errors from every page", async () => {
      const waiter = context.waitForEvent("weberror", 5_000);
      await page.goto(
        "data:text/html,<script>throw new RangeError('ctx-route')</script>",
        null,
      );
      const err = (await waiter) as unknown as WebError;
      const d = err.error();
      expect(d.name).toBe("RangeError");
      expect(d.message).toBe("ctx-route");
    });

    it("page.on('pageerror') callback gets a plain { name, message, stack } snapshot", async () => {
      const received: Array<{ name: string; message: string; stack: string }> =
        [];
      const id = page.on("pageerror", (data) => {
        received.push(
          data as unknown as { name: string; message: string; stack: string },
        );
      });
      try {
        await page.goto(
          "data:text/html,<script>throw new Error('callback-path')</script>",
          null,
        );
        // Give the async event loop a tick to deliver the callback.
        const deadline = Date.now() + 3_000;
        while (received.length === 0 && Date.now() < deadline) {
          await new Promise((r) => setTimeout(r, 50));
        }
      } finally {
        page.off(id);
      }
      expect(received.length).toBeGreaterThanOrEqual(1);
      const last = received[received.length - 1]!;
      expect(last.name).toBe("Error");
      expect(last.message).toBe("callback-path");
    });
  });
}
