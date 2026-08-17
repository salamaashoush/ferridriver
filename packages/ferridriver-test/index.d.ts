// Type declarations for ferridriver's native TypeScript test runner.
//
// `import { test, describe, expect } from '@ferridriver/test'` resolves
// at RUN time to a native module inside the embedded QuickJS engine
// (`ferridriver test`); this package carries only the editor/typecheck
// surface. The declarations cover the shipped binding surface — when a
// binding gains a method, it is added here in the same change. There
// are intentionally no index-signature escapes: a missing declaration
// is a visible type error, not a silently-any call.

// ── Test runner ──────────────────────────────────────────────────────

export interface TestDetailsAnnotation {
  type: string;
  description?: string;
}

export interface TestDetails {
  tag?: string | string[];
  annotation?: TestDetailsAnnotation | TestDetailsAnnotation[];
  timeout?: number;
  retries?: number;
}

export interface TestProject {
  name: string;
}

export interface TestInfo {
  readonly title: string;
  readonly titlePath: string[];
  readonly file: string;
  readonly line: number;
  readonly column: number;
  readonly retry: number;
  readonly workerIndex: number;
  readonly parallelIndex: number;
  readonly repeatEachIndex: number;
  readonly timeout: number;
  readonly expectedStatus: 'passed' | 'failed' | 'timedOut' | 'skipped';
  readonly tags: string[];
  readonly outputDir: string;
  readonly snapshotDir: string;
  readonly snapshotSuffix: string;
  readonly project: TestProject | null;
  readonly annotations: TestDetailsAnnotation[];
  readonly attachmentCount: number;
  readonly errors: string[];
  attach(
    name: string,
    contentType: string,
    body: string | Uint8Array | ArrayBuffer | Buffer,
    options?: undefined
  ): Promise<void>;
  attach(name: string, options: { body?: string | Uint8Array | ArrayBuffer | Buffer; contentType?: string; path?: string }): Promise<void>;
  annotate(type: string, description?: string): void;
  skip(): void;
  skip(condition: boolean, description?: string): void;
  fixme(): void;
  fixme(condition: boolean, description?: string): void;
  fail(): void;
  fail(condition: boolean, description?: string): void;
  slow(): void;
  slow(condition: boolean, description?: string): void;
  setTimeout(timeout: number): void;
  outputPath(...pathSegments: string[]): string;
  snapshotPath(name: string): string;
}

export interface TestFixtures {
  page: Page;
  context: BrowserContext;
  request: APIRequestContext;
  browser: Browser;
  browserName: string;
  headless: boolean;
  isMobile: boolean;
  hasTouch: boolean;
  baseURL: string | undefined;
  testInfo: TestInfo;
}

export type TestBody<TFixtures> = (fixtures: TFixtures, testInfoOrRow?: any) => void | Promise<void>;

export interface DescribeFunction {
  (title: string, body: () => void): void;
  serial(title: string, body: () => void): void;
  parallel(title: string, body: () => void): void;
  skip(title: string, body: () => void): void;
  fixme(title: string, body: () => void): void;
  only(title: string, body: () => void): void;
  each<Row>(rows: Row[]): (titleTemplate: string, body: (row: Row) => void) => void;
  configure(options: { mode?: 'serial' | 'parallel' | 'default'; retries?: number; timeout?: number }): void;
}

export type FixtureScope = 'test' | 'worker';

export type FixtureValue<T, TFixtures> =
  | ((fixtures: TFixtures, use: (value: T) => Promise<void>) => void | Promise<void>)
  | [
      T | ((fixtures: TFixtures, use: (value: T) => Promise<void>) => void | Promise<void>),
      { scope?: FixtureScope; auto?: boolean; option?: boolean },
    ];

export interface TestType<TFixtures = TestFixtures> {
  (title: string, body: TestBody<TFixtures>): void;
  (title: string, details: TestDetails, body: TestBody<TFixtures>): void;

  skip(title: string, body: TestBody<TFixtures>): void;
  skip(title: string, details: TestDetails, body: TestBody<TFixtures>): void;
  skip(): void;
  skip(condition: boolean, description?: string): void;

  fixme(title: string, body: TestBody<TFixtures>): void;
  fixme(title: string, details: TestDetails, body: TestBody<TFixtures>): void;
  fixme(): void;
  fixme(condition: boolean, description?: string): void;

  fail(title: string, body: TestBody<TFixtures>): void;
  fail(title: string, details: TestDetails, body: TestBody<TFixtures>): void;
  fail(): void;
  fail(condition: boolean, description?: string): void;

  slow(title: string, body: TestBody<TFixtures>): void;
  slow(title: string, details: TestDetails, body: TestBody<TFixtures>): void;
  slow(): void;
  slow(condition: boolean, description?: string): void;

  only(title: string, body: TestBody<TFixtures>): void;
  only(title: string, details: TestDetails, body: TestBody<TFixtures>): void;

  each<Row>(rows: Row[]): (titleTemplate: string, body: (fixtures: TFixtures, row: Row) => void | Promise<void>) => void;

  beforeAll(body: (fixtures: Partial<TFixtures>, testInfo: TestInfo) => void | Promise<void>): void;
  beforeAll(title: string, body: (fixtures: Partial<TFixtures>, testInfo: TestInfo) => void | Promise<void>): void;
  afterAll(body: (fixtures: Partial<TFixtures>, testInfo: TestInfo) => void | Promise<void>): void;
  afterAll(title: string, body: (fixtures: Partial<TFixtures>, testInfo: TestInfo) => void | Promise<void>): void;
  beforeEach(body: (fixtures: TFixtures, testInfo: TestInfo) => void | Promise<void>): void;
  beforeEach(title: string, body: (fixtures: TFixtures, testInfo: TestInfo) => void | Promise<void>): void;
  afterEach(body: (fixtures: TFixtures, testInfo: TestInfo) => void | Promise<void>): void;
  afterEach(title: string, body: (fixtures: TFixtures, testInfo: TestInfo) => void | Promise<void>): void;

  use(options: Record<string, unknown>): void;
  setTimeout(timeout: number): void;
  info(): TestInfo;
  step<T>(title: string, body: () => T | Promise<T>): Promise<T>;
  extend<T extends object>(fixtures: {
    [K in keyof T]: FixtureValue<T[K], TFixtures & T>;
  }): TestType<TFixtures & T>;

  describe: DescribeFunction;
}

export const test: TestType;
export const describe: DescribeFunction;

/// The unextended root `test`, before any `extend` — Playwright's
/// `_baseTest`. It is the same object as `test`, exported under the name
/// a suite that composes fixture chains expects.
export const _baseTest: TestType;

/** The fixtures of every `test` in the list, intersected. */
type MergedFixtures<List> = List extends [TestType<infer T>, ...infer Rest] ? T & MergedFixtures<Rest> : TestFixtures;

/**
 * Compose independent `test.extend` chains into one `test`. Fixtures the
 * chains share are registered once, so a shared registration never
 * becomes an override of itself.
 */
export function mergeTests<List extends unknown[]>(...tests: List): TestType<MergedFixtures<List>>;

/** Secondary browser factories, independent of the project's backend. */
export const chromium: (options?: { transport?: 'pipe' | 'ws' }) => BrowserType;
export const firefox: () => BrowserType;
export const webkit: () => BrowserType;
/** The session's HTTP client, the same object the `request` fixture holds. */
export const request: APIRequestContext;

/** The module object is the `test` function itself, as in Playwright. */
declare const frameworkModule: TestType;
export default frameworkModule;

// ── Expect ───────────────────────────────────────────────────────────

export interface ExpectMatcherOptions {
  timeout?: number;
}

export interface TextMatcherOptions extends ExpectMatcherOptions {
  ignoreCase?: boolean;
  useInnerText?: boolean;
}

export interface ScreenshotAssertionOptions {
  threshold?: number;
  maxDiffPixels?: number;
  maxDiffPixelRatio?: number;
  animations?: 'disabled' | 'allow';
  caret?: 'hide' | 'initial';
  scale?: 'css' | 'device';
  stylePath?: string;
  mask?: string[];
  maskColor?: string;
  clip?: { x: number; y: number; width: number; height: number };
}

export interface GenericMatchers {
  toBe(expected: unknown): void;
  toEqual(expected: unknown): void;
  toStrictEqual(expected: unknown): void;
  toBeNull(): void;
  toBeUndefined(): void;
  toBeDefined(): void;
  toBeTruthy(): void;
  toBeFalsy(): void;
  toBeNaN(): void;
  toBeCloseTo(expected: number, numDigits?: number): void;
  toBeGreaterThan(expected: number): void;
  toBeGreaterThanOrEqual(expected: number): void;
  toBeLessThan(expected: number): void;
  toBeLessThanOrEqual(expected: number): void;
  toContain(expected: unknown): void;
  toContainEqual(expected: unknown): void;
  toHaveLength(length: number): void;
  toHaveProperty(path: string | Array<string | number>, value?: unknown): void;
  toMatch(pattern: string | RegExp): void;
  toMatchObject(subset: object): void;
  toBeInstanceOf(ctor: Function): void;
  toThrow(matcher?: string | RegExp | Function | { message?: string | RegExp; name?: string }): void | Promise<void>;
  toMatchSnapshot(name?: string): Promise<void>;
}

export interface WebFirstMatchers {
  toBeVisible(options?: ExpectMatcherOptions & { visible?: boolean }): Promise<void>;
  toBeHidden(options?: ExpectMatcherOptions): Promise<void>;
  toBeEnabled(options?: ExpectMatcherOptions & { enabled?: boolean }): Promise<void>;
  toBeDisabled(options?: ExpectMatcherOptions): Promise<void>;
  toBeChecked(options?: ExpectMatcherOptions & { checked?: boolean }): Promise<void>;
  toBeEditable(options?: ExpectMatcherOptions & { editable?: boolean }): Promise<void>;
  toBeAttached(options?: ExpectMatcherOptions & { attached?: boolean }): Promise<void>;
  toBeEmpty(options?: ExpectMatcherOptions): Promise<void>;
  toBeFocused(options?: ExpectMatcherOptions): Promise<void>;
  toBeInViewport(options?: ExpectMatcherOptions & { ratio?: number }): Promise<void>;
  toHaveText(expected: string | RegExp | Array<string | RegExp>, options?: TextMatcherOptions): Promise<void>;
  toContainText(expected: string | RegExp | Array<string | RegExp>, options?: TextMatcherOptions): Promise<void>;
  toHaveTexts(expected: Array<string | RegExp>, options?: ExpectMatcherOptions): Promise<void>;
  toContainTexts(expected: Array<string | RegExp>, options?: ExpectMatcherOptions): Promise<void>;
  toHaveValue(expected: string | RegExp, options?: ExpectMatcherOptions): Promise<void>;
  toHaveValues(expected: Array<string | RegExp>, options?: ExpectMatcherOptions): Promise<void>;
  toHaveAttribute(name: string, value: string | RegExp, options?: ExpectMatcherOptions & { ignoreCase?: boolean }): Promise<void>;
  toHaveAttribute(name: string, options?: ExpectMatcherOptions): Promise<void>;
  toHaveClass(expected: string | RegExp | Array<string | RegExp>, options?: ExpectMatcherOptions): Promise<void>;
  toContainClass(expected: string, options?: ExpectMatcherOptions): Promise<void>;
  toHaveCSS(name: string, value: string | RegExp, options?: ExpectMatcherOptions & { pseudo?: string }): Promise<void>;
  toHaveId(expected: string | RegExp, options?: ExpectMatcherOptions): Promise<void>;
  toHaveRole(role: string, options?: ExpectMatcherOptions): Promise<void>;
  toHaveAccessibleName(expected: string | RegExp, options?: ExpectMatcherOptions): Promise<void>;
  toHaveAccessibleDescription(expected: string | RegExp, options?: ExpectMatcherOptions): Promise<void>;
  toHaveAccessibleErrorMessage(expected: string | RegExp, options?: ExpectMatcherOptions): Promise<void>;
  toHaveJSProperty(name: string, value: unknown, options?: ExpectMatcherOptions): Promise<void>;
  toHaveCount(count: number, options?: ExpectMatcherOptions): Promise<void>;
  toMatchSnapshot(name?: string): Promise<void>;
  toHaveScreenshot(name?: string, options?: ScreenshotAssertionOptions): Promise<void>;
  toHaveScreenshot(options?: ScreenshotAssertionOptions): Promise<void>;
  toMatchAriaSnapshot(expected: string, options?: ExpectMatcherOptions): Promise<void>;
}

export interface PageMatchers {
  toHaveTitle(expected: string | RegExp, options?: ExpectMatcherOptions): Promise<void>;
  toContainTitle(expected: string, options?: ExpectMatcherOptions): Promise<void>;
  toHaveURL(expected: string | RegExp, options?: ExpectMatcherOptions & { ignoreCase?: boolean }): Promise<void>;
  toContainURL(expected: string, options?: ExpectMatcherOptions): Promise<void>;
  toHaveScreenshot(name?: string, options?: ScreenshotAssertionOptions): Promise<void>;
  toHaveScreenshot(options?: ScreenshotAssertionOptions): Promise<void>;
  toMatchAriaSnapshot(expected: string, options?: ExpectMatcherOptions): Promise<void>;
}

export interface APIResponseMatchers {
  toBeOK(): void;
}

// Every value matcher works on every receiver at runtime, but a
// Locator / Page / APIResponse subject only TYPES the six Playwright
// allows (`AllowedGenericMatchers`, types/test.d.ts:8842) — the rest
// would be a mistake on a handle rather than on its value.
export type AllowedGenericMatchers = Pick<
  GenericMatchers,
  'toBe' | 'toBeDefined' | 'toBeFalsy' | 'toBeNull' | 'toBeTruthy' | 'toBeUndefined'
>;

export type LocatorAssertions = WebFirstMatchers &
  AllowedGenericMatchers & { not: WebFirstMatchers & AllowedGenericMatchers };
export type PageAssertions = PageMatchers & AllowedGenericMatchers & { not: PageMatchers & AllowedGenericMatchers };
export type APIResponseAssertions = APIResponseMatchers &
  AllowedGenericMatchers & { not: APIResponseMatchers & AllowedGenericMatchers };
// `.resolves` / `.rejects` settle the subject first, so every matcher
// under them returns a Promise and must be awaited. The settled value is
// dispatched afresh, so a promise resolving to a Locator reaches the
// Locator matchers — the same rule Playwright's `MakeMatchers` applies to
// `Awaited<T>`.
export type Promisified<T> = {
  [K in keyof T]: T[K] extends (...args: infer A) => unknown ? (...args: A) => Promise<void> : T[K];
};

// The `[T] extends [X]` form is deliberate: a naked type parameter
// distributes, and `Awaited<Promise<never>>` is `never`, which would
// distribute to `never` and leave the chain with no matchers at all.
export type MatchersFor<T> = [T] extends [Locator]
  ? WebFirstMatchers & AllowedGenericMatchers
  : [T] extends [Page]
    ? PageMatchers & AllowedGenericMatchers
    : [T] extends [APIResponse]
      ? APIResponseMatchers & AllowedGenericMatchers
      : GenericMatchers;

export type SettledAssertions<T = unknown> = Promisified<MatchersFor<T>> & {
  not: Promisified<MatchersFor<T>>;
};

export type ValueAssertions<T = unknown> = GenericMatchers & {
  not: GenericMatchers;
  resolves: SettledAssertions<Awaited<T>>;
  rejects: SettledAssertions;
};

/// Playwright: `expect(actual, messageOrOptions?: string | { message?: string })`.
export type ExpectMessage = string | { message?: string };

export interface PollAssertions {
  toBe(expected: unknown): Promise<void>;
  toEqual(expected: unknown): Promise<void>;
  toSatisfy(predicate: (value: unknown) => boolean): Promise<void>;
  not: PollAssertions;
}

/// The `this` a custom matcher body reads.
export interface MatcherState {
  readonly isNot: boolean;
  readonly isSoft: boolean;
  readonly promise: '' | 'resolves' | 'rejects';
  readonly timeout: number;
  readonly utils: {
    printReceived(value: unknown): string;
    printExpected(value: unknown): string;
    stringify(value: unknown): string;
    matcherHint(name: string, ...rest: unknown[]): string;
  };
  /// Present for jest compatibility; calling it throws, as upstream.
  equals(...args: unknown[]): boolean;
}

export interface MatcherReturn {
  pass: boolean;
  message?: string | (() => string);
  expected?: unknown;
  actual?: unknown;
  log?: string[];
}

export type MatcherFunction = (
  this: MatcherState,
  received: any,
  ...args: any[]
) => MatcherReturn | Promise<MatcherReturn>;

/// A registered matcher as it is called: the receiver is bound, and an
/// async body makes the call awaitable.
export type ToUserMatchers<E> = {
  [K in keyof E]: E[K] extends (this: any, received: any, ...args: infer A) => infer R
    ? (...args: A) => R extends Promise<unknown> ? Promise<void> : void
    : never;
};

type MergedMatchers<List> = List extends [ExpectBase<infer E>, ...infer Rest] ? E & MergedMatchers<Rest> : {};

/// Every registered matcher is published as an asymmetric one too, so it
/// can stand in for a value inside `toEqual` / `toMatchObject`.
export type ToUserAsymmetric<E> = {
  [K in keyof E]: E[K] extends (this: any, received: any, ...args: infer A) => unknown ? (...args: A) => unknown : never;
};

/// Custom matchers reach `.`, `.not`, `.resolves` and `.rejects`, the
/// same four places a built-in does.
type UserChain<E> = ToUserMatchers<E> & { not: ToUserMatchers<E> };
type SettledUser<E> = Promisified<ToUserMatchers<E>> & { not: Promisified<ToUserMatchers<E>> };

export interface ExpectBase<E = {}> {
  (locator: Locator, messageOrOptions?: ExpectMessage): LocatorAssertions & UserChain<E>;
  (page: Page, messageOrOptions?: ExpectMessage): PageAssertions & UserChain<E>;
  (response: APIResponse, messageOrOptions?: ExpectMessage): APIResponseAssertions & UserChain<E>;
  (fn: () => unknown | Promise<unknown>, messageOrOptions?: ExpectMessage): ValueAssertions &
    ToUserMatchers<E> & {
      toPass(options?: { timeout?: number; intervals?: number[] }): Promise<void>;
      not: GenericMatchers &
        ToUserMatchers<E> & { toPass(options?: { timeout?: number; intervals?: number[] }): Promise<void> };
    };
  <T>(value: T, messageOrOptions?: ExpectMessage): ValueAssertions<T> &
    UserChain<E> & {
      resolves: SettledAssertions<Awaited<T>> & SettledUser<E>;
      rejects: SettledAssertions & SettledUser<E>;
    };

  /// Register custom matchers. The result carries them; a name that is
  /// not a built-in also becomes available on THIS expect, so
  /// `expect.extend({...})` works without capturing the return value.
  extend<M extends Record<string, MatcherFunction>>(matchers: M): Expect<E & M>;
  /// A new expect with these defaults — the receiver is unchanged.
  configure(configuration: { message?: string; timeout?: number; soft?: boolean }): Expect<E>;
  /// Jest compatibility; answers an empty object.
  getState(): Record<string, unknown>;
  /// The soft expect: failures are recorded rather than thrown.
  readonly soft: Expect<E>;
  poll(
    fn: () => unknown | Promise<unknown>,
    messageOrOptions?: string | { message?: string; timeout?: number; intervals?: number[] },
  ): PollAssertions;
  any(ctor: Function): unknown;
  anything(): unknown;
  arrayContaining(items: unknown[]): unknown;
  objectContaining(subset: object): unknown;
  stringContaining(substring: string): unknown;
  stringMatching(pattern: string | RegExp): unknown;
  closeTo(value: number, numDigits?: number): unknown;
  arrayOf(sample: unknown): unknown;
  not: {
    any(ctor: Function): unknown;
    anything(): unknown;
    arrayContaining(items: unknown[]): unknown;
    objectContaining(subset: object): unknown;
    stringContaining(substring: string): unknown;
    stringMatching(pattern: string | RegExp): unknown;
    closeTo(value: number, numDigits?: number): unknown;
    arrayOf(sample: unknown): unknown;
  } & ToUserAsymmetric<E>;
}

/// `expect` itself: the assertion factory plus every asymmetric matcher,
/// including the ones `extend` registered.
export type Expect<E = {}> = ExpectBase<E> & ToUserAsymmetric<E>;

export const expect: Expect;

/// One expect exposing every custom matcher of the expects passed in.
export function mergeExpects<List extends unknown[]>(...expects: List): Expect<MergedMatchers<List>>;

// ── Browser surface (QuickJS bindings over the Rust core) ────────────

export interface GotoOptions {
  timeout?: number;
  waitUntil?: 'load' | 'domcontentloaded' | 'networkidle' | 'commit';
  referer?: string;
}

export interface ClickOptions {
  button?: 'left' | 'right' | 'middle';
  clickCount?: number;
  delay?: number;
  position?: { x: number; y: number };
  modifiers?: Array<'Alt' | 'Control' | 'ControlOrMeta' | 'Meta' | 'Shift'>;
  force?: boolean;
  noWaitAfter?: boolean;
  trial?: boolean;
  timeout?: number;
}

export interface FillOptions {
  force?: boolean;
  noWaitAfter?: boolean;
  timeout?: number;
}

export interface TimeoutOption {
  timeout?: number;
}

export interface WaitForSelectorOptions extends TimeoutOption {
  state?: 'attached' | 'detached' | 'visible' | 'hidden';
}

export interface ScreenshotOptions extends TimeoutOption {
  type?: 'png' | 'jpeg';
  quality?: number;
  fullPage?: boolean;
  clip?: { x: number; y: number; width: number; height: number };
  omitBackground?: boolean;
  path?: string;
  scale?: 'css' | 'device';
  mask?: Locator[];
  maskColor?: string;
}

export interface GetByRoleOptions {
  checked?: boolean;
  description?: string | RegExp;
  disabled?: boolean;
  exact?: boolean;
  expanded?: boolean;
  includeHidden?: boolean;
  level?: number;
  name?: string | RegExp;
  pressed?: boolean;
  selected?: boolean;
}

export interface GetByTextOptions {
  exact?: boolean;
}

export interface LocatorFilterOptions {
  has?: Locator;
  hasNot?: Locator;
  hasText?: string | RegExp;
  hasNotText?: string | RegExp;
  visible?: boolean;
}

export interface SelectOptionValues {
  value?: string;
  label?: string;
  index?: number;
}

export interface Keyboard {
  press(key: string, options?: { delay?: number }): Promise<void>;
  down(key: string): Promise<void>;
  up(key: string): Promise<void>;
  // namedKeys is a ferridriver extension: `{Enter}` presses the named
  // key, `{{` escapes a literal brace (default types braces verbatim).
  type(text: string, options?: { delay?: number; namedKeys?: boolean }): Promise<void>;
  insertText(text: string): Promise<void>;
}

export interface Mouse {
  move(x: number, y: number, options?: { steps?: number }): Promise<void>;
  click(x: number, y: number, options?: ClickOptions): Promise<void>;
  dblclick(x: number, y: number, options?: ClickOptions): Promise<void>;
  down(options?: { button?: 'left' | 'right' | 'middle'; clickCount?: number }): Promise<void>;
  up(options?: { button?: 'left' | 'right' | 'middle'; clickCount?: number }): Promise<void>;
  wheel(deltaX: number, deltaY: number): Promise<void>;
}

export interface JSHandle {
  jsonValue(): Promise<unknown>;
  evaluate(pageFunction: Function | string, arg?: unknown): Promise<unknown>;
  evaluateHandle(pageFunction: Function | string, arg?: unknown): Promise<JSHandle>;
  getProperty(propertyName: string): Promise<JSHandle>;
  // Plain object rather than Playwright's Map<string, JSHandle> — the
  // documented ferridriver shape shared with the NAPI binding's
  // Record<string, JSHandle>.
  getProperties(): Promise<Record<string, JSHandle>>;
  asElement(): ElementHandle | null;
  isDisposed(): boolean;
  dispose(): Promise<void>;
}

export interface ElementHandle extends JSHandle {
  asJSHandle(): JSHandle;
  click(options?: ClickOptions): Promise<void>;
  dblclick(options?: ClickOptions): Promise<void>;
  hover(options?: ClickOptions): Promise<void>;
  tap(options?: ClickOptions): Promise<void>;
  fill(value: string, options?: FillOptions): Promise<void>;
  press(key: string, options?: TimeoutOption & { delay?: number }): Promise<void>;
  type(text: string, options?: TimeoutOption & { delay?: number }): Promise<void>;
  check(options?: ClickOptions): Promise<void>;
  uncheck(options?: ClickOptions): Promise<void>;
  setChecked(checked: boolean, options?: ClickOptions): Promise<void>;
  focus(): Promise<void>;
  scrollIntoViewIfNeeded(): Promise<void>;
  dispatchEvent(type: string, eventInit?: Record<string, unknown>): Promise<void>;
  selectOption(
    values: string | string[] | SelectOptionValues | SelectOptionValues[] | null,
    options?: TimeoutOption & { force?: boolean }
  ): Promise<string[]>;
  selectText(): Promise<void>;
  setInputFiles(
    files: string | string[] | { name: string; mimeType: string; buffer: Uint8Array | Buffer } | Array<{ name: string; mimeType: string; buffer: Uint8Array | Buffer }>,
    options?: TimeoutOption
  ): Promise<void>;
  textContent(): Promise<string | null>;
  innerText(): Promise<string>;
  innerHTML(): Promise<string>;
  inputValue(): Promise<string>;
  getAttribute(name: string): Promise<string | null>;
  isVisible(): Promise<boolean>;
  isHidden(): Promise<boolean>;
  isEnabled(): Promise<boolean>;
  isDisabled(): Promise<boolean>;
  isChecked(): Promise<boolean>;
  isEditable(): Promise<boolean>;
  boundingBox(): Promise<{ x: number; y: number; width: number; height: number } | null>;
  screenshot(options?: ScreenshotOptions): Promise<Uint8Array>;
  $(selector: string): Promise<ElementHandle | null>;
  $$(selector: string): Promise<ElementHandle[]>;
  $eval(selector: string, pageFunction: Function | string, arg?: unknown): Promise<unknown>;
  $$eval(selector: string, pageFunction: Function | string, arg?: unknown): Promise<unknown>;
  ownerFrame(): Promise<Frame | null>;
  contentFrame(): Promise<Frame | null>;
  waitForElementState(
    state: 'visible' | 'hidden' | 'stable' | 'enabled' | 'disabled' | 'editable',
    options?: TimeoutOption
  ): Promise<void>;
  waitForSelector(selector: string, options?: TimeoutOption): Promise<ElementHandle | null>;
}

export interface Locator {
  click(options?: ClickOptions): Promise<void>;
  dblclick(options?: ClickOptions): Promise<void>;
  tap(options?: ClickOptions): Promise<void>;
  fill(value: string, options?: FillOptions): Promise<void>;
  clear(options?: FillOptions): Promise<void>;
  press(key: string, options?: TimeoutOption & { delay?: number; noWaitAfter?: boolean }): Promise<void>;
  pressSequentially(text: string, options?: TimeoutOption & { delay?: number }): Promise<void>;
  type(text: string, options?: TimeoutOption & { delay?: number }): Promise<void>;
  dispatchEvent(type: string, eventInit?: Record<string, unknown>, options?: TimeoutOption): Promise<void>;
  check(options?: ClickOptions): Promise<void>;
  uncheck(options?: ClickOptions): Promise<void>;
  setChecked(checked: boolean, options?: ClickOptions): Promise<void>;
  hover(options?: ClickOptions): Promise<void>;
  focus(options?: TimeoutOption): Promise<void>;
  blur(options?: TimeoutOption): Promise<void>;
  selectOption(
    values: string | string[] | SelectOptionValues | SelectOptionValues[] | null,
    options?: TimeoutOption & { force?: boolean }
  ): Promise<string[]>;
  selectText(options?: TimeoutOption & { force?: boolean }): Promise<void>;
  setInputFiles(
    files: string | string[] | { name: string; mimeType: string; buffer: Uint8Array | Buffer } | Array<{ name: string; mimeType: string; buffer: Uint8Array | Buffer }>,
    options?: TimeoutOption
  ): Promise<void>;
  // steps is a ferridriver extension (default 5): intermediate
  // mousemoves so mousemove-tracked drag libraries cross their drag
  // threshold; Playwright emits a single move.
  dragTo(target: Locator, options?: TimeoutOption & { force?: boolean; sourcePosition?: { x: number; y: number }; targetPosition?: { x: number; y: number }; steps?: number; trial?: boolean }): Promise<void>;
  // ferridriver extension: drop a DataTransfer payload (in-memory files
  // and/or string data by MIME type) onto this element.
  drop(
    payload: {
      files?: { name: string; mimeType: string; buffer: Uint8Array | Buffer } | Array<{ name: string; mimeType: string; buffer: Uint8Array | Buffer }>;
      data?: Record<string, string>;
    },
    options?: TimeoutOption & { modifiers?: Array<'Alt' | 'Control' | 'ControlOrMeta' | 'Meta' | 'Shift'>; position?: { x: number; y: number } }
  ): Promise<void>;
  scrollIntoViewIfNeeded(options?: TimeoutOption): Promise<void>;

  textContent(options?: TimeoutOption): Promise<string | null>;
  innerText(options?: TimeoutOption): Promise<string>;
  innerHTML(options?: TimeoutOption): Promise<string>;
  inputValue(options?: TimeoutOption): Promise<string>;
  getAttribute(name: string, options?: TimeoutOption): Promise<string | null>;
  allTextContents(): Promise<string[]>;
  allInnerTexts(): Promise<string[]>;
  count(): Promise<number>;
  all(): Promise<Locator[]>;
  boundingBox(options?: TimeoutOption): Promise<{ x: number; y: number; width: number; height: number } | null>;
  screenshot(options?: ScreenshotOptions): Promise<Uint8Array>;
  ariaSnapshot(options?: TimeoutOption & { boxes?: boolean }): Promise<string>;

  isVisible(options?: TimeoutOption): Promise<boolean>;
  isHidden(options?: TimeoutOption): Promise<boolean>;
  isEnabled(options?: TimeoutOption): Promise<boolean>;
  isDisabled(options?: TimeoutOption): Promise<boolean>;
  isChecked(options?: TimeoutOption): Promise<boolean>;
  isEditable(options?: TimeoutOption): Promise<boolean>;
  isAttached(options?: TimeoutOption): Promise<boolean>;

  evaluate(pageFunction: Function | string, arg?: unknown, options?: TimeoutOption): Promise<unknown>;
  evaluateAll(pageFunction: Function | string, arg?: unknown): Promise<unknown>;
  evaluateHandle(pageFunction: Function | string, arg?: unknown): Promise<JSHandle>;
  elementHandle(options?: TimeoutOption): Promise<ElementHandle>;
  elementHandles(): Promise<ElementHandle[]>;
  waitFor(options?: WaitForSelectorOptions): Promise<void>;
  waitForFunction(pageFunction: Function | string, arg?: unknown, options?: TimeoutOption & { polling?: number | 'raf' }): Promise<JSHandle>;
  // highlight returns a Disposable that removes the overlay; hideHighlight
  // clears any overlay (ferridriver extensions over Playwright's void
  // highlight, which it documents as debug-only).
  highlight(options?: { style?: { outlineColor?: string; zIndex?: number } }): Promise<Disposable>;
  hideHighlight(): Promise<void>;
  describe(description: string): Locator;
  // Accessible-description surface (Playwright 1.58) plus ferridriver
  // extensions mirrored from the NAPI binding: the canonicalizing
  // normalize(), the selector/isStrict introspection getters,
  // setStrict(), and rightClick() (= click({ button: 'right' })).
  description(): string | null;
  normalize(): Promise<Locator>;
  readonly selector: string;
  readonly isStrict: boolean;
  setStrict(strict: boolean): Locator;
  rightClick(options?: ClickOptions): Promise<void>;

  locator(selectorOrLocator: string | Locator, options?: LocatorFilterOptions): Locator;
  filter(options?: LocatorFilterOptions): Locator;
  and(locator: Locator): Locator;
  or(locator: Locator): Locator;
  first(): Locator;
  last(): Locator;
  nth(index: number): Locator;
  getByRole(role: string, options?: GetByRoleOptions): Locator;
  getByText(text: string | RegExp, options?: GetByTextOptions): Locator;
  getByLabel(text: string | RegExp, options?: GetByTextOptions): Locator;
  getByPlaceholder(text: string | RegExp, options?: GetByTextOptions): Locator;
  getByAltText(text: string | RegExp, options?: GetByTextOptions): Locator;
  getByTitle(text: string | RegExp, options?: GetByTextOptions): Locator;
  getByTestId(testId: string | RegExp): Locator;
  frameLocator(selector: string): FrameLocator;
  contentFrame(): FrameLocator;
  page(): Page;
}

export interface FrameLocator {
  locator(selectorOrLocator: string | Locator, options?: LocatorFilterOptions): Locator;
  getByRole(role: string, options?: GetByRoleOptions): Locator;
  getByText(text: string | RegExp, options?: GetByTextOptions): Locator;
  getByLabel(text: string | RegExp, options?: GetByTextOptions): Locator;
  getByPlaceholder(text: string | RegExp, options?: GetByTextOptions): Locator;
  getByAltText(text: string | RegExp, options?: GetByTextOptions): Locator;
  getByTitle(text: string | RegExp, options?: GetByTextOptions): Locator;
  getByTestId(testId: string | RegExp): Locator;
  frameLocator(selector: string): FrameLocator;
  first(): FrameLocator;
  last(): FrameLocator;
  nth(index: number): FrameLocator;
  owner(): Locator;
}

export interface Route {
  request(): Request;
  fulfill(options?: {
    status?: number;
    headers?: Record<string, string>;
    contentType?: string;
    body?: string | Uint8Array | Buffer;
    json?: unknown;
    path?: string;
    response?: APIResponse;
  }): Promise<void>;
  continue(options?: {
    url?: string;
    method?: string;
    headers?: Record<string, string>;
    postData?: string | Uint8Array | Buffer;
  }): Promise<void>;
  fallback(options?: {
    url?: string;
    method?: string;
    headers?: Record<string, string>;
    postData?: string | Uint8Array | Buffer;
  }): Promise<void>;
  abort(errorCode?: string): Promise<void>;
  fetch(options?: {
    url?: string;
    method?: string;
    headers?: Record<string, string>;
    postData?: string | Uint8Array | Buffer;
    maxRedirects?: number;
  }): Promise<APIResponse>;
}

export interface Request {
  url(): string;
  method(): string;
  headers(): Record<string, string>;
  allHeaders(): Promise<Record<string, string>>;
  headersArray(): Promise<Array<{ name: string; value: string }>>;
  postData(): string | null;
  postDataJSON(): unknown;
  postDataBuffer(): Uint8Array | null;
  headerValue(name: string): Promise<string | null>;
  resourceType(): string;
  isNavigationRequest(): boolean;
  redirectedFrom(): Request | null;
  redirectedTo(): Request | null;
  frame(): Frame;
  response(): Promise<Response | null>;
  existingResponse(): Promise<Response | null>;
  failure(): { errorText: string } | null;
  timing(): Record<string, number>;
}

export interface Response {
  url(): string;
  status(): number;
  statusText(): string;
  ok(): boolean;
  headers(): Record<string, string>;
  allHeaders(): Promise<Record<string, string>>;
  headersArray(): Promise<Array<{ name: string; value: string }>>;
  headerValue(name: string): Promise<string | null>;
  headerValues(name: string): Promise<string[]>;
  httpVersion(): Promise<string>;
  body(): Promise<Uint8Array>;
  text(): Promise<string>;
  json(): Promise<unknown>;
  request(): Request;
  frame(): Frame;
  finished(): Promise<null>;
}

export interface Frame {
  name(): string;
  url(): string;
  isMainFrame(): boolean;
  isDetached(): boolean;
  waitForSelector(selector: string, options?: WaitForSelectorOptions): Promise<ElementHandle | null>;
  goto(url: string, options?: GotoOptions): Promise<Response | null>;
  content(): Promise<string>;
  title(): Promise<string>;
  locator(selectorOrLocator: string | Locator, options?: LocatorFilterOptions): Locator;
  getByRole(role: string, options?: GetByRoleOptions): Locator;
  getByText(text: string | RegExp, options?: GetByTextOptions): Locator;
  getByLabel(text: string | RegExp, options?: GetByTextOptions): Locator;
  getByPlaceholder(text: string | RegExp, options?: GetByTextOptions): Locator;
  getByAltText(text: string | RegExp, options?: GetByTextOptions): Locator;
  getByTitle(text: string | RegExp, options?: GetByTextOptions): Locator;
  getByTestId(testId: string | RegExp): Locator;
  frameLocator(selector: string): FrameLocator;
  page(): Page;
  evaluate(pageFunction: Function | string, arg?: unknown): Promise<unknown>;
  parentFrame(): Frame | null;
  childFrames(): Frame[];
}

export interface WebSocket {
  url(): string;
  isClosed(): boolean;
  // Accepts a bare timeout number (ferridriver extension mirroring
  // page.waitForEvent) or Playwright's { timeout } bag.
  waitForEvent(
    event: 'framesent' | 'framereceived' | 'socketerror' | 'close',
    optionsOrTimeout?: number | { timeout?: number },
  ): Promise<{ event: string; payload: string | null; error: string | null }>;
}

export interface WebSocketRoute {
  url(): string;
  send(message: string | Uint8Array | Buffer): void;
  close(options?: { code?: number; reason?: string }): void;
  onMessage(handler: (message: string | Uint8Array) => void): void;
  onClose(handler: (code?: number, reason?: string) => void): void;
  connectToServer(): WebSocketRoute;
}

export interface WebError {
  error(): Error;
  page(): Page | null;
  location(): { url: string; line: number; column: number };
}

export interface ConsoleMessage {
  type(): string;
  text(): string;
  args(): JSHandle[];
  location(): { url: string; lineNumber: number; columnNumber: number };
}

export interface Dialog {
  type(): string;
  message(): string;
  defaultValue(): string;
  accept(promptText?: string): Promise<void>;
  dismiss(): Promise<void>;
  page(): Page | null;
}

export interface Download {
  url(): string;
  suggestedFilename(): string;
  path(): Promise<string>;
  saveAs(path: string): Promise<void>;
  failure(): Promise<string | null>;
  cancel(): Promise<void>;
  delete(): Promise<void>;
  page(): Page | null;
}

export interface FileChooser {
  element(): ElementHandle;
  isMultiple(): boolean;
  page(): Page;
  setFiles(
    files: string | string[] | { name: string; mimeType: string; buffer: Uint8Array | Buffer } | Array<{ name: string; mimeType: string; buffer: Uint8Array | Buffer }>,
    options?: TimeoutOption
  ): Promise<void>;
}

export interface Clock {
  install(options?: { time?: number | string | Date }): Promise<void>;
  setFixedTime(time: number | string | Date): Promise<void>;
  setSystemTime(time: number | string | Date): Promise<void>;
  fastForward(ticks: number | string): Promise<void>;
  runFor(ticks: number | string): Promise<void>;
  pauseAt(time: number | string | Date): Promise<void>;
  resume(): Promise<void>;
}

export interface CDPSession {
  send(method: string, params?: Record<string, unknown>): Promise<unknown>;
  on(event: string, handler: (params: unknown) => void): void;
  detach(): Promise<void>;
}

export type PageEvent =
  | 'close'
  | 'console'
  | 'crash'
  | 'dialog'
  | 'domcontentloaded'
  | 'download'
  | 'filechooser'
  | 'frameattached'
  | 'framedetached'
  | 'framenavigated'
  | 'load'
  | 'pageerror'
  | 'popup'
  | 'request'
  | 'requestfailed'
  | 'requestfinished'
  | 'response'
  | 'websocket'
  | 'worker';

export interface Page {
  goto(url: string, options?: GotoOptions): Promise<Response | null>;
  goBack(options?: GotoOptions): Promise<Response | null>;
  goForward(options?: GotoOptions): Promise<Response | null>;
  reload(options?: GotoOptions): Promise<Response | null>;
  url(): string;
  title(): Promise<string>;
  content(): Promise<string>;
  setContent(html: string, options?: GotoOptions): Promise<void>;
  close(options?: { runBeforeUnload?: boolean }): Promise<void>;
  isClosed(): boolean;
  bringToFront(): Promise<void>;

  locator(selectorOrLocator: string | Locator, options?: LocatorFilterOptions): Locator;
  getByRole(role: string, options?: GetByRoleOptions): Locator;
  getByText(text: string | RegExp, options?: GetByTextOptions): Locator;
  getByLabel(text: string | RegExp, options?: GetByTextOptions): Locator;
  getByPlaceholder(text: string | RegExp, options?: GetByTextOptions): Locator;
  getByAltText(text: string | RegExp, options?: GetByTextOptions): Locator;
  getByTitle(text: string | RegExp, options?: GetByTextOptions): Locator;
  getByTestId(testId: string | RegExp): Locator;
  frameLocator(selector: string): FrameLocator;
  frames(): Frame[];
  mainFrame(): Frame;
  frame(nameOrOptions: string | { name?: string; url?: string | RegExp }): Frame | null;

  addLocatorHandler(
    locator: Locator,
    handler: (locator: Locator) => Promise<unknown> | unknown,
    options?: { noWaitAfter?: boolean; times?: number },
  ): void;
  removeLocatorHandler(locator: Locator): void;

  click(selector: string, options?: ClickOptions): Promise<void>;
  dblclick(selector: string, options?: ClickOptions): Promise<void>;
  fill(selector: string, value: string, options?: FillOptions): Promise<void>;
  press(selector: string, key: string, options?: TimeoutOption & { delay?: number }): Promise<void>;
  type(selector: string, text: string, options?: TimeoutOption & { delay?: number }): Promise<void>;
  check(selector: string, options?: ClickOptions): Promise<void>;
  uncheck(selector: string, options?: ClickOptions): Promise<void>;
  hover(selector: string, options?: ClickOptions): Promise<void>;
  focus(selector: string, options?: TimeoutOption): Promise<void>;
  selectOption(
    selector: string,
    values: string | string[] | SelectOptionValues | SelectOptionValues[] | null,
    options?: TimeoutOption & { force?: boolean }
  ): Promise<string[]>;
  setInputFiles(
    selector: string,
    files: string | string[] | { name: string; mimeType: string; buffer: Uint8Array | Buffer } | Array<{ name: string; mimeType: string; buffer: Uint8Array | Buffer }>,
    options?: TimeoutOption
  ): Promise<void>;
  dragAndDrop(source: string, target: string, options?: TimeoutOption & { force?: boolean; sourcePosition?: { x: number; y: number }; targetPosition?: { x: number; y: number }; steps?: number; trial?: boolean }): Promise<void>;
  tap(selector: string, options?: ClickOptions): Promise<void>;
  // ferridriver extensions (NOT Playwright): coordinate click and an
  // interpolated mouse move (Playwright equivalent: mouse.move with
  // steps).
  clickAt(x: number, y: number): Promise<void>;
  moveMouseSmooth(fromX: number, fromY: number, toX: number, toY: number, steps: number): Promise<void>;

  textContent(selector: string, options?: TimeoutOption): Promise<string | null>;
  innerText(selector: string, options?: TimeoutOption): Promise<string>;
  innerHTML(selector: string, options?: TimeoutOption): Promise<string>;
  inputValue(selector: string, options?: TimeoutOption): Promise<string>;
  getAttribute(selector: string, name: string, options?: TimeoutOption): Promise<string | null>;
  isVisible(selector: string, options?: TimeoutOption): Promise<boolean>;
  isHidden(selector: string, options?: TimeoutOption): Promise<boolean>;
  isEnabled(selector: string, options?: TimeoutOption): Promise<boolean>;
  isDisabled(selector: string, options?: TimeoutOption): Promise<boolean>;
  isChecked(selector: string, options?: TimeoutOption): Promise<boolean>;
  isEditable(selector: string, options?: TimeoutOption): Promise<boolean>;

  evaluate(pageFunction: Function | string, arg?: unknown): Promise<unknown>;
  evaluateHandle(pageFunction: Function | string, arg?: unknown): Promise<JSHandle>;
  pause(): Promise<void>;
  requestGC(): Promise<void>;
  snapshotForAI(options?: { depth?: number; track?: string }): Promise<{ full: string; incremental?: string; refMap: Record<string, number> }>;
  consoleMessages(options?: { filter?: 'all' | 'since-navigation' }): ConsoleMessage[];
  clearConsoleMessages(): void;
  pageErrors(options?: { filter?: 'all' | 'since-navigation' }): Error[];
  clearPageErrors(): void;
  $(selector: string): Promise<ElementHandle | null>;
  $$(selector: string): Promise<ElementHandle[]>;
  // querySelector/querySelectorAll are the NAPI-mirrored long names for
  // $/$$ (ferridriver extension).
  querySelector(selector: string): Promise<ElementHandle | null>;
  querySelectorAll(selector: string): Promise<ElementHandle[]>;
  $eval(selector: string, pageFunction: Function | string, arg?: unknown): Promise<unknown>;
  $$eval(selector: string, pageFunction: Function | string, arg?: unknown): Promise<unknown>;
  addInitScript(script: Function | string | { content?: string; path?: string }, arg?: unknown): Promise<void>;
  addScriptTag(options?: { url?: string; path?: string; content?: string; type?: string }): Promise<ElementHandle>;
  addStyleTag(options?: { url?: string; path?: string; content?: string }): Promise<ElementHandle>;
  exposeFunction(name: string, callback: Function): Promise<void>;
  exposeBinding(name: string, callback: Function, options?: { handle?: boolean }): Promise<void>;

  waitForSelector(selector: string, options?: WaitForSelectorOptions): Promise<ElementHandle | null>;
  waitForFunction(pageFunction: Function | string, arg?: unknown, options?: TimeoutOption & { polling?: number | 'raf' }): Promise<JSHandle>;
  waitForLoadState(state?: 'load' | 'domcontentloaded' | 'networkidle', options?: TimeoutOption): Promise<void>;
  waitForURL(url: string | RegExp | ((url: URL) => boolean), options?: TimeoutOption & { waitUntil?: 'load' | 'domcontentloaded' | 'networkidle' | 'commit' }): Promise<void>;
  waitForTimeout(timeout: number): Promise<void>;
  waitForEvent(event: PageEvent, optionsOrPredicate?: number | ((event: unknown) => boolean) | { predicate?: (event: unknown) => boolean; timeout?: number }): Promise<unknown>;
  waitForRequest(urlOrPredicate: string | RegExp | ((request: Request) => boolean), options?: TimeoutOption): Promise<Request>;
  waitForResponse(urlOrPredicate: string | RegExp | ((response: Response) => boolean), options?: TimeoutOption): Promise<Response>;

  // Page-level route returns a Disposable that reverses the
  // registration (a ferridriver extension mirroring Playwright's
  // internal DisposableStub at client/page.ts:535).
  route(url: string | RegExp | ((url: URL) => boolean), handler: (route: Route, request: Request) => void | Promise<void>, options?: { times?: number }): Promise<Disposable>;
  routeFromHAR(har: string, options?: { notFound?: 'abort' | 'fallback'; url?: string | RegExp; update?: boolean; updateContent?: 'embed' | 'attach' }): Promise<void>;
  unroute(url: string | RegExp | ((url: URL) => boolean), handler?: (route: Route, request: Request) => void | Promise<void>): Promise<void>;
  unrouteAll(options?: { behavior?: 'wait' | 'ignoreErrors' | 'default' }): Promise<void>;
  routeWebSocket(url: string | RegExp | ((url: URL) => boolean), handler: (ws: WebSocketRoute) => void | Promise<void>): Promise<void>;

  screenshot(options?: ScreenshotOptions): Promise<Uint8Array>;
  pdf(options?: Record<string, unknown>): Promise<Uint8Array>;

  setViewportSize(size: { width: number; height: number }): Promise<void>;
  viewportSize(): { width: number; height: number } | null;
  setExtraHTTPHeaders(headers: Record<string, string>): Promise<void>;
  emulateMedia(options?: { media?: 'screen' | 'print' | null; colorScheme?: 'light' | 'dark' | 'no-preference' | null; reducedMotion?: 'reduce' | 'no-preference' | null; forcedColors?: 'active' | 'none' | null; contrast?: 'more' | 'less' | 'no-preference' | null }): Promise<void>;
  setDefaultTimeout(timeout: number): void;
  setDefaultNavigationTimeout(timeout: number): void;

  // The Node emitter surface Playwright exposes: registrations chain,
  // removal is by function identity.
  on(event: PageEvent, handler: (arg: unknown) => void): this;
  addListener(event: PageEvent, handler: (arg: unknown) => void): this;
  once(event: PageEvent, handler: (arg: unknown) => void): this;
  prependListener(event: PageEvent, handler: (arg: unknown) => void): this;
  prependOnceListener(event: PageEvent, handler: (arg: unknown) => void): this;
  off(event: PageEvent, handler?: (arg: unknown) => void): this;
  removeListener(event: PageEvent, handler?: (arg: unknown) => void): this;
  removeAllListeners(event?: PageEvent): this;
  removeAllListeners(event: PageEvent | undefined, options: { behavior?: 'wait' | 'ignoreErrors' | 'default' }): Promise<void>;
  listeners(event: PageEvent): ((arg: unknown) => void)[];
  rawListeners(event: PageEvent): ((arg: unknown) => void)[];
  listenerCount(event: PageEvent): number;
  eventNames(): PageEvent[];
  setMaxListeners(max: number): this;
  getMaxListeners(): number;

  keyboard: Keyboard;
  mouse: Mouse;
  touchscreen: Touchscreen;
  clock: Clock;
  // Context-bound API request client (page.request IS context.request):
  // shares the browser context's cookies in both directions.
  request: APIRequestContext;
  context(): BrowserContext;
  opener(): Promise<Page | null>;
  video(): Video | null;
  // ferridriver extensions: driver-side WebStorage accessors and a
  // page-to-markdown snapshot.
  readonly localStorage: WebStorage;
  readonly sessionStorage: WebStorage;
  markdown(): Promise<string>;
}

export interface Touchscreen {
  tap(x: number, y: number): Promise<void>;
}

export interface Disposable {
  dispose(): Promise<void>;
}

export interface Cookie {
  name: string;
  value: string;
  domain?: string;
  path?: string;
  url?: string;
  expires?: number;
  httpOnly?: boolean;
  secure?: boolean;
  sameSite?: 'Strict' | 'Lax' | 'None';
}

export interface StorageState {
  cookies: Cookie[];
  origins: Array<{
    origin: string;
    localStorage: Array<{ name: string; value: string }>;
  }>;
}

export interface BrowserContext {
  newPage(): Promise<Page>;
  // Playwright's pages() is synchronous; ferridriver resolves the list
  // from the Rust browser state, so it is a promise here.
  pages(): Promise<Page[]>;
  close(): Promise<void>;
  cookies(urls?: string | string[]): Promise<Cookie[]>;
  addCookies(cookies: Cookie[]): Promise<void>;
  clearCookies(options?: { name?: string | RegExp; domain?: string | RegExp; path?: string | RegExp }): Promise<void>;
  // ferridriver extension mirroring the NAPI binding.
  deleteCookie(name: string, domain?: string): Promise<void>;
  setStorageState(state: StorageState): Promise<void>;
  setHTTPCredentials(credentials: { username: string; password: string; origin?: string; send?: 'always' | 'unauthorized' } | null): Promise<void>;
  isClosed(): Promise<boolean>;
  grantPermissions(permissions: string[], options?: { origin?: string }): Promise<void>;
  clearPermissions(): Promise<void>;
  setGeolocation(geolocation: { latitude: number; longitude: number; accuracy?: number } | null): Promise<void>;
  setOffline(offline: boolean): Promise<void>;
  setExtraHTTPHeaders(headers: Record<string, string>): Promise<void>;
  storageState(options?: { path?: string }): Promise<StorageState>;
  addInitScript(script: Function | string | { content?: string; path?: string }, arg?: unknown): Promise<void>;
  // Context-level expose returns a Disposable that unregisters the
  // binding and removes the page-side window proxy (a ferridriver
  // extension over Playwright's void return).
  exposeFunction(name: string, callback: Function): Promise<Disposable>;
  exposeBinding(name: string, callback: Function, options?: { handle?: boolean }): Promise<Disposable>;
  route(url: string | RegExp | ((url: URL) => boolean), handler: (route: Route, request: Request) => void | Promise<void>, options?: { times?: number }): Promise<void>;
  routeWebSocket(url: string | RegExp | ((url: URL) => boolean), handler: (ws: WebSocketRoute) => void | Promise<void>): Promise<void>;
  unroute(url: string | RegExp | ((url: URL) => boolean), handler?: (route: Route, request: Request) => void | Promise<void>): Promise<void>;
  unrouteAll(options?: { behavior?: 'wait' | 'ignoreErrors' | 'default' }): Promise<void>;
  waitForEvent(event: string, optionsOrPredicate?: number | ((event: unknown) => boolean) | { predicate?: (event: unknown) => boolean; timeout?: number }): Promise<unknown>;
  on(event: string, handler: (arg: unknown) => void): this;
  addListener(event: string, handler: (arg: unknown) => void): this;
  once(event: string, handler: (arg: unknown) => void): this;
  prependListener(event: string, handler: (arg: unknown) => void): this;
  prependOnceListener(event: string, handler: (arg: unknown) => void): this;
  off(event: string, handler?: (arg: unknown) => void): this;
  removeListener(event: string, handler?: (arg: unknown) => void): this;
  removeAllListeners(event?: string): this;
  removeAllListeners(event: string | undefined, options: { behavior?: 'wait' | 'ignoreErrors' | 'default' }): Promise<void>;
  listeners(event: string): ((arg: unknown) => void)[];
  rawListeners(event: string): ((arg: unknown) => void)[];
  listenerCount(event: string): number;
  eventNames(): string[];
  setMaxListeners(max: number): this;
  getMaxListeners(): number;
  setDefaultTimeout(timeout: number): void;
  setDefaultNavigationTimeout(timeout: number): void;
  newCDPSession(page: Page): Promise<CDPSession>;
  browser(): Browser | null;
  clock: Clock;
  tracing: Tracing;
  // Context-bound API request client sharing this context's cookies in
  // both directions.
  request: APIRequestContext;
  routeFromHAR(har: string, options?: { notFound?: 'abort' | 'fallback'; url?: string | RegExp; update?: boolean; updateContent?: 'embed' | 'attach' }): Promise<void>;
  // ferridriver extension: arm video recording for pages opened after
  // this call (Playwright takes recordVideo as a context-creation
  // option only).
  setRecordVideo(options: { dir: string; size?: { width: number; height: number } }): Promise<void>;
}

export interface Tracing {
  start(options?: { title?: string; screenshots?: boolean; snapshots?: boolean; sources?: boolean }): Promise<void>;
  startChunk(options?: { title?: string }): Promise<void>;
  stopChunk(options?: { path?: string }): Promise<void>;
  stop(options?: { path?: string }): Promise<void>;
  // ferridriver extensions: standalone HAR recording without a full
  // trace.
  startHar(path: string): Promise<void>;
  stopHar(): Promise<void>;
}

export interface Video {
  path(): Promise<string>;
  saveAs(path: string): Promise<void>;
  delete(): Promise<void>;
}

export interface BrowserType {
  name(): string;
  executablePath(): string | null;
  launch(options?: { headless?: boolean; args?: string[] }): Promise<Browser>;
  connect(wsEndpoint: string): Promise<Browser>;
  connectOverCDP(endpoint: string): Promise<Browser>;
  launchPersistentContext(userDataDir: string, options?: BrowserContextOptions & { headless?: boolean; args?: string[] }): Promise<BrowserContext>;
}

export interface BrowserContextOptions {
  viewport?: { width: number; height: number } | null;
  userAgent?: string;
  locale?: string;
  timezoneId?: string;
  geolocation?: { latitude: number; longitude: number; accuracy?: number };
  permissions?: string[];
  extraHTTPHeaders?: Record<string, string>;
  httpCredentials?: { username: string; password: string; origin?: string };
  offline?: boolean;
  colorScheme?: 'light' | 'dark' | 'no-preference';
  reducedMotion?: 'reduce' | 'no-preference';
  forcedColors?: 'active' | 'none';
  isMobile?: boolean;
  hasTouch?: boolean;
  deviceScaleFactor?: number;
  javaScriptEnabled?: boolean;
  bypassCSP?: boolean;
  ignoreHTTPSErrors?: boolean;
  acceptDownloads?: boolean;
  baseURL?: string;
  storageState?: string | StorageState;
  proxy?: { server: string; bypass?: string; username?: string; password?: string };
  serviceWorkers?: 'allow' | 'block';
  screen?: { width: number; height: number };
}

export interface Browser {
  newContext(options?: BrowserContextOptions): Promise<BrowserContext>;
  newPage(options?: BrowserContextOptions): Promise<Page>;
  contexts(): BrowserContext[];
  version(): Promise<string>;
  browserType(): unknown;
  isConnected(): boolean;
  close(): Promise<void>;
  newBrowserCDPSession(): Promise<CDPSession>;
  waitForEvent(event: string, optionsOrPredicate?: number | ((event: unknown) => boolean) | { predicate?: (event: unknown) => boolean; timeout?: number }): Promise<unknown>;
  on(event: string, handler: (arg: unknown) => void): this;
  addListener(event: string, handler: (arg: unknown) => void): this;
  once(event: string, handler: (arg: unknown) => void): this;
  prependListener(event: string, handler: (arg: unknown) => void): this;
  prependOnceListener(event: string, handler: (arg: unknown) => void): this;
  off(event: string, handler?: (arg: unknown) => void): this;
  removeListener(event: string, handler?: (arg: unknown) => void): this;
  removeAllListeners(event?: string): this;
  listeners(event: string): ((arg: unknown) => void)[];
  rawListeners(event: string): ((arg: unknown) => void)[];
  listenerCount(event: string): number;
  eventNames(): string[];
  setMaxListeners(max: number): this;
  getMaxListeners(): number;
}

export interface WebStorage {
  getItem(name: string): Promise<string | null>;
  setItem(name: string, value: string): Promise<void>;
  removeItem(name: string): Promise<void>;
  clear(): Promise<void>;
  items(): Promise<Array<{ name: string; value: string }>>;
}

export interface APIResponse {
  url(): string;
  status(): number;
  statusText(): string;
  ok(): boolean;
  headers(): Record<string, string>;
  headersArray(): Array<{ name: string; value: string }>;
  header(name: string): string | null;
  serverAddr(): { ipAddress: string; port: number } | null;
  body(): Promise<Uint8Array>;
  text(): Promise<string>;
  json(): Promise<unknown>;
  dispose(): Promise<void>;
}

export interface APIRequestOptions {
  headers?: Record<string, string>;
  data?: string | Uint8Array | Buffer | object;
  // ferridriver extension: explicit JSON body (Playwright routes
  // serializable bodies through `data`).
  json?: unknown;
  form?: Record<string, string | number | boolean>;
  multipart?: Record<
    string,
    string | number | boolean | { name: string; mimeType?: string; buffer: Uint8Array | Buffer | string }
  >;
  params?: Record<string, string | number | boolean>;
  timeout?: number;
  failOnStatusCode?: boolean;
  ignoreHTTPSErrors?: boolean;
  maxRedirects?: number;
  maxRetries?: number;
}

export interface APIRequestContext {
  get(url: string, options?: APIRequestOptions): Promise<APIResponse>;
  post(url: string, options?: APIRequestOptions): Promise<APIResponse>;
  put(url: string, options?: APIRequestOptions): Promise<APIResponse>;
  patch(url: string, options?: APIRequestOptions): Promise<APIResponse>;
  delete(url: string, options?: APIRequestOptions): Promise<APIResponse>;
  head(url: string, options?: APIRequestOptions): Promise<APIResponse>;
  fetch(urlOrRequest: string | Request, options?: APIRequestOptions & { method?: string }): Promise<APIResponse>;
  dispose(): Promise<void>;
}

// The QuickJS environment provides Node-parity `Buffer` and the
// web-platform text codecs (UTF-8 only) as globals.
declare global {
  // BrowserType factories: secondary browsers independent of the
  // project's own backend. chromium accepts a transport override
  // (pipe is the default CDP transport).
  function chromium(options?: { transport?: 'pipe' | 'ws' }): BrowserType;
  function firefox(): BrowserType;
  function webkit(): BrowserType;

  // Sandboxed file access rooted at the runner's working directory
  // (writes are additionally scoped -- see the engine's PathSandbox).
  const fs: {
    readFile(path: string): Promise<string>;
    readFileBytes(path: string): Promise<number[]>;
    readFileSync(path: string): string;
    readFileBytesSync(path: string): number[];
    existsSync(path: string): boolean;
    writeFile(path: string, contents: string): Promise<void>;
    readdir(path: string): Promise<string[]>;
    exists(path: string): Promise<boolean>;
  };

  class Buffer extends Uint8Array {
    static from(value: string | ArrayBuffer | Uint8Array | number[], encoding?: 'utf8' | 'utf-8' | 'base64' | 'hex'): Buffer;
    static isBuffer(value: unknown): value is Buffer;
    static concat(buffers: Buffer[]): Buffer;
    static alloc(size: number): Buffer;
    toString(encoding?: 'utf8' | 'utf-8' | 'base64' | 'hex'): string;
  }

  class TextEncoder {
    readonly encoding: 'utf-8';
    encode(input?: string): Uint8Array;
  }

  class TextDecoder {
    constructor(label?: string);
    readonly encoding: string;
    decode(input?: Uint8Array | ArrayBuffer | number[]): string;
  }
}
