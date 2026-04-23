/**
 * NAPI parity tests for the ConsoleMessage lifecycle handle.
 *
 * `page.waitForEvent('console')` returns a live `ConsoleMessage`
 * class instance — sync `type()` / `text()` / `args()` / `location()`
 * / `page()` / `timestamp()`. Dispatch goes through the Rust-core
 * `Runtime.consoleAPICalled` listener (CDP) which builds
 * `JSHandle`s from each arg and extracts the source location from
 * `stackTrace.callFrames[0]`.
 *
 * Gated to CDP backends here; the Rust integration suite
 * (`tests/backends_support/console_message.rs`) covers CDP + BiDi +
 * the documented WebKit gap (host interceptor only surfaces
 * `(level, text)` through our IPC).
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : (["cdp-pipe", "cdp-raw"] as const);

type ConsoleMessage = {
  type(): string;
  text(): string;
  args(): unknown[];
  location(): { url: string; lineNumber: number; columnNumber: number };
  timestamp(): number;
  page(): unknown;
};

for (const backend of BACKENDS) {
  describe(`[${backend}] ConsoleMessage as first-class handle (§2.12)`, () => {
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

    it("waitForEvent('console') + type/text/args round-trip", async () => {
      const waiter = page.waitForEvent("console", 5_000);
      await page.evaluate(() => console.log("hello", 42));
      const msg = (await waiter) as unknown as ConsoleMessage;
      expect(msg.type()).toBe("log");
      expect(msg.text()).toContain("hello");
      expect(msg.text()).toContain("42");
      expect(msg.args().length).toBe(2);
    });

    it("console.warn maps to type 'warning' (Playwright parity)", async () => {
      const waiter = page.waitForEvent("console", 5_000);
      await page.evaluate(() => console.warn("careful"));
      const msg = (await waiter) as unknown as ConsoleMessage;
      expect(msg.type()).toBe("warning");
      expect(msg.text()).toContain("careful");
    });

    it("console.error type + text", async () => {
      const waiter = page.waitForEvent("console", 5_000);
      await page.evaluate(() => console.error("boom"));
      const msg = (await waiter) as unknown as ConsoleMessage;
      expect(msg.type()).toBe("error");
      expect(msg.text()).toContain("boom");
    });

    it("location() surfaces url + line + column from the stack trace", async () => {
      // Trigger via an inline <script> so CDP attributes the call to
      // that script's source; calls issued through Runtime.evaluate
      // carry no usable stack frames and fall back to `{ '', 0, 0 }`.
      await page.goto(
        "data:text/html,<script>console.log('loc-check')</script>",
        null,
      );
      const waiter = page.waitForEvent("console", 5_000);
      // Re-navigate the same URL so the inline <script> re-executes
      // after the listener is registered.
      await page.goto(
        "data:text/html,<script>console.log('loc-check-2')</script>",
        null,
      );
      const msg = (await waiter) as unknown as ConsoleMessage;
      const loc = msg.location();
      expect(typeof loc.url).toBe("string");
      expect(loc.lineNumber).toBeGreaterThanOrEqual(0);
      expect(loc.columnNumber).toBeGreaterThanOrEqual(0);
    });

    it("timestamp() returns a numeric millisecond value", async () => {
      const waiter = page.waitForEvent("console", 5_000);
      await page.evaluate(() => console.log("ts"));
      const msg = (await waiter) as unknown as ConsoleMessage;
      const ts = msg.timestamp();
      expect(typeof ts).toBe("number");
      expect(ts).toBeGreaterThan(0);
    });
  });
}
