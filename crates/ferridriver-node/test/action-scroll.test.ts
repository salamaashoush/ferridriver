/**
 * The `scroll: 'auto' | 'none'` action option: 'none' leaves the page
 * where it is and reports the element outside the viewport.
 */
import { describe, it, expect, beforeAll, afterAll, beforeEach, afterEach } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const backend = process.env.FERRIDRIVER_BACKEND ?? "cdp-pipe";

describe(`[${backend}] action scroll option`, () => {
  let browser: Browser;
  let page: Page;

  beforeAll(async () => {
    browser = await launchForBackend(backend);
  });

  // A page each: the scroll offset an earlier test left survives setContent.
  beforeEach(async () => {
    page = await browser.newPage();
  });

  afterEach(async () => {
    await page.close();
  });

  afterAll(async () => {
    await browser.close();
  });

  it("does not scroll to click when scroll is none", async () => {
    await page.setContent(`<div style="height: 2000px;"></div><button onclick="window._clicked=true">click me</button>`);
    const error = (await page
      .locator("button")
      .click({ scroll: "none", timeout: 1000 })
      .catch((e: Error) => e)) as Error;
    expect(error.message).toContain("element is outside of the viewport");
    expect(await page.evaluate("window._clicked")).toBeFalsy();
    expect(await page.evaluate("window.scrollY")).toBe(0);
    await page.locator("button").click({ scroll: "auto" });
    expect(await page.evaluate("window._clicked")).toBe(true);
  });

  it("does not scroll to hover or check when scroll is none", async () => {
    await page.setContent(`<div style="height: 2000px;"></div><input type="checkbox">`);
    const hover = (await page.hover("input", { scroll: "none", timeout: 1000 }).catch((e: Error) => e)) as Error;
    expect(hover.message).toContain("element is outside of the viewport");
    const check = (await page.locator("input").check({ scroll: "none", timeout: 1000 }).catch((e: Error) => e)) as Error;
    expect(check.message).toContain("element is outside of the viewport");
    expect(await page.evaluate("window.scrollY")).toBe(0);
    expect(await page.locator("input").isChecked()).toBe(false);
  });

  it("rejects an unknown scroll mode", async () => {
    await page.setContent("<button>b</button>");
    const error = await page
      .locator("button")
      .click({ scroll: "sideways" as "none" })
      .catch((e: Error) => e);
    expect(String(error)).toContain("sideways");
  });
});
