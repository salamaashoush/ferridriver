/**
 * page.on('popup') / page.waitForEvent('popup'): the opener reports the
 * page it opened.
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const backend = process.env.FERRIDRIVER_BACKEND ?? "cdp-pipe";

describe(`[${backend}] popup event`, () => {
  let browser: Browser;
  let page: Page;

  beforeAll(async () => {
    browser = await launchForBackend(backend);
    page = await browser.newPage();
  });

  afterAll(async () => {
    await browser.close();
  });

  it("waitForEvent resolves with the opened page", async () => {
    const [popup] = await Promise.all([
      page.waitForEvent("popup") as Promise<Page>,
      page.evaluate("window.open('about:blank'); null"),
    ]);
    expect(await popup.evaluate("!!window.opener")).toBe(true);
    expect(await popup.opener()).toBe(page);
  });

  it("page.on delivers the popup", async () => {
    const seen: Page[] = [];
    page.on("popup", (popup) => seen.push(popup as Page));
    await page.evaluate("window.open('about:blank', undefined, 'noopener'); null");
    for (let i = 0; i < 100 && seen.length === 0; i++) await new Promise((r) => setTimeout(r, 20));
    expect(seen.length).toBe(1);
    expect(await seen[0].evaluate("!!window.opener")).toBe(false);
  });
});
