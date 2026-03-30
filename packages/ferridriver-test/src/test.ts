/**
 * Test declaration API — mirrors Playwright's test() and test.describe().
 *
 * Tests are registered as callbacks. The Rust runner calls them with
 * a Page fixture and handles parallelism, retries, timeouts.
 */

import type { Page, TestMeta } from 'ferridriver';

/** Fixtures available in test callbacks. */
export interface TestFixtures {
  page: Page;
}

/** Test function signature. */
type TestBody = (fixtures: TestFixtures) => Promise<void>;

interface TestOptions {
  tag?: string | string[];
  timeout?: number;
  retries?: number;
}

// ── Global test registry ──

interface RegisteredTest {
  meta: TestMeta;
  body: (page: Page) => Promise<void>;
}

const registry: RegisteredTest[] = [];
const describeStack: string[] = [];
let hasOnly = false;

function fullTitle(name: string): string {
  return [...describeStack, name].join(' > ');
}

function makeId(file: string, title: string): string {
  return `${file}:${title}`;
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
      modifier: 'none',
      timeout: options.timeout,
      retries: options.retries,
      tags: options.tag ? (Array.isArray(options.tag) ? options.tag : [options.tag]) : undefined,
    },
    body: (page) => body({ page }),
  });
}

testFn.skip = (name: string, body: TestBody) => {
  const title = fullTitle(name);
  registry.push({
    meta: { id: makeId(currentFile, title), title, file: currentFile, modifier: 'skip' },
    body: (page) => body({ page }),
  });
};

testFn.only = (name: string, body: TestBody) => {
  hasOnly = true;
  const title = fullTitle(name);
  registry.push({
    meta: { id: makeId(currentFile, title), title, file: currentFile, modifier: 'only' },
    body: (page) => body({ page }),
  });
};

testFn.fixme = (name: string, body: TestBody) => {
  const title = fullTitle(name);
  registry.push({
    meta: { id: makeId(currentFile, title), title, file: currentFile, modifier: 'fixme' },
    body: (page) => body({ page }),
  });
};

// ── describe() ──

export function describe(name: string, fn: () => void): void {
  describeStack.push(name);
  fn();
  describeStack.pop();
}

describe.skip = (name: string, fn: () => void) => {
  // All tests inside get skip modifier
  describeStack.push(name);
  const startIdx = registry.length;
  fn();
  for (let i = startIdx; i < registry.length; i++) {
    registry[i].meta.modifier = 'skip';
  }
  describeStack.pop();
};

// ── Internal state ──

let currentFile = '';

/** Set the current file being loaded (called by the runner before importing each test file). */
export function _setCurrentFile(file: string): void {
  currentFile = file;
}

/** Get all registered tests and reset the registry. */
export function _drainTests(): RegisteredTest[] {
  const tests = [...registry];
  registry.length = 0;
  hasOnly = false;
  return tests;
}

/** Check if any test.only() was used. */
export function _hasOnly(): boolean {
  return hasOnly;
}

export const test = testFn;
