/**
 * Test declaration API — mirrors Playwright's test() and test.describe().
 *
 * Tests are registered as callbacks. The Rust runner calls them with
 * a Page fixture and handles parallelism, retries, timeouts.
 *
 * In component testing mode (--ct), a `mount` fixture is also provided.
 */

import type { Page, TestMeta } from 'ferridriver';

/** Mount function for component testing. */
export type MountFunction = (
  component: any,
  options?: { props?: Record<string, any>; hooksConfig?: Record<string, any> },
) => Promise<void>;

/** Fixtures available in test callbacks. */
export interface TestFixtures {
  page: Page;
  /** Available in component testing mode (--ct). Mounts a component into the page. */
  mount: MountFunction;
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

// ── Component testing mount function (set by CLI in --ct mode) ──

let _ctMountFactory: ((page: Page) => MountFunction) | null = null;

/** Called by the CLI to inject the mount factory for CT mode. */
export function _setCtMountFactory(factory: (page: Page) => MountFunction): void {
  _ctMountFactory = factory;
}

function createMount(page: Page): MountFunction {
  if (_ctMountFactory) {
    return _ctMountFactory(page);
  }
  // E2E mode — mount not available.
  return async () => {
    throw new Error('mount() is only available in component testing mode (--ct)');
  };
}

// ── Helpers ──

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
    body: (page) => body({ page, mount: createMount(page) }),
  });
}

testFn.skip = (name: string, body: TestBody) => {
  const title = fullTitle(name);
  registry.push({
    meta: { id: makeId(currentFile, title), title, file: currentFile, modifier: 'skip' },
    body: (page) => body({ page, mount: createMount(page) }),
  });
};

testFn.only = (name: string, body: TestBody) => {
  hasOnly = true;
  const title = fullTitle(name);
  registry.push({
    meta: { id: makeId(currentFile, title), title, file: currentFile, modifier: 'only' },
    body: (page) => body({ page, mount: createMount(page) }),
  });
};

testFn.fixme = (name: string, body: TestBody) => {
  const title = fullTitle(name);
  registry.push({
    meta: { id: makeId(currentFile, title), title, file: currentFile, modifier: 'fixme' },
    body: (page) => body({ page, mount: createMount(page) }),
  });
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
    registry[i].meta.modifier = 'skip';
  }
  describeStack.pop();
};

describe.only = (name: string, fn: () => void) => {
  hasOnly = true;
  describeStack.push(name);
  const startIdx = registry.length;
  fn();
  for (let i = startIdx; i < registry.length; i++) {
    registry[i].meta.modifier = 'only';
  }
  describeStack.pop();
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
