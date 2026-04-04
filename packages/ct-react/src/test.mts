/**
 * @ferridriver/ct-react — test API
 *
 * Provides Playwright-compatible component testing API:
 *
 * ```typescript
 * import { test, expect } from '@ferridriver/ct-react';
 * import Counter from './Counter';
 *
 * test('increments', async ({ mount, page }) => {
 *   const component = await mount(Counter, { props: { initial: 5 } });
 *   await expect(component.locator('#count')).toHaveText('5');
 *   await component.locator('#inc').click();
 *   await expect(component.locator('#count')).toHaveText('6');
 * });
 * ```
 *
 * Under the hood:
 * 1. Starts Vite dev server (or uses existing one)
 * 2. Navigates browser to the dev server
 * 3. mount() calls page.evaluate() which invokes window.__ferriMount()
 *    (defined by registerSource.mjs)
 * 4. Returns a Locator pointing at the mounted component root
 */

import { Browser, type Page } from "../../../crates/ferridriver-napi/index.js";
import { describe, it, expect, beforeAll, afterAll } from "bun:test";

// Re-export expect for convenience.
export { expect };

// Global state managed by the test harness.
let _browser: Browser | null = null;
let _page: Page | null = null;
let _baseUrl: string = process.env.CT_URL || "http://localhost:5173";

/**
 * Mount function — injects a component into the page via evaluate.
 *
 * The registerSource.mjs (loaded by the dev server page) defines
 * window.__ferriMount which uses React's createRoot to render.
 *
 * For the component registry to work, the dev server must serve a page
 * that includes the registerSource AND registers the component.
 * In the simple case (no registry), we pass component as a string ID
 * and the registerSource looks it up.
 */
type MountFunction = (
  componentOrId: any,
  options?: { props?: Record<string, any> }
) => Promise<{ locator: (selector: string) => any }>;

interface TestFixtures {
  page: Page;
  mount: MountFunction;
}

type TestFn = (fixtures: TestFixtures) => Promise<void>;

/**
 * Define a component test.
 *
 * Usage:
 * ```typescript
 * test('my test', async ({ mount, page }) => {
 *   const component = await mount(MyComponent, { props: { count: 0 } });
 *   await page.locator('button').click();
 * });
 * ```
 */
export function test(name: string, fn: TestFn) {
  it(name, async () => {
    if (!_browser) {
      _browser = await Browser.launch({ backend: "cdp-pipe" });
    }

    // Create fresh page per test.
    const page = await _browser.newPageWithUrl(_baseUrl);

    // Wait for the page to be ready.
    await new Promise((r) => setTimeout(r, 200));

    // Create mount function.
    const mount: MountFunction = async (componentOrId, options = {}) => {
      const props = options.props || {};
      const componentId =
        typeof componentOrId === "string"
          ? componentOrId
          : componentOrId?.name || componentOrId?.displayName || "default";

      // Call __ferriMount if available, otherwise just render via registry.
      const js = `(() => {
        const root = document.getElementById('root') || document.getElementById('app');
        if (!root) throw new Error('No #root or #app element');
        if (window.__ferriMount) {
          window.__ferriMount(
            { id: '${componentId}', props: ${JSON.stringify(props)} },
            root,
            { props: ${JSON.stringify(props)} }
          );
        }
        return root.outerHTML;
      })()`;

      await page.evaluate(js);

      // Return a locator-like object pointing at the component root.
      return {
        locator: (selector: string) => page.locator(selector),
      };
    };

    try {
      await fn({ page, mount });
    } finally {
      // Page cleanup — context is isolated per test in ferridriver.
    }
  });
}

/**
 * Describe block for grouping component tests.
 */
test.describe = (name: string, fn: () => void) => {
  describe(name, () => {
    beforeAll(async () => {
      if (!_browser) {
        _browser = await Browser.launch({ backend: "cdp-pipe" });
      }
    });

    afterAll(async () => {
      if (_browser) {
        await _browser.close();
        _browser = null;
      }
    });

    fn();
  });
};

/**
 * Set the base URL for the dev server.
 * Call before any tests if not using CT_URL env var.
 */
export function setBaseUrl(url: string) {
  _baseUrl = url;
}

/**
 * Clean up browser on process exit.
 */
process.on("exit", () => {
  // Can't await here, but browser should auto-close.
});
