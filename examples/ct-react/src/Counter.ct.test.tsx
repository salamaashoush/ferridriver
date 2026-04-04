/**
 * Proper component test for React Counter.
 *
 * This file imports the component directly — the import transform
 * rewrites it to an importRef, the Vite plugin bundles it into
 * the component registry, and mount() resolves it in the browser.
 *
 * Run:
 *   cd examples/ct-react
 *   FERRIDRIVER_CT_URL=http://localhost:3199 bun test src/Counter.ct.test.tsx
 *
 * (The CT runner builds + serves the bundle at the URL above)
 */

import { test, expect } from "@ferridriver/ct-react/test";
import Counter from "./Counter.tsx";

test.describe("Counter component", () => {
  test("mounts and renders initial count", async ({ mount }) => {
    const component = await mount(Counter, { props: { initial: 0 } });
    // After mount, #root contains the rendered Counter.
    // TODO: once full pipeline works, this will use the component locator.
  });

  test("increments on + click", async ({ mount, page }) => {
    await mount(Counter, { props: { initial: 0 } });
    await page.locator("#inc").click();
    const count = await page.locator("#count").textContent();
    expect(count).toBe("1");
  });

  test("decrements on - click", async ({ mount, page }) => {
    await mount(Counter, { props: { initial: 5 } });
    await page.locator("#dec").click();
    const count = await page.locator("#count").textContent();
    expect(count).toBe("4");
  });
});
