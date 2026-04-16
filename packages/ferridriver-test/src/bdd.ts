/**
 * @ferridriver/test BDD API — Cucumber/Gherkin step definitions in TypeScript.
 *
 * Step callbacks receive a StepContext with typed access to page, params, table, docstring.
 *
 * @example
 *   Given('I navigate to {string}', async ({ page, params: [url] }) => {
 *     await page.goto(url);
 *   });
 *
 *   Given('I have {int} {string}', async ({ page, params: [count, item] }) => {
 *     // count and item are strings — parse as needed
 *   });
 *
 *   // Also works with RegExp:
 *   Given(/^I have (\d+) items?$/, async ({ page, params: [count] }) => {
 *     await page.goto(`/items?n=${count}`);
 *   });
 */

import { TestRunner, type Page, type RunSummary, type TestFixtures } from '@ferridriver/node';

// ── Cucumber Expression Type Inference ────────────────────────────────────

/** Map cucumber param type name to TypeScript type. */
type CucumberParamType<T extends string> =
  T extends 'string' ? string :
  T extends 'int' ? number :
  T extends 'float' ? number :
  T extends 'word' ? string :
  T extends '' ? string :
  string;

/** Extract parameter types from a cucumber expression string as a tuple. */
type ExtractParams<T extends string> =
  T extends `${string}{${infer P}}${infer Rest}`
    ? [CucumberParamType<P>, ...ExtractParams<Rest>]
    : [];

// ── StepContext — TestFixtures with typed BDD params ─────────────────────

/**
 * BDD step/hook context — extends TestFixtures with typed BDD params.
 *
 * Steps get the full E2E fixture set (page, browser, context, request, testInfo)
 * plus typed params from cucumber expressions:
 *   `{string}` → `string`, `{int}` → `number`, `{float}` → `number`
 *
 * Hooks get the same fixtures with args/dataTable/docString as null.
 */
export interface StepContext<Params extends unknown[] = unknown[]> extends TestFixtures {
  /** Extracted parameters from the expression (typed: int→number, string→string). */
  readonly args: Params;
  /** Alias for args. */
  readonly params: Params;
  /** Inline data table, if the step has one. */
  readonly dataTable: string[][] | null;
  /** Doc string content, if the step has one. */
  readonly docString: string | null;
}

/** Step callback with typed context. */
type TypedStepCallback<P extends unknown[] = unknown[]> = (ctx: StepContext<P>) => Promise<void>;

/** Untyped step callback (for RegExp and hooks). */
type StepCallback = (ctx: StepContext) => Promise<void>;

/** Hook callback — receives TestFixtures (BDD fields are null). */
type HookCallback = (fixtures: TestFixtures) => Promise<void>;

interface HookOptions {
  tags?: string;
  name?: string;
  timeout?: number;
}

interface StepOptions {
  timeout?: number;
  wrapperOptions?: any;
}

interface ParameterTypeOptions {
  name: string;
  regexp: string | RegExp | readonly (string | RegExp)[];
  transformer?: (...args: string[]) => any;
  useForSnippets?: boolean;
  preferForRegexpMatch?: boolean;
}

// ── Status ────────────────────────────────────────────────────────────────

export enum Status {
  PASSED = 'PASSED',
  FAILED = 'FAILED',
  PENDING = 'PENDING',
  SKIPPED = 'SKIPPED',
  UNDEFINED = 'UNDEFINED',
  AMBIGUOUS = 'AMBIGUOUS',
  UNKNOWN = 'UNKNOWN',
}

// ── DataTable ─────────────────────────────────────────────────────────────

export class DataTable {
  private readonly _raw: string[][];

  constructor(raw: string[][]) {
    this._raw = raw;
  }

  raw(): string[][] { return this._raw; }
  rows(): string[][] { return this._raw.slice(1); }

  hashes(): Record<string, string>[] {
    if (this._raw.length < 2) return [];
    const headers = this._raw[0];
    return this._raw.slice(1).map(row => {
      const obj: Record<string, string> = {};
      headers.forEach((h, i) => { obj[h] = row[i] ?? ''; });
      return obj;
    });
  }

  rowsHash(): Record<string, string> {
    const obj: Record<string, string> = {};
    for (const row of this._raw) {
      if (row.length >= 2) obj[row[0]] = row[1];
    }
    return obj;
  }

  transpose(): DataTable {
    if (this._raw.length === 0) return new DataTable([]);
    const maxCols = Math.max(...this._raw.map(r => r.length));
    const transposed: string[][] = Array.from({ length: maxCols }, () => []);
    for (const row of this._raw) {
      for (let i = 0; i < maxCols; i++) {
        transposed[i].push(row[i] ?? '');
      }
    }
    return new DataTable(transposed);
  }
}

// ── Version ───────────────────────────────────────────────────────────────

export const version = '0.2.0';

// ── Runner ────────────────────────────────────────────────────────────────

// Runner is shared via globalThis.__ferridriver.runner — set once by CLI via test.ts._setRunner().
// No separate _setRunner needed for BDD.
const _state = (globalThis as any).__ferridriver;

function getRunner(): InstanceType<typeof TestRunner> {
  const runner = _state?.runner;
  if (!runner) {
    throw new Error('Runner not initialized — test.ts._setRunner() must be called before step registration');
  }
  return runner;
}

export function configureBdd(config: Record<string, any>): void {
  // Config is applied via TestRunner.create() — this is a no-op now.
}

export function setDefaultTimeout(ms: number): void {
  // Timeout is set via TestRunnerConfig — this is a no-op now.
}

// ── Step Registration ─────────────────────────────────────────────────────

function registerStep(
  kind: 'given' | 'when' | 'then' | 'step',
  pattern: string | RegExp,
  optionsOrCallback: StepOptions | Function,
  callback?: Function,
): void {
  const [opts, cb] = typeof optionsOrCallback === 'function'
    ? [{} as StepOptions, optionsOrCallback]
    : [optionsOrCallback, callback!];

  if (pattern instanceof RegExp) {
    getRunner().registerStep(kind, pattern.source, cb as any, true, opts.timeout);
  } else {
    getRunner().registerStep(kind, pattern, cb as any, false, opts.timeout);
  }
}

/**
 * Register a Given step definition.
 *
 * Cucumber expressions get typed params: `{int}` → `number`, `{string}` → `string`.
 * RegExp captures are `unknown[]`.
 *
 * @example
 * Given('I have {int} {string}', async ({ page, params: [count, item] }) => {
 *   // count: number, item: string — inferred from the expression!
 * });
 *
 * Given(/^I have (\d+) items$/, async ({ page, params: [count] }) => {
 *   // count: unknown (regex captures aren't typed)
 * });
 */
export function Given<E extends string>(pattern: E, callback: TypedStepCallback<ExtractParams<E>>): void;
export function Given<E extends string>(pattern: E, options: StepOptions, callback: TypedStepCallback<ExtractParams<E>>): void;
export function Given(pattern: RegExp, callback: StepCallback): void;
export function Given(pattern: RegExp, options: StepOptions, callback: StepCallback): void;
export function Given(pattern: string | RegExp, optionsOrCallback: any, callback?: any): void {
  registerStep('given', pattern, optionsOrCallback, callback);
}

export function When<E extends string>(pattern: E, callback: TypedStepCallback<ExtractParams<E>>): void;
export function When<E extends string>(pattern: E, options: StepOptions, callback: TypedStepCallback<ExtractParams<E>>): void;
export function When(pattern: RegExp, callback: StepCallback): void;
export function When(pattern: RegExp, options: StepOptions, callback: StepCallback): void;
export function When(pattern: string | RegExp, optionsOrCallback: any, callback?: any): void {
  registerStep('when', pattern, optionsOrCallback, callback);
}

export function Then<E extends string>(pattern: E, callback: TypedStepCallback<ExtractParams<E>>): void;
export function Then<E extends string>(pattern: E, options: StepOptions, callback: TypedStepCallback<ExtractParams<E>>): void;
export function Then(pattern: RegExp, callback: StepCallback): void;
export function Then(pattern: RegExp, options: StepOptions, callback: StepCallback): void;
export function Then(pattern: string | RegExp, optionsOrCallback: any, callback?: any): void {
  registerStep('then', pattern, optionsOrCallback, callback);
}

export function Step<E extends string>(pattern: E, callback: TypedStepCallback<ExtractParams<E>>): void;
export function Step<E extends string>(pattern: E, options: StepOptions, callback: TypedStepCallback<ExtractParams<E>>): void;
export function Step(pattern: RegExp, callback: StepCallback): void;
export function Step(pattern: RegExp, options: StepOptions, callback: StepCallback): void;
export function Step(pattern: string | RegExp, optionsOrCallback: any, callback?: any): void {
  registerStep('step', pattern, optionsOrCallback, callback);
}

/** Keyword-agnostic step definition (Cucumber compat alias). */
export const defineStep = Step;

// ── Parameter Types ───────────────────────────────────────────────────────

export function defineParameterType(options: ParameterTypeOptions): void;
export function defineParameterType(name: string, regex: string): void;
export function defineParameterType(
  nameOrOptions: string | ParameterTypeOptions,
  regex?: string,
): void {
  if (typeof nameOrOptions === 'string') {
    getRunner().defineParameterType(nameOrOptions, regex!);
  } else {
    const r = nameOrOptions.regexp;
    const regexStr = Array.isArray(r)
      ? (r as readonly (string | RegExp)[]).map(x => typeof x === 'string' ? x : x.source).join('|')
      : typeof r === 'string' ? r : (r as RegExp).source;
    getRunner().defineParameterType(nameOrOptions.name, regexStr);
  }
}

// ── Hooks ─────────────────────────────────────────────────────────────────

function registerHook(
  point: 'before' | 'after',
  scope: 'scenario' | 'step' | 'all',
  optionsOrTagsOrCallback: HookOptions | string | HookCallback,
  callback?: HookCallback,
): void {
  if (typeof optionsOrTagsOrCallback === 'function') {
    getRunner().registerBddHook(point, scope, optionsOrTagsOrCallback as any);
  } else if (typeof optionsOrTagsOrCallback === 'string') {
    getRunner().registerBddHook(point, scope, callback as any, optionsOrTagsOrCallback);
  } else {
    getRunner().registerBddHook(point, scope, callback as any, optionsOrTagsOrCallback.tags, optionsOrTagsOrCallback.name, optionsOrTagsOrCallback.timeout);
  }
}

/**
 * Before hook — runs before each scenario.
 *
 * @example
 * Before(async ({ page }) => { await page.goto('/'); });
 * Before('@auth', async ({ page }) => { ... });
 * Before({ tags: '@auth', name: 'login', timeout: 10000 }, async ({ page }) => { ... });
 */
export function Before(callback: HookCallback): void;
export function Before(tags: string, callback: HookCallback): void;
export function Before(options: HookOptions, callback: HookCallback): void;
export function Before(a: HookOptions | string | HookCallback, b?: HookCallback): void {
  registerHook('before', 'scenario', a, b);
}

export function After(callback: HookCallback): void;
export function After(tags: string, callback: HookCallback): void;
export function After(options: HookOptions, callback: HookCallback): void;
export function After(a: HookOptions | string | HookCallback, b?: HookCallback): void {
  registerHook('after', 'scenario', a, b);
}

export function BeforeStep(callback: HookCallback): void;
export function BeforeStep(tags: string, callback: HookCallback): void;
export function BeforeStep(options: HookOptions, callback: HookCallback): void;
export function BeforeStep(a: HookOptions | string | HookCallback, b?: HookCallback): void {
  registerHook('before', 'step', a, b);
}

export function AfterStep(callback: HookCallback): void;
export function AfterStep(tags: string, callback: HookCallback): void;
export function AfterStep(options: HookOptions, callback: HookCallback): void;
export function AfterStep(a: HookOptions | string | HookCallback, b?: HookCallback): void {
  registerHook('after', 'step', a, b);
}

export function BeforeAll(callback: () => Promise<void>): void {
  getRunner().registerBddHook('before', 'all', callback as any);
}

export function AfterAll(callback: () => Promise<void>): void {
  getRunner().registerBddHook('after', 'all', callback as any);
}

// ── World ─────────────────────────────────────────────────────────────────

/**
 * Cucumber compat shim. In ferridriver, the world is Page-based and managed
 * by the Rust engine. Steps receive a StepContext with `page` instead.
 */
export function setWorldConstructor(_fn: new (options: any) => any): void {}

// ── Pending ───────────────────────────────────────────────────────────────

export function Pending(message = 'step not yet implemented'): never {
  throw Object.assign(new Error(message), { pending: true });
}

// ── Run ───────────────────────────────────────────────────────────────────

export async function runFeatures(features?: string | string[]): Promise<RunSummary> {
  const featureList = features ? (Array.isArray(features) ? features : [features]) : undefined;
  return getRunner().run(featureList);
}

export type { RunSummary, Page, StepCallback, HookCallback, HookOptions, StepOptions, ParameterTypeOptions };
