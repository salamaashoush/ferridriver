/**
 * @ferridriver/ct-react — test API
 *
 * Full Playwright-style component testing for React:
 *
 * ```typescript
 * import { test, expect } from '@ferridriver/ct-react';
 * import Counter from './Counter';
 *
 * test('increments', async ({ mount, page }) => {
 *   const component = await mount(<Counter initial={5} />);
 *   await expect(component.locator('#count')).toHaveText('5');
 *   await component.locator('#inc').click();
 *   await expect(component.locator('#count')).toHaveText('6');
 * });
 * ```
 *
 * Pipeline:
 * 1. Before tests: ct-core scans test files, builds Vite bundle with registry
 * 2. Starts preview server at FERRIDRIVER_CT_URL
 * 3. Each test: navigates to preview, mount() → page.evaluate() → __ferriMount()
 * 4. After tests: shuts down server + browser
 */

import { Browser, type Page } from "../../../crates/ferridriver-napi/index.js";
import { mount as ctMount } from "../../ct-core/src/mount.mjs";
import { describe, it, expect, beforeAll, afterAll } from "bun:test";

export { expect };

let _browser: Browser | null = null;
let _baseUrl: string = "";
let _boundCallbacks: Function[] = [];

interface MountResult {
  locator: (selector: string) => ReturnType<Page["locator"]>;
}

type MountFunction = (
  component: any,
  options?: { props?: Record<string, any>; hooksConfig?: Record<string, any> }
) => Promise<MountResult>;

interface TestFixtures {
  page: Page;
  mount: MountFunction;
}

type TestFn = (fixtures: TestFixtures) => Promise<void>;

/**
 * The base URL comes from:
 * 1. FERRIDRIVER_CT_URL env var (set by ct-core runner after Vite build+preview)
 * 2. CT_URL env var (manual dev server)
 * 3. Default http://localhost:3100
 */
function getBaseUrl(): string {
  return (
    process.env.FERRIDRIVER_CT_URL ||
    process.env.CT_URL ||
    "http://localhost:3100"
  );
}

/**
 * Define a component test.
 */
export function test(name: string, fn: TestFn) {
  it(name, async () => {
    if (!_browser) {
      _browser = await Browser.launch({ backend: "cdp-pipe" });
    }
    _baseUrl = getBaseUrl();
    _boundCallbacks = [];

    // Fresh page per test, navigated to the CT preview server.
    const page = await _browser.newPageWithUrl(_baseUrl);

    // Expose the callback bridge for function refs (event handlers etc).
    await page.evaluate(`(() => {
      window.__ferriDispatchFunction = window.__ferriDispatchFunction || function() {};
    })()`);

    // Create the mount function that uses ct-core's mount().
    const mount: MountFunction = async (component, options = {}) => {
      await ctMount(page, component, options, _boundCallbacks);
      const rootLocator = page.locator("#root");
      return {
        locator: (selector: string) => page.locator(`#root ${selector}`),
      };
    };

    await fn({ page, mount });
  });
}

/**
 * Describe block.
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
 * Configure the CT runner programmatically.
 */
export function configure(opts: { baseUrl?: string }) {
  if (opts.baseUrl) _baseUrl = opts.baseUrl;
}
