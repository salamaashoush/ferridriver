/**
 * @ferridriver/test — Playwright-compatible test framework powered by Rust.
 *
 * E2E:
 *   import { test, expect } from '@ferridriver/test';
 *
 *   test('basic', async ({ page }) => {
 *     await page.goto('https://example.com');
 *     await expect(page).toHaveTitle('Example Domain');
 *   });
 *
 * Component testing (with --ct flag):
 *   import { test, expect } from '@ferridriver/test';
 *   import Counter from './Counter';
 *
 *   test('counter', async ({ mount, page }) => {
 *     await mount(Counter, { props: { initial: 0 } });
 *     await page.locator('#inc').click();
 *     await expect(page.locator('#count')).toHaveText('1');
 *   });
 */

export { test, describe } from './test.js';
export type { TestFixtures, MountFunction } from './test.js';
export { expect } from './expect.js';
export { defineConfig } from './config.js';
export type { TestRunnerConfig } from 'ferridriver';

// BDD exports
export { Given, When, Then, Step, Before, After, BeforeAll, AfterAll, configureBdd, runFeatures } from './bdd.js';
export type { StepCallback, HookCallback, HookOptions } from './bdd.js';
