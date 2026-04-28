/**
 * TS Reporter interface — bridge into ferridriver's event bus.
 *
 * Mirrors Playwright's `Reporter` shape:
 * https://playwright.dev/docs/api/class-reporter — every callback is
 * optional and ferridriver only invokes the ones the user supplied.
 *
 * Usage:
 * ```ts
 * import { defineReporter } from '@ferridriver/test';
 *
 * export default defineReporter({
 *   onBegin(config, suite) { ... },
 *   onTestEnd(test, result) { ... },
 *   onEnd(result) { ... },
 * });
 * ```
 *
 * The default export from a reporter module is registered through the
 * NAPI runner's `registerJsReporter` shim, which translates each
 * `ReporterEvent` variant into the matching JS callback.
 */

/**
 * Subset of Playwright's `FullConfig` surfaced to TS reporters.
 * Fields ferridriver doesn't yet emit are absent rather than stubbed.
 */
export interface ReporterFullConfig {
  metadata?: Record<string, unknown>;
  workers?: number;
}

/** Subset of Playwright's `Suite` object passed to `onBegin`. */
export interface ReporterSuite {
  title: string;
  totalTests: number;
  tests: ReporterTestCase[];
  suites: ReporterSuite[];
}

/** Playwright-shaped TestCase passed to test/step lifecycle callbacks. */
export interface ReporterTestCase {
  id: string;
  title: string;
  location?: { file: string; line?: number; column?: number };
  titlePath: string[];
}

/** Playwright-shaped TestResult. */
export interface ReporterTestResult {
  retry: number;
  status: 'passed' | 'failed' | 'timedOut' | 'skipped' | 'flaky' | 'interrupted' | 'running';
  duration?: number;
  stdout?: string[];
  stderr?: string[];
  errors?: Array<{ message: string; stack?: string }>;
  error?: { message: string; stack?: string } | null;
  attachments?: Array<{ name: string; contentType: string; path?: string }>;
  steps?: ReporterTestStep[];
  workerIndex?: number;
  parallelIndex?: number;
}

/** Playwright-shaped TestStep. */
export interface ReporterTestStep {
  title: string;
  category: string;
  stepId?: string;
  parentStepId?: string | null;
  duration?: number;
  error?: { message: string } | null;
  metadata?: unknown;
}

/** Playwright's `FullResult` emitted on `onEnd`. */
export interface ReporterFullResult {
  status: 'passed' | 'failed';
  duration: number;
  startTime?: string | null;
  totals: {
    total: number;
    passed: number;
    failed: number;
    skipped: number;
    flaky: number;
  };
}

/**
 * Playwright `Reporter` interface. Every method is optional; the ones
 * a user implements get called with the matching event payload.
 */
export interface Reporter {
  printsToStdio?(): boolean;
  onBegin?(config: ReporterFullConfig, suite: ReporterSuite): void;
  onTestBegin?(test: ReporterTestCase, result: ReporterTestResult): void;
  onStepBegin?(test: ReporterTestCase, result: ReporterTestResult, step: ReporterTestStep): void;
  onStepEnd?(test: ReporterTestCase, result: ReporterTestResult, step: ReporterTestStep): void;
  onTestEnd?(test: ReporterTestCase, result: ReporterTestResult): void;
  onEnd?(result: ReporterFullResult): void | Promise<void>;
  onError?(error: { message: string; stack?: string }): void;
  onStdOut?(chunk: string, test?: ReporterTestCase, result?: ReporterTestResult): void;
  onStdErr?(chunk: string, test?: ReporterTestCase, result?: ReporterTestResult): void;
  onWorkerStarted?(payload: { workerId: number }): void;
  onWorkerFinished?(payload: { workerId: number }): void;
  onExit?(): void | Promise<void>;
}

/**
 * The dispatcher signature ferridriver registers via
 * `TestRunner.registerJsReporter`. Callers should typically use
 * `defineReporter` rather than building this directly.
 */
export type ReporterDispatcher = (payload: { event: string; args: unknown[] }) => unknown;

/**
 * Wrap a Reporter implementation in the dispatcher shape ferridriver
 * expects. Returns a function the test runner can register through
 * the NAPI bridge.
 */
export function defineReporter(impl: Reporter): ReporterDispatcher {
  return (payload) => {
    const { event, args } = payload;
    switch (event) {
      case 'onBegin':
        return impl.onBegin?.(args[0] as ReporterFullConfig, args[1] as ReporterSuite);
      case 'onTestBegin':
        return impl.onTestBegin?.(args[0] as ReporterTestCase, args[1] as ReporterTestResult);
      case 'onStepBegin':
        return impl.onStepBegin?.(
          args[0] as ReporterTestCase,
          args[1] as ReporterTestResult,
          args[2] as ReporterTestStep,
        );
      case 'onStepEnd':
        return impl.onStepEnd?.(
          args[0] as ReporterTestCase,
          args[1] as ReporterTestResult,
          args[2] as ReporterTestStep,
        );
      case 'onTestEnd':
        return impl.onTestEnd?.(args[0] as ReporterTestCase, args[1] as ReporterTestResult);
      case 'onEnd':
        return impl.onEnd?.(args[0] as ReporterFullResult);
      case 'onError':
        return impl.onError?.(args[0] as { message: string; stack?: string });
      case 'onStdOut':
        return impl.onStdOut?.(
          args[0] as string,
          args[1] as ReporterTestCase | undefined,
          args[2] as ReporterTestResult | undefined,
        );
      case 'onStdErr':
        return impl.onStdErr?.(
          args[0] as string,
          args[1] as ReporterTestCase | undefined,
          args[2] as ReporterTestResult | undefined,
        );
      case 'onWorkerStarted':
        return impl.onWorkerStarted?.(args[0] as { workerId: number });
      case 'onWorkerFinished':
        return impl.onWorkerFinished?.(args[0] as { workerId: number });
      case 'onExit':
        return impl.onExit?.();
      default:
        return undefined;
    }
  };
}
