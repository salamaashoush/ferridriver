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

/** Parameterized test body — receives fixtures + data row. */
type EachTestBody<T> = (fixtures: TestFixtures, data: T) => Promise<void>;

interface TestOptions {
  tag?: string | string[];
  timeout?: number;
  retries?: number;
  /** Mark as fixme. `true` = unconditional, string = condition (e.g. "linux", "webkit", "ci"). */
  fixme?: boolean | string;
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

/**
 * Parse a tagged template literal table into an array of objects.
 * Format:
 *   test.each`
 *     role       | email
 *     ${'admin'} | ${'admin@example.com'}
 *     ${'guest'} | ${'guest@example.com'}
 *   `
 * First row is headers, subsequent rows are values interpolated from ${} expressions.
 */
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
      if (cell.includes('\x00')) {
        row[headers[j]] = values[valIdx++];
      } else {
        row[headers[j]] = cell;
      }
    }
    rows.push(row);
  }
  return rows;
}

/** Interpolate `$variable` placeholders in test names with values from a data row. */
function interpolateTemplate(template: string, data: any): string {
  if (typeof data !== 'object' || data === null) {
    return template.replace(/\$\{?\w+\}?/g, String(data));
  }
  return template.replace(/\$(\w+)/g, (_, key) => {
    return key in data ? String(data[key]) : `$${key}`;
  });
}

function fullTitle(name: string): string {
  return [...describeStack, name].join(' > ');
}

function makeId(file: string, title: string): string {
  return `${file}:${title}`;
}

// ── test() ──

/** Build annotations array matching Rust TestAnnotation serde format. */
function buildAnnotations(options: TestOptions, extra?: any[]): any[] {
  const annotations: any[] = extra ? [...extra] : [];
  if (options.fixme === true) {
    annotations.push({ fixme: { reason: null, condition: null } });
  } else if (typeof options.fixme === 'string') {
    annotations.push({ fixme: { reason: null, condition: options.fixme } });
  }
  if (options.tag) {
    const tags = Array.isArray(options.tag) ? options.tag : [options.tag];
    for (const t of tags) annotations.push({ tag: t });
  }
  return annotations;
}

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
    body: (page) => body({ page, mount: createMount(page) }),
  });
}

testFn.skip = (name: string, body: TestBody) => {
  const title = fullTitle(name);
  registry.push({
    meta: { id: makeId(currentFile, title), title, file: currentFile, annotations: [{ skip: { reason: null } }] },
    body: (page) => body({ page, mount: createMount(page) }),
  });
};

testFn.only = (name: string, body: TestBody) => {
  hasOnly = true;
  const title = fullTitle(name);
  registry.push({
    meta: { id: makeId(currentFile, title), title, file: currentFile, annotations: ['only'] },
    body: (page) => body({ page, mount: createMount(page) }),
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
        meta: {
          id: makeId(currentFile, title),
          title,
          file: currentFile,
          annotations: [],
        },
        body: (page) => body({ page, mount: createMount(page) }, row as T),
      });
    }
  };
};

testFn.fixme = (name: string, body: TestBody) => {
  const title = fullTitle(name);
  registry.push({
    meta: { id: makeId(currentFile, title), title, file: currentFile, annotations: [{ fixme: { reason: null, condition: null } }] },
    body: (page) => body({ page, mount: createMount(page) }),
  });
};

/** Runtime test info — accessible inside test bodies. */
class TestInfoRuntime {
  annotations: Array<{ type: string; description: string }> = [];
}

/** Current test info instance (set per-test). */
let currentTestInfo: TestInfoRuntime | null = null;

/** Get the current test's info for annotations. */
testFn.info = (): TestInfoRuntime => {
  if (!currentTestInfo) {
    currentTestInfo = new TestInfoRuntime();
  }
  return currentTestInfo;
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
    registry[i].meta.annotations.push({ skip: { reason: null } });
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
