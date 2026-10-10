/**
 * frameLocator() without a selector, locator.visible()/within(), and
 * page.getByRef(): Playwright 1.6x locator surface.
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const backend = process.env.FERRIDRIVER_BACKEND ?? "cdp-pipe";

describe(`[${backend}] locator frames and filters`, () => {
  let browser: Browser;
  let page: Page;

  beforeAll(async () => {
    browser = await launchForBackend(backend);
    page = await browser.newPage();
  });

  afterAll(async () => {
    await browser.close();
  });

  it("frameLocator() finds the frame the locator matches in", async () => {
    await page.setContent(`<iframe srcdoc="<button>inside</button>"></iframe>`);
    await page.frameLocator().locator("button").waitFor();
    expect(await page.frameLocator().locator("button").textContent()).toBe("inside");
    expect(String(page.frameLocator().locator("button"))).toBe(`frameLocator().locator('button')`);
    expect(() => page.frameLocator().first()).toThrow("Selecting the nth frame is not allowed on frameLocator().");
  });

  it("visible() and within() narrow the match", async () => {
    await page.setContent(`<section><p>inside</p></section><p>outside</p><p style="display:none">hidden</p>`);
    expect(await page.locator("p").visible().count()).toBe(2);
    expect(await page.locator("p").within(page.locator("section")).textContent()).toBe("inside");
  });

  it("getByRef() finds the element an AI snapshot labelled", async () => {
    await page.setContent(`<button id="submit">Submit</button>`);
    const snapshot = await page.locator("body").ariaSnapshot({ mode: "ai" });
    const ref = /button "Submit" \[ref=(e\d+)\]/.exec(snapshot)?.[1];
    expect(ref).toBeTruthy();
    expect(await page.getByRef(ref!).getAttribute("id")).toBe("submit");
  });
});
