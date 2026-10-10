/**
 * `exposeFunctions` on evaluate and addInitScript, and the argument
 * serialization behind it: rich values, handles nested in objects, cycles.
 */
import { describe, it, expect, beforeAll, afterAll, beforeEach, afterEach } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const backend = process.env.FERRIDRIVER_BACKEND ?? "cdp-pipe";

describe(`[${backend}] exposeFunctions`, () => {
  let browser: Browser;
  let page: Page;

  beforeAll(async () => {
    browser = await launchForBackend(backend);
  });

  afterAll(async () => {
    await browser.close();
  });

  beforeEach(async () => {
    page = await browser.newPage();
  });

  afterEach(async () => {
    await page.close();
  });

  it("rejects a function without the option", async () => {
    const error = (await page.evaluate("({ cb }) => cb()", { cb: () => {} }).catch((e: Error) => e)) as Error;
    expect(error.message).toContain('Attempting to serialize unexpected value at position "cb": () => {}');
  });

  it("calls back into a function and gets its result", async () => {
    const received: number[] = [];
    const result = await page.evaluate(
      async ({ cb, double }: any) => {
        await cb(1);
        return await double(21);
      },
      { cb: (n: number) => { received.push(n); }, double: async (n: number) => n * 2 },
      { exposeFunctions: true },
    );
    expect(result).toBe(42);
    expect(received).toEqual([1]);
  });

  it("propagates a callback error to the page", async () => {
    const message = await page.evaluate(
      async (cb: any) => {
        try {
          await cb();
          return "no error";
        } catch (e) {
          return (e as Error).message;
        }
      },
      async () => { throw new Error("boom"); },
      { exposeFunctions: true },
    );
    expect(message).toContain("boom");
  });

  it("works through locator.evaluate", async () => {
    await page.setContent("<div id=target>7</div>");
    const result = await page
      .locator("#target")
      .evaluate(async (element: Element, { double }: any) => await double(+element.textContent!), { double: async (n: number) => n * 2 }, { exposeFunctions: true });
    expect(result).toBe(14);
  });

  it("serializes rich values, nested handles and cycles", async () => {
    await page.setContent("<div id=target>x</div>");
    const element = await page.$("#target");
    const nested: Record<string, unknown> = {};
    nested.self = nested;
    const result = await page.evaluate(
      ({ when, re, url, big, bytes, error, holder, nested }: any) => [
        when instanceof Date && when.getTime(),
        re instanceof RegExp && re.flags,
        url instanceof URL && url.pathname,
        typeof big,
        bytes instanceof Uint16Array && Array.from(bytes),
        error instanceof Error && error.message,
        holder.el.id,
        nested.self === nested,
      ],
      { when: new Date(5), re: /a/gi, url: new URL("https://example.com/path"), big: 10n, bytes: new Uint16Array([1, 2]), error: new Error("bad"), holder: { el: element }, nested },
    );
    expect(result).toEqual([5, "gi", "/path", "bigint", [1, 2], "bad", "target", true]);
  });

  it("passes functions to an init script", async () => {
    const received: number[] = [];
    await page.addInitScript(
      async ({ cb }: any) => {
        (window as any).result = await cb(41);
      },
      { cb: (n: number) => { received.push(n); return n + 1; } },
      { exposeFunctions: true },
    );
    await page.goto("data:text/html,<p>x</p>");
    for (let i = 0; i < 50 && received.length === 0; i++) await new Promise((r) => setTimeout(r, 20));
    expect(received).toEqual([41]);
    expect(await page.evaluate("window.result")).toBe(42);
  });

  it("drops functions from a plain init script argument", async () => {
    await page.addInitScript((arg: any) => { (window as any).seen = typeof arg.cb + ":" + arg.n; }, { cb: () => {}, n: 1 });
    await page.goto("data:text/html,<p>x</p>");
    expect(await page.evaluate("window.seen")).toBe("undefined:1");
  });
});
