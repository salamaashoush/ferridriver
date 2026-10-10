/**
 * One JS object per page and per frame, as Playwright's client gives.
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const backend = process.env.FERRIDRIVER_BACKEND ?? "cdp-pipe";

describe(`[${backend}] object identity`, () => {
  let browser: Browser;
  let page: Page;

  beforeAll(async () => {
    browser = await launchForBackend(backend);
    page = await browser.newPage();
  });

  afterAll(async () => {
    await browser.close();
  });

  it("pages and frames keep their identity", async () => {
    await page.setContent('<iframe srcdoc="<p>child</p>"></iframe>');
    await page.frameLocator("iframe").locator("p").waitFor();
    const main = page.mainFrame();
    const [first, child] = page.frames();
    expect(main === page.mainFrame()).toBe(true);
    expect(first === main).toBe(true);
    expect(child.parentFrame() === main).toBe(true);
    expect(main.page() === page).toBe(true);
    expect(page.locator("iframe").page() === page).toBe(true);
    const iframe = await page.locator("iframe").elementHandle();
    expect((await iframe.contentFrame()) === child).toBe(true);
  });
});
