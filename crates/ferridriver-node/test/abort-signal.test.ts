/**
 * The `signal` option: an operation given an AbortSignal stops when it
 * fires and rejects with AbortError, the signal's reason as its cause.
 * Ported from Playwright's page-click.spec.ts ("should abort via signal").
 */
import { describe, it, expect, beforeAll, afterAll, beforeEach, afterEach } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const backend = process.env.FERRIDRIVER_BACKEND ?? "cdp-pipe";

describe(`[${backend}] signal option`, () => {
  let browser: Browser;
  let page: Page;

  beforeAll(async () => {
    browser = await launchForBackend(backend);
  });

  beforeEach(async () => {
    page = await browser.newPage();
  });

  afterEach(async () => {
    await page.close();
  });

  afterAll(async () => {
    await browser.close();
  });

  it("aborts a click in flight", async () => {
    await page.setContent(`<button style="display:none">click me</button>`);
    const controller = new AbortController();
    const promise = page
      .locator("button")
      .click({ signal: controller.signal, timeout: 0 })
      .catch((e: Error) => e);
    await page.waitForTimeout(300);
    const reason = new Error("foo bar");
    controller.abort(reason);
    const error = (await promise) as Error;
    expect(error.message).toStartWith("AbortError: foo bar");
    expect(error.message).toMatch(/Call log:[\s\S]*operation was aborted: foo bar/);
    expect(error.cause).toBe(reason);
  });

  it("hands a string reason back as the cause", async () => {
    await page.setContent(`<button style="display:none">click me</button>`);
    const controller = new AbortController();
    const promise = page.locator("button").click({ signal: controller.signal, timeout: 0 });
    controller.abort("aborted by user");
    const error = (await promise.catch((e: Error) => e)) as Error;
    expect(error).toBeInstanceOf(Error);
    expect(error.message).toStartWith("AbortError: aborted by user");
    expect(error.cause).toBe("aborted by user");
  });

  it("refuses an already-aborted signal", async () => {
    await page.setContent(`<button onclick="window.clicked = true">click me</button>`);
    const controller = new AbortController();
    const reason = new Error("Already aborted");
    controller.abort(reason);
    const error = (await page
      .locator("button")
      .click({ signal: controller.signal })
      .catch((e: Error) => e)) as Error;
    expect(error.message).toStartWith("AbortError: The operation was aborted");
    expect(error.cause).toBe(reason);
    expect(await page.evaluate("window.clicked")).toBeFalsy();
  });

  it("aborts the waits", async () => {
    await page.setContent(`<div id="root"></div>`);
    const aborted = async (start: (signal: AbortSignal) => Promise<unknown>, reason: string) => {
      const controller = new AbortController();
      const promise = start(controller.signal).catch((e: Error) => e);
      setTimeout(() => controller.abort(reason), 200);
      const error = (await promise) as Error;
      expect(error).toBeInstanceOf(Error);
      expect(error.message).toStartWith(`AbortError: ${reason}`);
      expect(error.cause).toBe(reason);
    };
    await aborted(signal => page.waitForSelector("#never", { signal, timeout: 0 }), "stop the selector");
    await aborted(signal => page.locator("#never").waitFor({ signal, timeout: 0 }), "stop the locator");
    await aborted(signal => page.waitForFunction("false", undefined, { signal, timeout: 0 }), "stop the function");
    await aborted(signal => page.waitForEvent("console", { signal, timeout: 20000 }), "stop the event");
    const root = (await page.$("#root"))!;
    await aborted(signal => root.waitForSelector("#never", { signal, timeout: 0 }), "stop the handle");
  });

  it("aborts a navigation", async () => {
    let release = () => {};
    const held = new Promise<void>(resolve => (release = resolve));
    const server = Bun.serve({
      port: 0,
      hostname: "127.0.0.1",
      fetch: async () => {
        await held;
        return new Response("late");
      },
    });
    try {
      const controller = new AbortController();
      const reason = new Error("navigation cancelled");
      const goto = page.goto(`http://127.0.0.1:${server.port}/held`, { signal: controller.signal }).catch((e: Error) => e);
      setTimeout(() => controller.abort(reason), 200);
      const error = (await goto) as Error;
      expect(error.message).toStartWith("AbortError: navigation cancelled");
      expect(error.cause).toBe(reason);
    } finally {
      release();
      await server.stop(true);
    }
  });

  it("a signal that never fires changes nothing and keeps no listener", async () => {
    await page.setContent(`<button onclick="window.clicked = true">click me</button>`);
    const signal = new AbortController().signal;
    const listeners = new Set<unknown>();
    const add = signal.addEventListener.bind(signal);
    const remove = signal.removeEventListener.bind(signal);
    signal.addEventListener = ((type: string, listener: EventListener, options?: AddEventListenerOptions) => {
      listeners.add(listener);
      add(type, listener, options);
    }) as typeof signal.addEventListener;
    signal.removeEventListener = ((type: string, listener: EventListener) => {
      listeners.delete(listener);
      remove(type, listener);
    }) as typeof signal.removeEventListener;
    await page.locator("button").click({ signal });
    expect(await page.evaluate("window.clicked")).toBe(true);
    expect(listeners.size).toBe(0);
  });
});
