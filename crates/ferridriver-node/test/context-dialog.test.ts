/**
 * browserContext.on('dialog') / waitForEvent('dialog'): the context hears
 * its pages' dialogs, and a context listener alone keeps one open.
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Dialog } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const backend = process.env.FERRIDRIVER_BACKEND ?? "cdp-pipe";

describe(`[${backend}] context dialog event`, () => {
  let browser: Browser;

  beforeAll(async () => {
    browser = await launchForBackend(backend);
  });

  afterAll(async () => {
    await browser.close();
  });

  it("waitForEvent resolves with the page's dialog", async () => {
    const context = browser.newContext();
    const page = await context.newPage();
    const promise = page.evaluate("prompt('hey?')");
    const dialog = (await context.waitForEvent("dialog")) as Dialog;
    expect(dialog.message()).toBe("hey?");
    await dialog.accept("hello");
    expect(await promise).toBe("hello");
    await context.close();
  });

  it("a context listener alone handles the dialog", async () => {
    const context = browser.newContext();
    const page = await context.newPage();
    context.on("dialog", (dialog) => {
      void (dialog as Dialog).accept("from context");
    });
    expect(await page.evaluate("prompt('who?')")).toBe("from context");
    await context.close();
  });
});
