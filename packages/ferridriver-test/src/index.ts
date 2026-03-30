/**
 * @ferridriver/test — Playwright-compatible E2E test framework powered by Rust.
 *
 * Usage:
 *   import { test, expect } from '@ferridriver/test';
 *
 *   test('basic test', async ({ page }) => {
 *     await page.goto('https://example.com');
 *     await expect(page).toHaveTitle('Example Domain');
 *   });
 */

export { test, describe } from './test.js';
export { expect } from './expect.js';
export { defineConfig } from './config.js';
export type { TestRunnerConfig } from 'ferridriver';
