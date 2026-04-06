/**
 * @ferridriver/test BDD API — Cucumber/Gherkin step definitions in TypeScript.
 *
 * Steps are registered via Given/When/Then and executed by the Rust engine
 * with full parallel worker pool, retries, and reporters.
 *
 * Usage:
 *   import { Given, When, Then, runFeatures } from '@ferridriver/test/bdd';
 *
 *   Given('I navigate to {string}', async (page, url) => {
 *     await page.goto(url);
 *   });
 *
 *   When('I click {string}', async (page, selector) => {
 *     await page.locator(selector).click();
 *   });
 *
 *   Then('{string} should be visible', async (page, selector) => {
 *     await expect(page.locator(selector)).toBeVisible();
 *   });
 *
 *   await runFeatures('features/**\/*.feature');
 */

import { BddRunner as NativeBddRunner, type BddRunnerConfig, type BddRunSummary, type Page } from 'ferridriver';

type StepCallback = (page: Page, ...args: any[]) => Promise<void>;
type HookCallback = (page: Page) => Promise<void>;

interface HookOptions {
  tags?: string;
}

// Global runner instance (lazily created or set by CLI).
let _runner: InstanceType<typeof NativeBddRunner> | null = null;
let _config: BddRunnerConfig = {};

function getRunner(): InstanceType<typeof NativeBddRunner> {
  if (!_runner) {
    _runner = NativeBddRunner.create(_config);
  }
  return _runner;
}

/**
 * Configure the BDD runner before registering steps.
 * Must be called before any Given/When/Then if custom config is needed.
 */
export function configureBdd(config: BddRunnerConfig): void {
  _config = config;
  _runner = null; // Reset so next getRunner() picks up new config.
}

/**
 * Set an externally-created runner instance (used by CLI).
 * Step registrations will go to this runner.
 */
export function _setRunner(runner: InstanceType<typeof NativeBddRunner>): void {
  _runner = runner;
}

/**
 * Get the current runner (used by CLI to call run()).
 */
export function _getRunner(): InstanceType<typeof NativeBddRunner> {
  return getRunner();
}

/**
 * Register a Given step definition.
 *
 * @param pattern Cucumber expression (e.g., "I navigate to {string}")
 * @param callback Async function receiving (page, ...captured params)
 */
export function Given(pattern: string, callback: StepCallback): void {
  getRunner().registerStep('given', pattern, callback as any);
}

/**
 * Register a When step definition.
 */
export function When(pattern: string, callback: StepCallback): void {
  getRunner().registerStep('when', pattern, callback as any);
}

/**
 * Register a Then step definition.
 */
export function Then(pattern: string, callback: StepCallback): void {
  getRunner().registerStep('then', pattern, callback as any);
}

/**
 * Register a keyword-agnostic step definition (matches Given/When/Then).
 */
export function Step(pattern: string, callback: StepCallback): void {
  getRunner().registerStep('step', pattern, callback as any);
}

/**
 * Register a Before hook.
 *
 * @param optionsOrCallback Hook options or callback
 * @param callback Callback if first arg is options
 */
export function Before(optionsOrCallback: HookOptions | HookCallback, callback?: HookCallback): void {
  if (typeof optionsOrCallback === 'function') {
    getRunner().registerHook('before', 'scenario', optionsOrCallback as any);
  } else {
    getRunner().registerHook('before', 'scenario', callback as any, optionsOrCallback.tags);
  }
}

/**
 * Register an After hook.
 */
export function After(optionsOrCallback: HookOptions | HookCallback, callback?: HookCallback): void {
  if (typeof optionsOrCallback === 'function') {
    getRunner().registerHook('after', 'scenario', optionsOrCallback as any);
  } else {
    getRunner().registerHook('after', 'scenario', callback as any, optionsOrCallback.tags);
  }
}

/**
 * Register a BeforeAll hook (runs once per worker, no page).
 */
export function BeforeAll(callback: () => Promise<void>): void {
  getRunner().registerHook('before', 'all', callback as any);
}

/**
 * Register an AfterAll hook.
 */
export function AfterAll(callback: () => Promise<void>): void {
  getRunner().registerHook('after', 'all', callback as any);
}

/**
 * Run BDD features.
 *
 * Discovers .feature files, matches steps against registered definitions
 * (both TypeScript and built-in Rust steps), executes via the core TestRunner
 * with parallel workers, retries, and reporters.
 *
 * @returns Run summary with pass/fail/skip counts
 */
export async function runFeatures(features?: string | string[]): Promise<BddRunSummary> {
  const runner = getRunner();

  // If features passed, reconfigure.
  if (features) {
    const patterns = Array.isArray(features) ? features : [features];
    // Re-create runner with feature patterns if needed.
    // For now, features are set via config.
  }

  return runner.run();
}

export type { BddRunnerConfig, BddRunSummary, Page, StepCallback, HookCallback, HookOptions };
