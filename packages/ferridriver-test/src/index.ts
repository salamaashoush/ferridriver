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
export type { MountFunction } from './test.js';
export { expect } from './expect.js';
export { defineConfig } from './config.js';
export type { FerridriverTestConfig, UseOptions, ProjectConfig, WebServerConfig, ExpectConfig } from './config.js';
export type { TestRunnerConfig, TestFixtures, TestInfo } from '@ferridriver/node';

// BDD exports — matches @cucumber/cucumber API surface
export {
  // Step definitions
  Given, When, Then, Step, defineStep,
  // Hooks
  Before, After, BeforeAll, AfterAll, BeforeStep, AfterStep,
  // Configuration
  defineParameterType, setDefaultTimeout, setWorldConstructor,
  // Utilities
  Pending, Status, DataTable, version,
  // Runner
  configureBdd, runFeatures,
} from './bdd.js';
export type { StepContext, StepCallback, HookCallback, HookOptions, StepOptions, ParameterTypeOptions } from './bdd.js';
