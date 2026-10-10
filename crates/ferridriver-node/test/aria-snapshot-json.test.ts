/**
 * page/locator ariaSnapshotJSON() and page.ariaSnapshot() with
 * Playwright's options.
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const backend = process.env.FERRIDRIVER_BACKEND ?? "cdp-pipe";

describe(`[${backend}] ariaSnapshotJSON`, () => {
  let browser: Browser;
  let page: Page;

  beforeAll(async () => {
    browser = await launchForBackend(backend);
    page = await browser.newPage();
  });

  afterAll(async () => {
    await browser.close();
  });

  it("snapshots the page and a locator as JSON", async () => {
    await page.setContent(`<h1>title</h1><ul aria-label="my list"><li>one</li><li>two</li></ul>`);
    expect(await page.ariaSnapshotJSON()).toEqual([
      { role: "heading", name: "title", level: 1 },
      { role: "list", name: "my list", children: [{ role: "listitem", text: "one" }, { role: "listitem", text: "two" }] },
    ]);
    expect(await page.locator("ul").ariaSnapshotJSON({ depth: 1 })).toEqual([
      { role: "list", name: "my list", children: [{ role: "listitem", text: "one" }, { role: "listitem", text: "two" }] },
    ]);
  });

  it("renders page.ariaSnapshot with iframes in ai mode", async () => {
    await page.setContent(`<h1>Hello</h1><iframe srcdoc="<button>In frame</button>"></iframe>`);
    await page.frameLocator("iframe").locator("button").waitFor();
    expect(await page.ariaSnapshot()).toBe('- heading "Hello" [level=1]\n- iframe');
    expect(await page.ariaSnapshot({ mode: "ai" })).toMatch(/- iframe \[ref=e\d+\]:\n\s+- button "In frame" \[ref=f\d+e\d+\]/);
  });
});
