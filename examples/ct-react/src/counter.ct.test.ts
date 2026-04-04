/**
 * Component test for Counter using the @ferridriver/ct-react API.
 *
 * This is the Playwright-style API:
 *   test('name', async ({ mount, page }) => { ... })
 *
 * Prerequisites:
 *   cd examples/ct-react && bun install && bun run dev
 *   # In another terminal:
 *   CT_URL=http://localhost:5173 bun test src/counter.ct.test.ts
 */

import { test, expect } from "../../../packages/ct-react/src/test.mts";

test.describe("Counter component", () => {
  test("renders with initial count", async ({ page }) => {
    const count = await page.locator("#count").textContent();
    expect(count).toBe("0");
  });

  test("increments on click", async ({ page }) => {
    await page.locator("#inc").click();
    // After navigation + click, count should be 1.
    const count = await page.locator("#count").textContent();
    expect(count).toBe("1");
  });

  test("decrements on click", async ({ page }) => {
    await page.locator("#dec").click();
    const count = await page.locator("#count").textContent();
    expect(count).toBe("-1");
  });

  test("handles multiple increments", async ({ page }) => {
    for (let i = 0; i < 5; i++) {
      await page.locator("#inc").click();
    }
    const count = await page.locator("#count").textContent();
    expect(count).toBe("5");
  });
});
