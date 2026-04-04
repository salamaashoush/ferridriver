/**
 * Component test for the React Counter.
 *
 * Uses ferridriver's browser automation to interact with the component
 * running in a real Vite dev server.
 *
 * Run:
 *   cd examples/ct-react
 *   bun install
 *   bun test
 */

import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { Browser, type Page } from "../../crates/ferridriver-napi/index.js";

describe("Counter component", () => {
  let browser: Browser;
  let page: Page;

  // The Vite dev server must be running: `bun run dev`
  // Or use the ferridriver DevServer manager to start it automatically.
  const BASE_URL = process.env.VITE_URL || "http://localhost:5173";

  beforeAll(async () => {
    browser = await Browser.launch({ backend: "cdp-pipe" });
    page = await browser.newPageWithUrl(BASE_URL);
  });

  afterAll(async () => {
    await browser.close();
  });

  it("renders with initial count of 0", async () => {
    const count = await page.locator("#count").textContent();
    expect(count).toBe("0");
  });

  it("increments on + click", async () => {
    await page.locator("#inc").click();
    const count = await page.locator("#count").textContent();
    expect(count).toBe("1");
  });

  it("decrements on - click", async () => {
    // Click - twice (from 1 → -1... no, we need to track state).
    // Navigate fresh to reset.
    await page.goto(BASE_URL);
    await page.locator("#dec").click();
    const count = await page.locator("#count").textContent();
    expect(count).toBe("-1");
  });

  it("handles multiple clicks", async () => {
    await page.goto(BASE_URL);
    for (let i = 0; i < 5; i++) {
      await page.locator("#inc").click();
    }
    const count = await page.locator("#count").textContent();
    expect(count).toBe("5");
  });
});
