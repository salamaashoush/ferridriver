/**
 * Test declaration API — mirrors Playwright's test() and test.describe().
 *
 * Playwright-compatible API:
 *   test('name', async ({ page, browserName }) => {
 *     test.skip(browserName === 'firefox', 'not supported');
 *     test.slow();
 *     await page.goto('...');
 *   });
 *
 * The Rust test runner is the single execution engine. This module is a thin
 * registration layer — it collects test metadata and callbacks, then the CLI
 * hands them to the Rust runner via NAPI.
 */

import type { Page, TestMeta, TestRunner } from '@ferridriver/core';

/** Mount function for component testing. */
export type MountFunction = (
  component: any,
  options?: { props?: Record<string, any>; hooksConfig?: Record<string, any> },
) => Promise<void>;

/** Fixtures available in test callbacks — mirrors Playwright's fixtures. */
export interface TestFixtures {
  page: Page;
  browserName: string;
  headless: boolean;
  isMobile: boolean;
  hasTouch: boolean;
  colorScheme: string | null;
  locale: string | null;
  channel: string | null;
  /** Available in component testing mode (--ct). */
  mount: MountFunction;
}

/** Test function signature. */
type TestBody = (fixtures: TestFixtures) => Promise<void>;

/** Parameterized test body. */
type EachTestBody<T> = (fixtures: TestFixtures, data: T) => Promise<void>;

/** Options for test registration. */
interface TestOptions {
  tag?: string | string[];
  timeout?: number;
  retries?: number;
}

// ── Errors ──

/** Thrown by test.skip() / test.fixme() inside a test body — caught by the Rust worker. */
class TestSkipError extends Error {
  constructor(reason?: string) {
    super(`__FERRIDRIVER_SKIP__:${reason || ''}`);
    this.name = 'TestSkipError';
  }
}

// ── Runtime test context (set per-test before body executes) ──

interface RuntimeContext {
  browserName: string;
  headless: boolean;
  isMobile: boolean;
  hasTouch: boolean;
  colorScheme: string | null;
  locale: string | null;
  channel: string | null;
  /** Set by test.fail() inside body — wrapper handles inversion. */
  _expectedFailure: boolean;
  /** Set by test.slow() inside body — annotation added. */
  _slow: boolean;
}

let _currentContext: RuntimeContext | null = null;

// ── Global test registry ──

interface RegisteredTest {
  meta: TestMeta;
  body: (page: Page) => Promise<void>;
}

const registry: RegisteredTest[] = [];
const describeStack: string[] = [];
let hasOnly = false;

// ── Runner reference (set by CLI after TestRunner.create()) ──

let _runner: TestRunner | null = null;

/** Called by CLI to provide the runner for fixture resolution. */
export function _setRunner(runner: TestRunner): void {
  _runner = runner;
}

// ── Component testing ──

let _ctMountFactory: ((page: Page) => MountFunction) | null = null;

export function _setCtMountFactory(factory: (page: Page) => MountFunction): void {
  _ctMountFactory = factory;
}

function createMount(page: Page): MountFunction {
  if (_ctMountFactory) return _ctMountFactory(page);
  return async () => { throw new Error('mount() is only available in component testing mode (--ct)'); };
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
  if (typeof data !== 'object' || data === null) {
    return template.replace(/\$\{?\w+\}?/g, String(data));
  }
  return template.replace(/\$(\w+)/g, (_, key) => key in data ? String(data[key]) : `$${key}`);
}

function fullTitle(name: string): string {
  return [...describeStack, name].join(' > ');
}

function makeId(file: string, title: string): string {
  return `${file}:${title}`;
}

// ── Annotation builder ──

function buildAnnotations(options: TestOptions, extra?: any[]): any[] {
  const annotations: any[] = extra ? [...extra] : [];
  if (options.tag) {
    const tags = Array.isArray(options.tag) ? options.tag : [options.tag];
    for (const t of tags) annotations.push({ tag: t });
  }
  return annotations;
}

// ── Body wrapper: injects fixtures from runner config, handles runtime modifiers ──

function wrapBody(body: TestBody): (page: Page) => Promise<void> {
  return async (page: Page) => {
    // Build fixture object from runner config (set by CLI).
    const ctx: RuntimeContext = {
      browserName: _runner?.getBrowserName() ?? 'chromium',
      headless: _runner?.getHeadless() ?? true,
      isMobile: _runner?.getIsMobile() ?? false,
      hasTouch: _runner?.getHasTouch() ?? false,
      colorScheme: _runner?.getColorScheme() ?? null,
      locale: _runner?.getLocale() ?? null,
      channel: _runner?.getChannel() ?? null,
      _expectedFailure: false,
      _slow: false,
    };
    _currentContext = ctx;

    const fixtures: TestFixtures = {
      page,
      browserName: ctx.browserName,
      headless: ctx.headless,
      isMobile: ctx.isMobile,
      hasTouch: ctx.hasTouch,
      colorScheme: ctx.colorScheme,
      locale: ctx.locale,
      channel: ctx.channel,
      mount: createMount(page),
    };

    try {
      await body(fixtures);
      // Body passed. If test.fail() was called, this is unexpected → report failure.
      if (ctx._expectedFailure) {
        throw new Error('expected test to fail, but it passed');
      }
    } catch (e) {
      // test.skip() / test.fixme() → TestSkipError → propagate as-is for Rust worker.
      if (e instanceof TestSkipError) throw e;
      // test.fail() was called and body failed → expected, swallow error (report pass).
      if (ctx._expectedFailure) return;
      // Normal failure → propagate.
      throw e;
    } finally {
      _currentContext = null;
    }
  };
}

// ── test() ──

function testFn(name: string, body: TestBody): void;
function testFn(name: string, options: TestOptions, body: TestBody): void;
function testFn(name: string, optionsOrBody: TestOptions | TestBody, maybeBody?: TestBody): void {
  const body = typeof optionsOrBody === 'function' ? optionsOrBody : maybeBody!;
  const options = typeof optionsOrBody === 'function' ? {} : optionsOrBody;
  const title = fullTitle(name);
  registry.push({
    meta: {
      id: makeId(currentFile, title),
      title,
      file: currentFile,
      timeout: options.timeout,
      retries: options.retries,
      annotations: buildAnnotations(options),
    },
    body: wrapBody(body),
  });
}

// ── Runtime modifiers (Playwright API) ──
// Called inside test body: test.skip(condition, reason)

/**
 * test.skip() — Playwright-compatible.
 *
 * At describe level (registration time):
 *   test.skip('name', body)  — register a skipped test
 *
 * Inside test body (runtime):
 *   test.skip(browserName === 'firefox', 'not supported')
 *   test.skip()  — unconditional skip
 */
testFn.skip = (...args: any[]) => {
  // Runtime: test.skip() or test.skip(condition, reason)
  if (args.length === 0 || typeof args[0] === 'boolean') {
    const condition = args[0] ?? true;
    const reason = args[1] as string | undefined;
    if (condition) throw new TestSkipError(reason);
    return;
  }
  // Registration: test.skip('name', body)
  const [name, body] = args as [string, TestBody];
  const title = fullTitle(name);
  registry.push({
    meta: {
      id: makeId(currentFile, title),
      title,
      file: currentFile,
      annotations: [{ skip: { reason: null, condition: null } }],
    },
    body: wrapBody(body),
  });
};

/**
 * test.fixme() — same as skip, communicates intent to fix.
 */
testFn.fixme = (...args: any[]) => {
  if (args.length === 0 || typeof args[0] === 'boolean') {
    const condition = args[0] ?? true;
    const reason = args[1] as string | undefined;
    if (condition) throw new TestSkipError(reason);
    return;
  }
  const [name, body] = args as [string, TestBody];
  const title = fullTitle(name);
  registry.push({
    meta: {
      id: makeId(currentFile, title),
      title,
      file: currentFile,
      annotations: [{ fixme: { reason: null, condition: null } }],
    },
    body: wrapBody(body),
  });
};

/**
 * test.fail() — expect failure (inverts pass/fail).
 *
 * Inside test body:
 *   test.fail()  — unconditional
 *   test.fail(browserName === 'webkit', 'known bug')
 */
testFn.fail = (...args: any[]) => {
  if (args.length === 0 || typeof args[0] === 'boolean') {
    const condition = args[0] ?? true;
    if (condition && _currentContext) {
      _currentContext._expectedFailure = true;
    }
    return;
  }
  // Registration: test.fail('name', body)
  const [name, body] = args as [string, TestBody];
  const title = fullTitle(name);
  registry.push({
    meta: {
      id: makeId(currentFile, title),
      title,
      file: currentFile,
      annotations: [{ fail: { reason: null, condition: null } }],
    },
    body: wrapBody(body),
  });
};

/**
 * test.slow() — triple timeout.
 *
 * Inside test body:
 *   test.slow()
 *   test.slow(condition, reason)
 *
 * At registration:
 *   test.slow('name', body)
 */
testFn.slow = (...args: any[]) => {
  if (args.length === 0 || typeof args[0] === 'boolean') {
    const condition = args[0] ?? true;
    if (condition && _currentContext) {
      _currentContext._slow = true;
    }
    return;
  }
  const [name, body] = args as [string, TestBody];
  const title = fullTitle(name);
  registry.push({
    meta: {
      id: makeId(currentFile, title),
      title,
      file: currentFile,
      annotations: [{ slow: { reason: null, condition: null } }],
    },
    body: wrapBody(body),
  });
};

testFn.only = (name: string, body: TestBody) => {
  hasOnly = true;
  const title = fullTitle(name);
  registry.push({
    meta: { id: makeId(currentFile, title), title, file: currentFile, annotations: ['only'] },
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
        meta: { id: makeId(currentFile, title), title, file: currentFile, annotations: [] },
        body: wrapBody((fixtures) => body(fixtures, row as T)),
      });
    }
  };
};

/** Runtime test info. */
testFn.info = () => {
  if (!_currentContext) throw new Error('test.info() can only be called inside a test body');
  return {
    browserName: _currentContext.browserName,
    headless: _currentContext.headless,
    isMobile: _currentContext.isMobile,
    hasTouch: _currentContext.hasTouch,
    colorScheme: _currentContext.colorScheme,
    locale: _currentContext.locale,
    channel: _currentContext.channel,
  };
};

// ── describe() ──

export function describe(name: string, fn: () => void): void {
  describeStack.push(name);
  fn();
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

// ── Internal state ──

let currentFile = '';

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

export const test = testFn;
