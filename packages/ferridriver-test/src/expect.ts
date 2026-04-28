/**
 * Auto-retrying assertions and Jest-compatible value matchers.
 *
 * Polling matchers (`toBeVisible`, `toHaveText`, …) delegate to Rust
 * via NAPI for zero-round-trip retries. Generic value matchers
 * (`toBe`, `toEqual`, asymmetric matchers, …) run JS-side because
 * they're pure-value comparisons and Playwright itself routes them
 * through Jest's `expect` library — same shape, no protocol calls.
 */
/* eslint-disable @typescript-eslint/no-explicit-any */

import type { Page, Locator, ApiResponse } from '@ferridriver/node';
import { _currentTestInfo } from './test.js';

const DEFAULT_TIMEOUT = 5000;

// ── Asymmetric-matcher tag + dispatch ─────────────────────────────────────

const ASYM = Symbol.for('ferridriver.asymmetric');

interface AsymmetricMatcher {
  [ASYM]: string;
  match: (actual: any) => boolean;
  describe: () => string;
}

function isAsymmetric(value: any): value is AsymmetricMatcher {
  return value != null && typeof value === 'object' && (value as any)[ASYM];
}

// ── Deep equality (recognises asymmetric matchers) ────────────────────────

function deepEqual(a: any, b: any, partial = false): boolean {
  // Asymmetric matcher on the expected side wins immediately.
  if (isAsymmetric(b)) return b.match(a);
  if (isAsymmetric(a)) return a.match(b);

  if (Object.is(a, b)) return true;
  if (a == null || b == null) return false;
  if (typeof a !== typeof b) return false;
  if (typeof a !== 'object') return false;

  if (a instanceof Date && b instanceof Date) return a.getTime() === b.getTime();
  if (a instanceof RegExp && b instanceof RegExp) return a.source === b.source && a.flags === b.flags;

  if (Array.isArray(a) && Array.isArray(b)) {
    if (a.length !== b.length) return false;
    for (let i = 0; i < a.length; i++) if (!deepEqual(a[i], b[i], partial)) return false;
    return true;
  }
  if (Array.isArray(a) !== Array.isArray(b)) return false;

  const aKeys = Object.keys(a);
  const bKeys = Object.keys(b);
  if (!partial && aKeys.length !== bKeys.length) return false;
  // For partial (toMatchObject), every key in b must match in a.
  // For exact (toEqual), every key in a must equal in b too.
  for (const key of bKeys) {
    if (!deepEqual(a[key], b[key], partial)) return false;
  }
  if (!partial) {
    for (const key of aKeys) {
      if (!(key in b)) return false;
    }
  }
  return true;
}

function pretty(value: any): string {
  try {
    if (isAsymmetric(value)) return value.describe();
    if (typeof value === 'function') return value.name || '<fn>';
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}

// ── Custom matcher registry (expect.extend) ───────────────────────────────

type CustomMatcherFn = (
  this: { isNot: boolean },
  actual: any,
  ...args: any[]
) => { pass: boolean; message: () => string };

const _customMatchers: Record<string, CustomMatcherFn> = {};

// ── Soft-error push ───────────────────────────────────────────────────────

function pushSoftError(message: string): void {
  const info = _currentTestInfo();
  if (info && typeof info.pushSoftError === 'function') {
    info.pushSoftError(message);
  }
}

// ── Page / Locator polling assertion classes ─────────────────────────────-

class PageAssertions {
  constructor(
    private page: Page,
    private isNot: boolean,
    private timeout: number,
    private soft: boolean,
  ) {}

  get not(): PageAssertions {
    return new PageAssertions(this.page, !this.isNot, this.timeout, this.soft);
  }

  private async _wrap(fn: () => Promise<void>): Promise<void> {
    if (!this.soft) return fn();
    try {
      await fn();
    } catch (e: any) {
      pushSoftError(e?.message || String(e));
    }
  }

  async toHaveTitle(expected: string): Promise<void> {
    await this._wrap(() => this.page.expectTitle(expected, this.isNot, this.timeout));
  }

  async toHaveURL(expected: string): Promise<void> {
    await this._wrap(() => this.page.expectUrl(expected, this.isNot, this.timeout));
  }
}

class LocatorAssertions {
  constructor(
    private locator: Locator,
    private isNot: boolean,
    private timeout: number,
    private soft: boolean,
  ) {}

  get not(): LocatorAssertions {
    return new LocatorAssertions(this.locator, !this.isNot, this.timeout, this.soft);
  }

  private async _wrap(fn: () => Promise<void>): Promise<void> {
    if (!this.soft) return fn();
    try {
      await fn();
    } catch (e: any) {
      pushSoftError(e?.message || String(e));
    }
  }

  async toBeVisible(): Promise<void> { await this._wrap(() => this.locator.expectVisible(this.isNot, this.timeout)); }
  async toBeHidden(): Promise<void> { await this._wrap(() => this.locator.expectHidden(this.isNot, this.timeout)); }
  async toBeEnabled(): Promise<void> { await this._wrap(() => this.locator.expectEnabled(this.isNot, this.timeout)); }
  async toBeDisabled(): Promise<void> { await this._wrap(() => this.locator.expectDisabled(this.isNot, this.timeout)); }
  async toBeChecked(): Promise<void> { await this._wrap(() => this.locator.expectChecked(this.isNot, this.timeout)); }
  async toHaveText(expected: string): Promise<void> { await this._wrap(() => this.locator.expectText(expected, this.isNot, this.timeout)); }
  async toContainText(expected: string): Promise<void> { await this._wrap(() => this.locator.expectContainText(expected, this.isNot, this.timeout)); }
  async toHaveValue(expected: string): Promise<void> { await this._wrap(() => this.locator.expectValue(expected, this.isNot, this.timeout)); }
  async toHaveAttribute(name: string, value: string): Promise<void> { await this._wrap(() => this.locator.expectAttribute(name, value, this.isNot, this.timeout)); }
  async toHaveCount(expected: number): Promise<void> { await this._wrap(() => this.locator.expectCount(expected, this.isNot, this.timeout)); }
}

// ── APIResponse assertion class ───────────────────────────────────────────

class ResponseAssertions {
  constructor(
    private response: ApiResponse,
    private isNot: boolean,
    private soft: boolean,
  ) {}

  get not(): ResponseAssertions {
    return new ResponseAssertions(this.response, !this.isNot, this.soft);
  }

  private _emit(message: string): void {
    if (this.soft) pushSoftError(message);
    else throw new Error(message);
  }

  toBeOK(): void {
    const ok = this.response.ok();
    if (ok === this.isNot) {
      const status = this.response.status;
      this._emit(
        this.isNot
          ? `expect(response).not.toBeOK(): status ${status} unexpectedly in 2xx range`
          : `expect(response).toBeOK(): status ${status} not in 2xx range`,
      );
    }
  }
}

// ── Generic value assertion class ─────────────────────────────────────────

class ValueAssertions {
  constructor(
    private actual: any,
    private isNot: boolean,
    private soft: boolean,
    private promise?: 'resolves' | 'rejects',
  ) {}

  get not(): ValueAssertions {
    return new ValueAssertions(this.actual, !this.isNot, this.soft, this.promise);
  }

  get resolves(): ValueAssertions {
    return new ValueAssertions(this.actual, this.isNot, this.soft, 'resolves');
  }

  get rejects(): ValueAssertions {
    return new ValueAssertions(this.actual, this.isNot, this.soft, 'rejects');
  }

  private _resolve(): Promise<{ ok: boolean; value: any }> {
    if (this.promise === 'resolves') {
      return Promise.resolve(this.actual)
        .then((v) => ({ ok: true, value: v }))
        .catch((e) => ({ ok: false, value: e }));
    }
    if (this.promise === 'rejects') {
      return Promise.resolve(this.actual)
        .then((v) => ({ ok: true, value: v }))
        .catch((e) => ({ ok: false, value: e }));
    }
    return Promise.resolve({ ok: true, value: this.actual });
  }

  private _emit(message: string): void {
    if (this.soft) pushSoftError(message);
    else throw new Error(message);
  }

  private async _evaluate(predicate: (actual: any) => boolean | Promise<boolean>, render: (actual: any) => string): Promise<void> {
    const resolved = await this._resolve();
    let actual: any;
    if (this.promise === 'resolves') {
      if (!resolved.ok) {
        this._emit(`expect(...).resolves: promise rejected with ${pretty(resolved.value)}`);
        return;
      }
      actual = resolved.value;
    } else if (this.promise === 'rejects') {
      if (resolved.ok) {
        this._emit(`expect(...).rejects: promise resolved with ${pretty(resolved.value)}`);
        return;
      }
      actual = resolved.value;
    } else {
      actual = resolved.value;
    }
    const passed = await predicate(actual);
    const matched = passed !== this.isNot;
    if (!matched) {
      this._emit(`${this.isNot ? 'expect.not(' : 'expect('}${pretty(actual)}): ${render(actual)}`);
    }
  }

  // ── Equality / identity ──
  async toBe(expected: any): Promise<void> {
    await this._evaluate(
      (a) => Object.is(a, expected),
      () => `toBe(${pretty(expected)})`,
    );
  }

  async toEqual(expected: any): Promise<void> {
    await this._evaluate(
      (a) => deepEqual(a, expected, false),
      () => `toEqual(${pretty(expected)})`,
    );
  }

  async toStrictEqual(expected: any): Promise<void> {
    await this.toEqual(expected);
  }

  async toMatchObject(expected: any): Promise<void> {
    await this._evaluate(
      (a) => deepEqual(a, expected, true),
      () => `toMatchObject(${pretty(expected)})`,
    );
  }

  // ── Truthiness ──
  async toBeTruthy(): Promise<void> {
    await this._evaluate((a) => Boolean(a), () => 'toBeTruthy()');
  }
  async toBeFalsy(): Promise<void> {
    await this._evaluate((a) => !a, () => 'toBeFalsy()');
  }
  async toBeDefined(): Promise<void> {
    await this._evaluate((a) => a !== undefined, () => 'toBeDefined()');
  }
  async toBeUndefined(): Promise<void> {
    await this._evaluate((a) => a === undefined, () => 'toBeUndefined()');
  }
  async toBeNull(): Promise<void> {
    await this._evaluate((a) => a === null, () => 'toBeNull()');
  }
  async toBeNaN(): Promise<void> {
    await this._evaluate((a) => Number.isNaN(a), () => 'toBeNaN()');
  }

  // ── Numeric ──
  async toBeGreaterThan(expected: number): Promise<void> {
    await this._evaluate((a) => a > expected, () => `toBeGreaterThan(${expected})`);
  }
  async toBeGreaterThanOrEqual(expected: number): Promise<void> {
    await this._evaluate((a) => a >= expected, () => `toBeGreaterThanOrEqual(${expected})`);
  }
  async toBeLessThan(expected: number): Promise<void> {
    await this._evaluate((a) => a < expected, () => `toBeLessThan(${expected})`);
  }
  async toBeLessThanOrEqual(expected: number): Promise<void> {
    await this._evaluate((a) => a <= expected, () => `toBeLessThanOrEqual(${expected})`);
  }
  async toBeCloseTo(expected: number, numDigits = 2): Promise<void> {
    const tol = Math.pow(10, -numDigits) / 2;
    await this._evaluate((a) => Math.abs(a - expected) < tol, () => `toBeCloseTo(${expected}, ${numDigits})`);
  }

  // ── Type ──
  async toBeInstanceOf(ctor: any): Promise<void> {
    await this._evaluate((a) => a instanceof ctor, () => `toBeInstanceOf(${ctor?.name ?? '?'})`);
  }

  // ── String / array containers ──
  async toContain(expected: any): Promise<void> {
    await this._evaluate(
      (a) => {
        if (typeof a === 'string') return a.includes(expected);
        if (Array.isArray(a)) return a.indexOf(expected) >= 0;
        return false;
      },
      () => `toContain(${pretty(expected)})`,
    );
  }

  async toContainEqual(expected: any): Promise<void> {
    await this._evaluate(
      (a) => Array.isArray(a) && a.some((v) => deepEqual(v, expected, false)),
      () => `toContainEqual(${pretty(expected)})`,
    );
  }

  async toHaveLength(expected: number): Promise<void> {
    await this._evaluate((a) => a != null && a.length === expected, () => `toHaveLength(${expected})`);
  }

  async toHaveProperty(path: string | string[], ...rest: [any?]): Promise<void> {
    const segments = Array.isArray(path) ? path : path.split('.');
    const checkValue = rest.length >= 1;
    const expectedValue = rest[0];
    await this._evaluate(
      (a) => {
        let cur = a;
        for (const seg of segments) {
          if (cur == null || !(seg in cur)) return false;
          cur = cur[seg];
        }
        if (checkValue) return deepEqual(cur, expectedValue, false);
        return true;
      },
      () => `toHaveProperty(${pretty(path)}${checkValue ? `, ${pretty(expectedValue)}` : ''})`,
    );
  }

  async toMatch(expected: string | RegExp): Promise<void> {
    await this._evaluate(
      (a) => {
        if (typeof a !== 'string') return false;
        return expected instanceof RegExp ? expected.test(a) : a.includes(expected);
      },
      () => `toMatch(${pretty(expected)})`,
    );
  }

  // ── Throw ──
  async toThrow(expected?: string | RegExp | (new (...args: any[]) => Error)): Promise<void> {
    let thrown: any = null;
    let didThrow = false;
    try {
      const value = typeof this.actual === 'function' ? this.actual() : this.actual;
      if (value && typeof value.then === 'function') await value;
    } catch (e) {
      thrown = e;
      didThrow = true;
    }
    let pass = didThrow;
    if (didThrow && expected !== undefined) {
      if (typeof expected === 'string') pass = (thrown?.message || String(thrown)).includes(expected);
      else if (expected instanceof RegExp) pass = expected.test(thrown?.message || String(thrown));
      else pass = thrown instanceof expected;
    }
    if (pass === this.isNot) {
      this._emit(this.isNot ? 'toThrow(): unexpected throw' : `toThrow(${expected !== undefined ? pretty(expected) : ''}): no throw`);
    }
  }

  async toThrowError(expected?: string | RegExp | (new (...args: any[]) => Error)): Promise<void> {
    await this.toThrow(expected);
  }

  /// Retry `actual` (must be a function) until it stops throwing,
  /// up to the timeout. Mirrors Playwright's
  /// `expect(async () => { ... }).toPass(options?)`.
  async toPass(options: ToPassOptions = {}): Promise<void> {
    if (typeof this.actual !== 'function') {
      throw new Error('expect(...).toPass() requires a function subject');
    }
    const block = this.actual as () => any;
    const timeout = options.timeout ?? DEFAULT_TIMEOUT;
    const intervals = options.intervals ?? [100, 250, 500, 1000];
    const deadline = Date.now() + timeout;
    let lastError: Error | undefined;
    let idx = 0;
    let attempts = 0;
    while (true) {
      attempts++;
      try {
        const value = block();
        if (value && typeof value.then === 'function') await value;
        return;
      } catch (e) {
        lastError = e as Error;
        const interval = intervals[Math.min(idx++, intervals.length - 1)];
        if (Date.now() + interval > deadline) break;
        await new Promise((r) => setTimeout(r, interval));
      }
    }
    const prefix = options.message ?? 'toPass()';
    const msg = `${prefix} failed after ${attempts} attempt(s) (${timeout}ms): ${lastError?.message ?? 'timed out'}`;
    if (this.soft) pushSoftError(msg);
    else throw new Error(msg);
  }
}

// Inject custom matchers as instance methods of ValueAssertions.
function _refreshCustomMatchers(): void {
  for (const [name, fn] of Object.entries(_customMatchers)) {
    if ((ValueAssertions.prototype as any)[name]) continue;
    (ValueAssertions.prototype as any)[name] = async function (this: any, ...args: any[]) {
      const result = fn.call({ isNot: this.isNot }, this.actual, ...args);
      if (result.pass === this.isNot) {
        const msg = result.message();
        if (this.soft) pushSoftError(msg);
        else throw new Error(msg);
      }
    };
  }
}

interface ToPassOptions {
  timeout?: number;
  intervals?: number[];
  message?: string;
}

// ── Polled value assertions (`expect.poll`) ───────────────────────────────

interface PollOptions {
  timeout?: number;
  intervals?: number[];
  message?: string;
}

class PollWrapper {
  constructor(private probe: () => Promise<any> | any, private options: PollOptions) {}

  private _make(modeNot: boolean): any {
    const probe = this.probe;
    const options = this.options;
    const inner = (predicate: (actual: any) => boolean, describe: () => string) => async () => {
      const timeout = options.timeout ?? DEFAULT_TIMEOUT;
      const intervals = options.intervals ?? [100, 250, 500, 1000];
      const deadline = Date.now() + timeout;
      let last: any;
      let idx = 0;
      while (true) {
        last = await probe();
        const matched = predicate(last) !== modeNot;
        if (matched) return;
        const interval = intervals[Math.min(idx++, intervals.length - 1)];
        if (Date.now() + interval > deadline) break;
        await new Promise((r) => setTimeout(r, interval));
      }
      const msg = options.message ?? `expect.poll(...).${modeNot ? 'not.' : ''}${describe()}`;
      throw new Error(`${msg} — last value: ${pretty(last)}`);
    };
    const self = this;
    return {
      get not() {
        return self._make(!modeNot);
      },
      toBe: (expected: any) => inner((a) => Object.is(a, expected), () => `toBe(${pretty(expected)})`)(),
      toEqual: (expected: any) => inner((a) => deepEqual(a, expected, false), () => `toEqual(${pretty(expected)})`)(),
      toBeTruthy: () => inner((a) => Boolean(a), () => 'toBeTruthy()')(),
      toBeFalsy: () => inner((a) => !a, () => 'toBeFalsy()')(),
      toBeGreaterThan: (n: number) => inner((a) => a > n, () => `toBeGreaterThan(${n})`)(),
      toBeLessThan: (n: number) => inner((a) => a < n, () => `toBeLessThan(${n})`)(),
      toContain: (expected: any) =>
        inner(
          (a) => (typeof a === 'string' ? a.includes(expected) : Array.isArray(a) && a.indexOf(expected) >= 0),
          () => `toContain(${pretty(expected)})`,
        )(),
      toMatch: (expected: string | RegExp) =>
        inner(
          (a) => (typeof a !== 'string' ? false : expected instanceof RegExp ? expected.test(a) : a.includes(expected)),
          () => `toMatch(${pretty(expected)})`,
        )(),
    };
  }

  build(): any {
    return this._make(false);
  }
}

// ── expect() entry point + asymmetric / soft / poll factories ─────────────

type Assertable = Page | Locator | ApiResponse;

function isPage(v: any): v is Page {
  return v != null && typeof v.goto === 'function' && typeof v.title === 'function';
}

function isApiResponse(v: any): v is ApiResponse {
  // ApiResponse has `status` getter (number), `text()` (Promise<string>),
  // `ok()` (boolean) — none of which look like a Page or Locator.
  return v != null && typeof v.ok === 'function' && typeof v.text === 'function' && typeof v.goto !== 'function';
}

function isLocator(v: any): v is Locator {
  return v != null && typeof v.click === 'function' && typeof v.textContent === 'function' && typeof v.goto !== 'function';
}

interface ExpectFn {
  // Page / Locator / ApiResponse overloads — these match before the
  // generic form so the polling matchers (`toBeVisible`, etc.) are
  // available without an explicit cast.
  (subject: Page, timeout?: number): PageAssertions;
  (subject: Locator, timeout?: number): LocatorAssertions;
  (subject: ApiResponse): ResponseAssertions;
  // Generic value form — also handles `() => ...` for `.toPass()` and
  // `.toThrow()`.
  <T>(actual: T): ValueAssertions;
}

interface ExpectStaticOps {
  soft: ExpectFn;
  poll: (probe: () => Promise<any> | any, options?: PollOptions) => any;
  extend: (matchers: Record<string, CustomMatcherFn>) => void;
  // Asymmetric matchers
  any: (constructor: any) => AsymmetricMatcher;
  anything: () => AsymmetricMatcher;
  arrayContaining: (subset: any[]) => AsymmetricMatcher;
  closeTo: (expected: number, numDigits?: number) => AsymmetricMatcher;
  objectContaining: (subset: Record<string, any>) => AsymmetricMatcher;
  stringContaining: (substring: string) => AsymmetricMatcher;
  stringMatching: (pattern: string | RegExp) => AsymmetricMatcher;
}

function buildExpect(soft: boolean): ExpectFn {
  function expectImpl(subject: any, timeout: number = DEFAULT_TIMEOUT): any {
    if (isPage(subject)) return new PageAssertions(subject as Page, false, timeout, soft);
    if (isApiResponse(subject)) return new ResponseAssertions(subject as ApiResponse, false, soft);
    if (isLocator(subject)) return new LocatorAssertions(subject as Locator, false, timeout, soft);
    return new ValueAssertions(subject, false, soft);
  }
  return expectImpl as ExpectFn;
}

const _expectFn: ExpectFn = buildExpect(false);
const _expectSoft: ExpectFn = buildExpect(true);

const _staticOps: ExpectStaticOps = {
  soft: _expectSoft,
  poll: (probe, options = {}) => new PollWrapper(probe, options).build(),
  extend: (matchers) => {
    for (const [name, fn] of Object.entries(matchers)) _customMatchers[name] = fn;
    _refreshCustomMatchers();
  },
  any: (constructor) => ({
    [ASYM]: 'any',
    match: (actual: any) => {
      if (constructor === String) return typeof actual === 'string';
      if (constructor === Number) return typeof actual === 'number' && !Number.isNaN(actual);
      if (constructor === Boolean) return typeof actual === 'boolean';
      if (constructor === Function) return typeof actual === 'function';
      if (constructor === Object) return typeof actual === 'object' && actual !== null;
      return actual instanceof constructor;
    },
    describe: () => `Any<${constructor?.name ?? '?'}>`,
  }),
  anything: () => ({
    [ASYM]: 'anything',
    match: (actual: any) => actual !== null && actual !== undefined,
    describe: () => 'Anything',
  }),
  arrayContaining: (subset) => ({
    [ASYM]: 'arrayContaining',
    match: (actual: any) => Array.isArray(actual) && subset.every((s) => actual.some((a) => deepEqual(a, s, false))),
    describe: () => `ArrayContaining(${pretty(subset)})`,
  }),
  closeTo: (expected, numDigits = 2) => {
    const tol = Math.pow(10, -numDigits) / 2;
    return {
      [ASYM]: 'closeTo',
      match: (actual: any) => typeof actual === 'number' && Math.abs(actual - expected) < tol,
      describe: () => `CloseTo(${expected}, ${numDigits})`,
    };
  },
  objectContaining: (subset) => ({
    [ASYM]: 'objectContaining',
    match: (actual: any) => actual != null && typeof actual === 'object' && deepEqual(actual, subset, true),
    describe: () => `ObjectContaining(${pretty(subset)})`,
  }),
  stringContaining: (substring) => ({
    [ASYM]: 'stringContaining',
    match: (actual: any) => typeof actual === 'string' && actual.includes(substring),
    describe: () => `StringContaining(${JSON.stringify(substring)})`,
  }),
  stringMatching: (pattern) => ({
    [ASYM]: 'stringMatching',
    match: (actual: any) => typeof actual === 'string' && (pattern instanceof RegExp ? pattern.test(actual) : actual.includes(pattern)),
    describe: () => `StringMatching(${pattern instanceof RegExp ? pattern.toString() : JSON.stringify(pattern)})`,
  }),
};

export const expect: ExpectFn & ExpectStaticOps = Object.assign(_expectFn, _staticOps);
