/**
 * Test declaration API — Playwright-compatible.
 *
 * Rust is the source of truth. This module is a thin registration layer.
 * Runtime modifiers (skip/fail/slow) delegate directly to NAPI TestInfo methods.
 *
 * Usage (matches Playwright):
 *   test('name', async ({ page, browserName, testInfo }) => {
 *     test.skip(browserName === 'firefox', 'not supported');
 *     await page.goto('...');
 *   });
 */

import type { TestMeta, TestRunner, TestFixtures } from '@ferridriver/core';

// ── Types ──

/** Test function signature — receives Playwright-compatible fixtures. */
type TestBody = (fixtures: TestFixtures) => Promise<void>;

/** Parameterized test body. */
type EachTestBody<T> = (fixtures: TestFixtures, data: T) => Promise<void>;

/** Test details — matches Playwright's TestDetails. */
interface TestDetails {
  tag?: string | string[];
  annotation?: { type: string; description?: string } | { type: string; description?: string }[];
  timeout?: number;
  retries?: number;
}

// ── Registry ──

interface RegisteredTest {
  meta: TestMeta;
  body: (fixtures: TestFixtures) => Promise<void>;
}

const registry: RegisteredTest[] = [];
const describeStack: string[] = [];
const suiteIdStack: string[] = [];
let hasOnly = false;
let currentFile = '';

// ── Runner + TestInfo (set by CLI) ──

let _runner: TestRunner | null = null;
let _currentTestInfo: any | null = null;

/** Called by CLI after TestRunner.create(). */
export function _setRunner(runner: TestRunner): void {
  _runner = runner;
}

// ── Mount (CT mode) ──

type MountFunction = (
  component: any,
  options?: { props?: Record<string, any>; hooksConfig?: Record<string, any> },
) => Promise<void>;

export type { MountFunction };

let _ctMountFactory: ((page: any) => MountFunction) | null = null;

export function _setCtMountFactory(factory: (page: any) => MountFunction): void {
  _ctMountFactory = factory;
}

// ── Helpers ──

function parseTemplateTable(strings: TemplateStringsArray, values: any[]): Record<string, any>[] {
  const raw = strings.join('\x00').split('\n').map(l => l.trim()).filter(l => l.length > 0);
  if (raw.length < 2) return [];
  const headers = raw[0].split('|').map(h => h.replace(/\x00/g, '').trim()).filter(Boolean);
  const rows: Record<string, any>[] = [];
  let valIdx = 0;
  for (let i = 1; i < raw.length; i++) {
    const cells = raw[i].split('|').map(c => c.trim()).filter(Boolean);
    const row: Record<string, any> = {};
    for (let j = 0; j < headers.length; j++) {
      const cell = cells[j] ?? '';
      if (cell.includes('\x00')) { row[headers[j]] = values[valIdx++]; }
      else { row[headers[j]] = cell; }
    }
    rows.push(row);
  }
  return rows;
}

function interpolateTemplate(template: string, data: any): string {
  if (typeof data !== 'object' || data === null)
    return template.replace(/\$\{?\w+\}?/g, String(data));
  return template.replace(/\$(\w+)/g, (_, key) => key in data ? String(data[key]) : `$${key}`);
}

function fullTitle(name: string): string {
  return [...describeStack, name].join(' > ');
}

function makeId(file: string, title: string): string {
  return `${file}:${title}`;
}

function currentSuiteId(): string | undefined {
  return suiteIdStack.length > 0 ? suiteIdStack[suiteIdStack.length - 1] : undefined;
}

// ── Annotations builder ──

function buildAnnotations(details?: TestDetails): any[] {
  const annotations: any[] = [];
  if (details?.tag) {
    const tags = Array.isArray(details.tag) ? details.tag : [details.tag];
    for (const t of tags) annotations.push({ tag: t });
  }
  if (details?.annotation) {
    const anns = Array.isArray(details.annotation) ? details.annotation : [details.annotation];
    for (const a of anns) {
      annotations.push({ info: { type_name: a.type, description: a.description ?? '' } });
    }
  }
  return annotations;
}

// ── Body wrapper (sets _currentTestInfo for runtime modifiers) ──

function wrapBody(body: TestBody): (fixtures: TestFixtures) => Promise<void> {
  return async (fixtures: TestFixtures) => {
    _currentTestInfo = (fixtures as any).testInfo;
    try { await body(fixtures); }
    finally { _currentTestInfo = null; }
  };
}

// ── test() ──

function testFn(name: string, body: TestBody): void;
function testFn(name: string, details: TestDetails, body: TestBody): void;
function testFn(name: string, detailsOrBody: TestDetails | TestBody, maybeBody?: TestBody): void {
  const body = typeof detailsOrBody === 'function' ? detailsOrBody : maybeBody!;
  const details = typeof detailsOrBody === 'function' ? undefined : detailsOrBody;
  const title = fullTitle(name);
  registry.push({
    meta: {
      id: makeId(currentFile, title),
      title,
      file: currentFile,
      timeout: details?.timeout,
      retries: details?.retries,
      suite_id: currentSuiteId(),
      annotations: buildAnnotations(details),
      use_options: currentUseOverrides(),
    },
    body: wrapBody(body),
  });
}

// ── Runtime modifiers — delegate to NAPI TestInfo ──

/**
 * test.skip() — Playwright-compatible.
 *
 * Registration: test.skip('name', body)
 * Runtime:      test.skip(browserName === 'firefox', 'not supported')
 *               test.skip()
 */
testFn.skip = (...args: any[]) => {
  if (args.length === 0 || typeof args[0] === 'boolean') {
    // Runtime: delegate to NAPI TestInfo.skip() — Rust handles everything.
    if (!_currentTestInfo) throw new Error('test.skip() can only be called inside a test body');
    _currentTestInfo.skip(args[0] ?? true, args[1]);
    return;
  }
  // Registration: test.skip('name', body) or test.skip('name', details, body)
  const [name, detailsOrBody, maybeBody] = args;
  const body = typeof detailsOrBody === 'function' ? detailsOrBody : maybeBody;
  const details = typeof detailsOrBody === 'function' ? undefined : detailsOrBody;
  const title = fullTitle(name);
  registry.push({
    meta: {
      id: makeId(currentFile, title), title, file: currentFile,
      timeout: details?.timeout, retries: details?.retries, suite_id: currentSuiteId(),
      annotations: [...buildAnnotations(details), { skip: { reason: null, condition: null } }],
    },
    body: wrapBody(body),
  });
};

/**
 * test.fixme() — known bug, same as skip semantically.
 */
testFn.fixme = (...args: any[]) => {
  if (args.length === 0 || typeof args[0] === 'boolean') {
    if (!_currentTestInfo) throw new Error('test.fixme() can only be called inside a test body');
    _currentTestInfo.fixme(args[0] ?? true, args[1]);
    return;
  }
  const [name, detailsOrBody, maybeBody] = args;
  const body = typeof detailsOrBody === 'function' ? detailsOrBody : maybeBody;
  const details = typeof detailsOrBody === 'function' ? undefined : detailsOrBody;
  const title = fullTitle(name);
  registry.push({
    meta: {
      id: makeId(currentFile, title), title, file: currentFile,
      timeout: details?.timeout, retries: details?.retries, suite_id: currentSuiteId(),
      annotations: [...buildAnnotations(details), { fixme: { reason: null, condition: null } }],
    },
    body: wrapBody(body),
  });
};

/**
 * test.fail() — expect failure (inverts pass/fail).
 *
 * Runtime: test.fail() or test.fail(condition, reason)
 * Registration: test.fail('name', body)
 */
testFn.fail = (...args: any[]) => {
  if (args.length === 0 || typeof args[0] === 'boolean') {
    if (!_currentTestInfo) throw new Error('test.fail() can only be called inside a test body');
    _currentTestInfo.fail(args[0] ?? true, args[1]);
    return;
  }
  const [name, detailsOrBody, maybeBody] = args;
  const body = typeof detailsOrBody === 'function' ? detailsOrBody : maybeBody;
  const details = typeof detailsOrBody === 'function' ? undefined : detailsOrBody;
  const title = fullTitle(name);
  registry.push({
    meta: {
      id: makeId(currentFile, title), title, file: currentFile,
      timeout: details?.timeout, retries: details?.retries, suite_id: currentSuiteId(),
      annotations: [...buildAnnotations(details), { fail: { reason: null, condition: null } }],
    },
    body: wrapBody(body),
  });
};

/**
 * test.slow() — triple timeout.
 *
 * Runtime: test.slow() or test.slow(condition, reason)
 * Registration: test.slow('name', body)
 */
testFn.slow = (...args: any[]) => {
  if (args.length === 0 || typeof args[0] === 'boolean') {
    if (!_currentTestInfo) throw new Error('test.slow() can only be called inside a test body');
    _currentTestInfo.slow(args[0] ?? true, args[1]);
    return;
  }
  const [name, detailsOrBody, maybeBody] = args;
  const body = typeof detailsOrBody === 'function' ? detailsOrBody : maybeBody;
  const details = typeof detailsOrBody === 'function' ? undefined : detailsOrBody;
  const title = fullTitle(name);
  registry.push({
    meta: {
      id: makeId(currentFile, title), title, file: currentFile,
      timeout: details?.timeout, retries: details?.retries, suite_id: currentSuiteId(),
      annotations: [...buildAnnotations(details), { slow: { reason: null, condition: null } }],
    },
    body: wrapBody(body),
  });
};

testFn.only = (...args: any[]) => {
  hasOnly = true;
  const [name, detailsOrBody, maybeBody] = args;
  const body = typeof detailsOrBody === 'function' ? detailsOrBody : maybeBody;
  const details = typeof detailsOrBody === 'function' ? undefined : detailsOrBody;
  const title = fullTitle(name);
  registry.push({
    meta: {
      id: makeId(currentFile, title), title, file: currentFile,
      timeout: details?.timeout, retries: details?.retries, suite_id: currentSuiteId(),
      annotations: [...buildAnnotations(details), 'only'],
    },
    body: wrapBody(body),
  });
};

testFn.each = <T>(dataOrStrings: T[] | TemplateStringsArray, ...templateValues: any[]) => {
  const rows = Array.isArray(dataOrStrings) && !('raw' in dataOrStrings)
    ? dataOrStrings as T[]
    : parseTemplateTable(dataOrStrings as TemplateStringsArray, templateValues);
  return (nameTemplate: string, body: EachTestBody<T>) => {
    for (const row of rows) {
      const name = interpolateTemplate(nameTemplate, row);
      const title = fullTitle(name);
      registry.push({
        meta: { id: makeId(currentFile, title), title, file: currentFile, annotations: [], suite_id: currentSuiteId() },
        body: wrapBody((fixtures) => body(fixtures, row as T)),
      });
    }
  };
};

/** Runtime test info — delegates to NAPI TestInfo. */
testFn.info = () => {
  if (!_currentTestInfo) throw new Error('test.info() can only be called inside a test body');
  return _currentTestInfo;
};

/** Runtime timeout change — delegates to NAPI TestInfo.setTimeout(). */
testFn.setTimeout = (ms: number) => {
  if (!_currentTestInfo) throw new Error('test.setTimeout() can only be called inside a test body');
  _currentTestInfo.setTimeout(ms);
};

/** Step API — delegates to NAPI TestInfo.beginStep(). */
testFn.step = async <T>(title: string, body: () => T | Promise<T>): Promise<T> => {
  if (!_currentTestInfo) throw new Error('test.step() can only be called inside a test body');
  const handle = await _currentTestInfo.beginStep(title);
  try {
    const result = await body();
    await handle.end();
    return result;
  } catch (e: any) {
    await handle.end(e?.message ?? String(e));
    throw e;
  }
};

// ── Hooks ──

testFn.beforeAll = (titleOrFn: string | TestBody, maybeFn?: TestBody) => {
  const fn = typeof titleOrFn === 'function' ? titleOrFn : maybeFn!;
  if (!_runner) throw new Error('test.beforeAll() requires runner to be initialized');
  const suiteId = currentSuiteId() ?? '';
  _runner.registerHook({ suiteId, kind: 'beforeAll' }, wrapBody(fn));
};

testFn.afterAll = (titleOrFn: string | TestBody, maybeFn?: TestBody) => {
  const fn = typeof titleOrFn === 'function' ? titleOrFn : maybeFn!;
  if (!_runner) throw new Error('test.afterAll() requires runner to be initialized');
  const suiteId = currentSuiteId() ?? '';
  _runner.registerHook({ suiteId, kind: 'afterAll' }, wrapBody(fn));
};

testFn.beforeEach = (titleOrFn: string | TestBody, maybeFn?: TestBody) => {
  const fn = typeof titleOrFn === 'function' ? titleOrFn : maybeFn!;
  if (!_runner) throw new Error('test.beforeEach() requires runner to be initialized');
  const suiteId = currentSuiteId() ?? '';
  _runner.registerHook({ suiteId, kind: 'beforeEach' }, wrapBody(fn));
};

testFn.afterEach = (titleOrFn: string | TestBody, maybeFn?: TestBody) => {
  const fn = typeof titleOrFn === 'function' ? titleOrFn : maybeFn!;
  if (!_runner) throw new Error('test.afterEach() requires runner to be initialized');
  const suiteId = currentSuiteId() ?? '';
  _runner.registerHook({ suiteId, kind: 'afterEach' }, wrapBody(fn));
};

// ── test.expect (alias for the expect export) ──

// Lazily imported to avoid circular dependency.
let _expect: any = null;
Object.defineProperty(testFn, 'expect', {
  get: () => {
    if (!_expect) _expect = require('./expect.js').expect;
    return _expect;
  },
});

// ── test.extend() — custom fixtures ──

type FixtureFactory<T> = (fixtures: any, use: (value: T) => Promise<void>) => Promise<void>;

/**
 * test.extend() — Playwright-compatible custom fixture extension.
 *
 * Returns a new `test` function with additional fixtures. Custom fixture factories
 * run TS-side (same as Playwright where custom fixtures are JS functions).
 * The Rust core handles the built-in fixtures; custom ones wrap around them.
 *
 * Usage:
 *   const myTest = test.extend<{ myFixture: string }>({
 *     myFixture: async ({ page }, use) => {
 *       const value = await setup();
 *       await use(value);
 *       await teardown();
 *     },
 *   });
 *   myTest('uses custom fixture', async ({ page, myFixture }) => { ... });
 */
testFn.extend = <T extends Record<string, any>>(customFixtures: { [K in keyof T]: FixtureFactory<T[K]> }) => {
  // Create a new test function that wraps bodies to inject custom fixtures.
  // Follows Playwright's setup/use/teardown pattern.
  const extendedTest = (name: string, ...args: any[]) => {
    const body = typeof args[args.length - 1] === 'function' ? args.pop() as TestBody : undefined;
    const details = args[0] as TestDetails | undefined;
    if (!body) throw new Error('test() requires a body function');

    const wrappedBody: TestBody = async (baseFixtures: any) => {
      // Snapshot NAPI getters into a plain object so we can spread custom values in.
      const base: Record<string, any> = {};
      for (const k of Object.getOwnPropertyNames(Object.getPrototypeOf(baseFixtures))) {
        if (k !== 'constructor') try { base[k] = baseFixtures[k]; } catch { /* skip */ }
      }

      const customValues: Record<string, any> = {};
      const factoryCleanups: (() => void)[] = [];

      // Resolve each custom fixture via setup/use/teardown.
      for (const [fixtureName, factory] of Object.entries(customFixtures)) {
        const allFixtures = { ...base, ...customValues };

        let resolveUse: (() => void) | null = null;
        const useComplete = new Promise<void>((r) => { resolveUse = r; });
        let resolveSetup: ((v?: any) => void) | null = null;
        const setupComplete = new Promise<void>((r) => { resolveSetup = r; });

        const factoryDone = (factory as FixtureFactory<any>)(allFixtures, async (value) => {
          customValues[fixtureName] = value;
          resolveSetup!();
          await useComplete;
        });

        await setupComplete;
        factoryCleanups.push(() => { resolveUse!(); });
        factoryDone.catch(() => { resolveSetup?.(); });
      }

      const merged = { ...base, ...customValues };

      try {
        await body(merged);
      } finally {
        for (const cleanup of factoryCleanups.reverse()) cleanup();
        await new Promise<void>((r) => setTimeout(r, 0));
      }
    };

    if (details) {
      testFn(name, details, wrappedBody);
    } else {
      testFn(name, wrappedBody);
    }
  };

  // Copy all methods from the original test function.
  Object.assign(extendedTest, testFn);
  return extendedTest as typeof testFn;
};

// ── describe() ──

export function describe(name: string, fn: () => void): void {
  describeStack.push(name);
  useOverridesStack.push({});
  fn();
  useOverridesStack.pop();
  describeStack.pop();
}

describe.skip = (name: string, fn: () => void) => {
  describeStack.push(name);
  const startIdx = registry.length;
  fn();
  for (let i = startIdx; i < registry.length; i++) {
    registry[i].meta.annotations.push({ skip: { reason: null, condition: null } });
  }
  describeStack.pop();
};

describe.fixme = (name: string, fn: () => void) => {
  describeStack.push(name);
  const startIdx = registry.length;
  fn();
  for (let i = startIdx; i < registry.length; i++) {
    registry[i].meta.annotations.push({ fixme: { reason: null, condition: null } });
  }
  describeStack.pop();
};

describe.only = (name: string, fn: () => void) => {
  hasOnly = true;
  describeStack.push(name);
  const startIdx = registry.length;
  fn();
  for (let i = startIdx; i < registry.length; i++) {
    registry[i].meta.annotations.push('only');
  }
  describeStack.pop();
};

describe.serial = (name: string, fn: () => void) => {
  describeStack.push(name);
  if (_runner) {
    const suiteId = _runner.registerSuite({ name, file: currentFile, mode: 'serial' });
    suiteIdStack.push(suiteId);
  }
  fn();
  if (_runner) suiteIdStack.pop();
  describeStack.pop();
};

describe.parallel = (name: string, fn: () => void) => {
  describeStack.push(name);
  if (_runner) {
    const suiteId = _runner.registerSuite({ name, file: currentFile, mode: 'parallel' });
    suiteIdStack.push(suiteId);
  }
  fn();
  if (_runner) suiteIdStack.pop();
  describeStack.pop();
};

describe.configure = (opts: { mode?: 'serial' | 'parallel'; retries?: number; timeout?: number }) => {
  // Apply suite-level config to all tests registered in this scope.
  // Mode is handled by registering a suite; retries/timeout are stored as overrides.
  if (opts.mode && _runner) {
    const name = describeStack[describeStack.length - 1] || '';
    const suiteId = _runner.registerSuite({ name, file: currentFile, mode: opts.mode });
    suiteIdStack.push(suiteId);
    // Note: suiteId is NOT popped here — it persists for the describe scope.
    // The caller (describe()) handles scope cleanup.
  }
};

describe.each = <T>(data: T[]) => {
  return (nameTemplate: string, fn: (data: T) => void) => {
    for (const row of data) {
      const name = interpolateTemplate(nameTemplate, row);
      describeStack.push(name);
      fn(row);
      describeStack.pop();
    }
  };
};

// ── test.use() ──

/** Scope-level fixture overrides stack. Pushed by test.use(), popped when scope ends. */
const useOverridesStack: Record<string, any>[] = [{}];

function currentUseOverrides(): Record<string, any> | undefined {
  // Merge all overrides on the stack (inner scopes override outer).
  let merged: Record<string, any> | undefined;
  for (const o of useOverridesStack) {
    if (Object.keys(o).length > 0) {
      merged = { ...merged, ...o };
    }
  }
  return merged;
}

/**
 * test.use() — Playwright-compatible fixture overrides.
 *
 * Sets fixture options for all tests in the current scope (describe block).
 * Overrides are serialized as `use_options` on each test's TestMeta and
 * applied by the Rust worker when creating the browser context.
 */
testFn.use = (options: Record<string, any>) => {
  if (useOverridesStack.length > 0) {
    Object.assign(useOverridesStack[useOverridesStack.length - 1], options);
  }
};

// ── Internal state ──

export function _setCurrentFile(file: string): void {
  currentFile = file;
}

export function _drainTests(): RegisteredTest[] {
  const tests = [...registry];
  registry.length = 0;
  hasOnly = false;
  return tests;
}

export function _hasOnly(): boolean {
  return hasOnly;
}

// test.describe — alias for the standalone describe, matching Playwright.
(testFn as any).describe = describe;

export const test = testFn;
