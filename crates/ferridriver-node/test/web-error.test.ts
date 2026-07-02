/**
 * NAPI parity tests for the WebError lifecycle handle.
 *
 * Playwright public API shapes (verified against
 * `/tmp/playwright/packages/playwright-core/types/types.d.ts`):
 *
 * - `page.on('pageerror', (error: Error) => any)` — native `Error`.
 * - `page.waitForEvent('pageerror'): Promise<Error>` — native `Error`.
 * - `context.on('weberror', (webError: WebError) => any)` — live
 *   `WebError` class instance.
 * - `context.waitForEvent('weberror'): Promise<WebError>` — live
 *   `WebError` class instance.
 * - `WebError.error(): Error` — native `Error` (not a plain object).
 *
 * All five shapes asserted here via `instanceof Error` + constructor
 * instance checks so divergences are caught loudly.
 *
 * Gated to CDP backends; the Rust integration suite
 * (`tests/backends_support/web_error.rs`) covers all four backends.
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page, type BrowserContext, WebError as WebErrorClass } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : (["cdp-pipe", "cdp-raw"] as const);

for (const backend of BACKENDS) {
  describe(`[${backend}] WebError as first-class handle (§2.13)`, () => {
    let browser: Browser;
    let page: Page;
    let context: BrowserContext;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
      context = browser.defaultContext();
      page = await browser.newPage();
    }, 30_000);

    afterAll(async () => {
      await browser?.close();
    });

    it("page.waitForEvent('pageerror') returns a native JS Error", async () => {
      const waiter = page.waitForEvent("pageerror", 5_000);
      await page.goto(
        "data:text/html,<script>throw new Error('boom')</script>",
        null,
      );
      const err = await waiter;
      expect(err).toBeInstanceOf(Error);
      expect((err as Error).name).toBe("Error");
      expect((err as Error).message).toBe("boom");
      expect(typeof (err as Error).stack).toBe("string");
    });

    it("TypeError name survives the CDP 'exception.description' split", async () => {
      const waiter = page.waitForEvent("pageerror", 5_000);
      await page.goto(
        "data:text/html,<script>throw new TypeError('nope')</script>",
        null,
      );
      const err = await waiter;
      expect(err).toBeInstanceOf(Error);
      expect((err as Error).name).toBe("TypeError");
      expect((err as Error).message).toBe("nope");
    });

    it("page.on('pageerror', cb) delivers a native JS Error", async () => {
      const received: Error[] = [];
      const id = page.on("pageerror", (data) => {
        received.push(data as unknown as Error);
      });
      try {
        await page.goto(
          "data:text/html,<script>throw new Error('callback-path')</script>",
          null,
        );
        const deadline = Date.now() + 3_000;
        while (received.length === 0 && Date.now() < deadline) {
          await new Promise((r) => setTimeout(r, 50));
        }
      } finally {
        page.off(id);
      }
      expect(received.length).toBeGreaterThanOrEqual(1);
      const last = received[received.length - 1]!;
      expect(last).toBeInstanceOf(Error);
      expect(last.name).toBe("Error");
      expect(last.message).toBe("callback-path");
    });

    it("context.waitForEvent('weberror') returns a live WebError class", async () => {
      const waiter = context.waitForEvent("weberror", 5_000);
      await page.goto(
        "data:text/html,<script>throw new RangeError('ctx-route')</script>",
        null,
      );
      const webErr = await waiter;
      expect(webErr).toBeInstanceOf(WebErrorClass);
      const err = webErr.error();
      expect(err).toBeInstanceOf(Error);
      expect(err.name).toBe("RangeError");
      expect(err.message).toBe("ctx-route");
    });

    it("context.on('weberror', cb) delivers a live WebError class", async () => {
      const received: WebErrorClass[] = [];
      const id = context.on("weberror", (webErr) => {
        received.push(webErr as WebErrorClass);
      });
      try {
        await page.goto(
          "data:text/html,<script>throw new Error('ctx-callback')</script>",
          null,
        );
        const deadline = Date.now() + 3_000;
        while (received.length === 0 && Date.now() < deadline) {
          await new Promise((r) => setTimeout(r, 50));
        }
      } finally {
        context.off(id);
      }
      expect(received.length).toBeGreaterThanOrEqual(1);
      const last = received[received.length - 1]!;
      expect(last).toBeInstanceOf(WebErrorClass);
      const err = last.error();
      expect(err).toBeInstanceOf(Error);
      expect(err.message).toBe("ctx-callback");
    });

    it("WebError.page() returns the owning Page", async () => {
      const waiter = context.waitForEvent("weberror", 5_000);
      await page.goto(
        "data:text/html,<script>throw new Error('ref-test')</script>",
        null,
      );
      const webErr = await waiter;
      const p = webErr.page();
      expect(p).not.toBeNull();
    });

    it("WebError.location() exposes { url, line, column } (1.60)", async () => {
      const [werr] = await Promise.all([
        context.waitForEvent("weberror", 5000),
        page.evaluate(() => {
          setTimeout(() => {
            throw new Error("boom-loc");
          }, 10);
        }),
      ]);
      expect(werr.error().message).toBe("boom-loc");
      const loc = werr.location();
      expect(typeof loc.url).toBe("string");
      expect(typeof loc.line).toBe("number");
      expect(typeof loc.column).toBe("number");
    });
  });
}
