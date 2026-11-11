// Cucumber step-definition surface of `ferridriver bdd`, as globals —
// the same shape cucumber-js has, plus `bindSteps(test)`, which binds
// the registrars to a `test.extend` chain so a step resolves fixtures
// from it.
//
// Loaded by `tests/tsconfig.json`; step files need no import.

import type { APIRequestContext, Browser, BrowserContext, Page, TestType } from './index';

declare global {
  /// The object every step and hook receives as its first argument, and
  /// as `this`: the scenario's resolved fixtures, the cucumber World
  /// surface, and — when the suite called `setWorldConstructor` — that
  /// instance as its prototype.
  type FerriWorld<TFixtures = object> = TFixtures & CucumberWorld;

  interface CucumberWorld {
    page: Page;
    context: BrowserContext;
    request: APIRequestContext;
    browser: Browser;
    /// `--world-parameters` / `[test] worldParameters`.
    parameters: Record<string, unknown>;
    /// Attach to the test result. A string attaches as `text/plain`
    /// unless `mediaType` says otherwise, bytes as
    /// `application/octet-stream`, anything else as JSON.
    attach(data: string | Uint8Array | ArrayBuffer | object, mediaType?: string): void;
    log(...args: unknown[]): void;
    /// Abort the step and report it skipped.
    skip(): never;
    [key: string]: unknown;
  }

  interface DataTable {
    raw(): string[][];
    rows(): string[][];
    hashes(): Record<string, string>[];
    rowsHash(): Record<string, string>;
    transpose(): DataTable;
  }

  interface StepOptions {
    /// Per-step timeout in milliseconds; overrides `setDefaultTimeout`.
    timeout?: number;
  }

  interface HookOptions extends StepOptions {
    /// Cucumber tag expression: the hook runs only for scenarios it
    /// matches (`'@smoke and not @slow'`).
    tags?: string;
  }

  type StepBody<TFixtures> = (
    this: FerriWorld<TFixtures>,
    world: FerriWorld<TFixtures>,
    ...args: any[]
  ) => unknown | Promise<unknown>;

  type HookBody<TFixtures> = (
    this: FerriWorld<TFixtures>,
    world: FerriWorld<TFixtures>,
    info?: { pickle: { name: string; tags: { name: string }[] }; result: { status: string; message?: string } },
  ) => unknown | Promise<unknown>;

  interface StepRegistrar<TFixtures = object> {
    (pattern: string | RegExp, body: StepBody<TFixtures>): void;
    (pattern: string | RegExp, options: StepOptions, body: StepBody<TFixtures>): void;
  }

  interface HookRegistrar<TFixtures = object> {
    (body: HookBody<TFixtures>): void;
    (tagsOrOptions: string | HookOptions, body: HookBody<TFixtures>): void;
  }

  /// The registrars, bound to one fixture chain.
  interface BoundSteps<TFixtures> {
    Given: StepRegistrar<TFixtures>;
    When: StepRegistrar<TFixtures>;
    Then: StepRegistrar<TFixtures>;
    Step: StepRegistrar<TFixtures>;
    defineStep: StepRegistrar<TFixtures>;
    And: StepRegistrar<TFixtures>;
    But: StepRegistrar<TFixtures>;
    Before: HookRegistrar<TFixtures>;
    After: HookRegistrar<TFixtures>;
    BeforeAll: HookRegistrar<object>;
    AfterAll: HookRegistrar<object>;
    BeforeStep: HookRegistrar<TFixtures>;
    AfterStep: HookRegistrar<TFixtures>;
  }

  const Given: StepRegistrar;
  const When: StepRegistrar;
  const Then: StepRegistrar;
  const defineStep: StepRegistrar;
  const And: StepRegistrar;
  const But: StepRegistrar;

  const Before: HookRegistrar;
  const After: HookRegistrar;
  const BeforeAll: HookRegistrar;
  const AfterAll: HookRegistrar;
  const BeforeStep: HookRegistrar;
  const AfterStep: HookRegistrar;

  /// Bind the registrars to a `test` object's fixture chain — the
  /// native primitive behind playwright-bdd's `createBdd(test)`. A step
  /// registered through them destructures that chain's fixtures from
  /// its first parameter.
  function bindSteps<TFixtures>(test: TestType<TFixtures>): BoundSteps<TFixtures>;

  /// A custom cucumber-expression parameter type. `regexp` is matched
  /// inside the step expression; `transformer` (if given) turns the
  /// matched text into the value the step receives.
  function defineParameterType(definition: {
    name: string;
    regexp: string | RegExp;
    transformer?: (...matches: string[]) => unknown;
  }): void;

  /// Default step/hook timeout in milliseconds.
  function setDefaultTimeout(ms: number): void;

  /// Wrap every step body (cross-cutting retry/log/trace).
  function setDefinitionFunctionWrapper(wrapper: (body: (...args: any[]) => unknown) => (...args: any[]) => unknown): void;

  /// The class instances become the prototype of each scenario's
  /// object, so its methods and constructor-set fields are reachable
  /// through `this`.
  function setWorldConstructor(ctor: new (options: { parameters: Record<string, unknown> }) => object): void;

  /// Accepted for cucumber-js compatibility and intentionally inert:
  /// ferridriver parallelises at the worker level, not per pickle.
  function setParallelCanAssign(...args: unknown[]): void;
}
