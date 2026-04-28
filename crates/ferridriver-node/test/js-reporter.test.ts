// §7.22 TS Reporter interface bridge — exercise the dispatcher path
// via `defineReporter(...)` + `TestRunner.registerJsReporter(...)`.
//
// Drives a tiny plan (2 passing + 1 failing) through the runner and
// asserts every Playwright lifecycle callback fired with the right
// shape. The failing test exists so onTestEnd's `status: failed` is
// covered alongside the passing path.

import { test, expect } from 'bun:test';
import { tmpdir } from 'os';
import { join } from 'path';
import { TestRunner, type TestMeta, type TestRunnerConfig } from '../index.js';
import {
  defineReporter,
  type Reporter,
  type ReporterFullConfig,
  type ReporterFullResult,
  type ReporterSuite,
  type ReporterTestCase,
  type ReporterTestResult,
} from '../../../packages/ferridriver-test/src/reporter.js';

const META: Omit<TestMeta, 'title' | 'id'> = {
  file: 'js-reporter.test.ts',
  annotations: [],
  requestedFixtures: [],
};

function makeMeta(title: string): TestMeta {
  return { ...META, id: title, title };
}

function makeConfig(overrides: Partial<TestRunnerConfig> = {}): TestRunnerConfig {
  return {
    workers: 1,
    reporter: ['null'],
    outputDir: join(tmpdir(), `ferri-js-reporter-${process.pid}-${Date.now()}-${Math.random().toString(36).slice(2)}`),
    screenshotOnFailure: false,
    ...overrides,
  };
}

interface CallLog {
  begin: Array<[ReporterFullConfig, ReporterSuite]>;
  testBegin: Array<[ReporterTestCase, ReporterTestResult]>;
  testEnd: Array<[ReporterTestCase, ReporterTestResult]>;
  end: ReporterFullResult[];
  exitCount: number;
}

function makeLog(): CallLog {
  return { begin: [], testBegin: [], testEnd: [], end: [], exitCount: 0 };
}

function makeRecordingReporter(log: CallLog): Reporter {
  return {
    onBegin(config, suite) {
      log.begin.push([config, suite]);
    },
    onTestBegin(testCase, result) {
      log.testBegin.push([testCase, result]);
    },
    onTestEnd(testCase, result) {
      log.testEnd.push([testCase, result]);
    },
    onEnd(result) {
      log.end.push(result);
    },
    onExit() {
      log.exitCount += 1;
    },
  };
}

test('TS Reporter dispatcher receives every lifecycle callback with Playwright-shaped payloads', async () => {
  const log = makeLog();
  const dispatcher = defineReporter(makeRecordingReporter(log));

  const runner = TestRunner.create(makeConfig());
  runner.registerJsReporter(dispatcher);
  runner.registerTestsBatch([
    {
      meta: makeMeta('passing-one'),
      callback: async () => {
        // pass
      },
    },
    {
      meta: makeMeta('passing-two'),
      callback: async () => {
        // pass
      },
    },
    {
      meta: makeMeta('failing-one'),
      callback: async () => {
        throw new Error('intentional failure');
      },
    },
  ]);
  const summary = await runner.run();

  expect(summary.total).toBe(3);
  expect(summary.passed).toBe(2);
  expect(summary.failed).toBe(1);

  // Lifecycle counts.
  expect(log.begin.length).toBe(1);
  expect(log.testBegin.length).toBe(3);
  expect(log.testEnd.length).toBe(3);
  expect(log.end.length).toBe(1);
  expect(log.exitCount).toBeGreaterThanOrEqual(1);

  // Begin shape — config has `metadata`/`workers`, suite has totalTests.
  const [config, suite] = log.begin[0];
  expect(typeof config).toBe('object');
  expect(typeof suite.totalTests).toBe('number');
  expect(suite.totalTests).toBe(3);

  // Per-test sanity: titles round-trip.
  const titles = log.testEnd.map(([tc]) => tc.title).sort();
  expect(titles).toEqual(['failing-one', 'passing-one', 'passing-two']);

  // Statuses align with the run.
  const statusByTitle: Record<string, string> = {};
  for (const [tc, result] of log.testEnd) {
    statusByTitle[tc.title] = result.status;
  }
  expect(statusByTitle['passing-one']).toBe('passed');
  expect(statusByTitle['passing-two']).toBe('passed');
  expect(['failed', 'timedOut']).toContain(statusByTitle['failing-one']);

  // onEnd totals match the runner's aggregate.
  const end = log.end[0];
  expect(end.totals.total).toBe(3);
  expect(end.totals.passed).toBe(2);
  expect(end.totals.failed).toBe(1);
  expect(end.status).toBe('failed');
});

test('registerJsReporter accepts multiple reporters and fans events to all of them', async () => {
  const logA = makeLog();
  const logB = makeLog();
  const runner = TestRunner.create(makeConfig());
  runner.registerJsReporter(defineReporter(makeRecordingReporter(logA)));
  runner.registerJsReporter(defineReporter(makeRecordingReporter(logB)));
  runner.registerTestsBatch([
    {
      meta: makeMeta('only-test'),
      callback: async () => {
        // pass
      },
    },
  ]);
  await runner.run();

  expect(logA.testEnd.length).toBe(1);
  expect(logB.testEnd.length).toBe(1);
  expect(logA.end.length).toBe(1);
  expect(logB.end.length).toBe(1);
});

test('A Reporter with no methods registered is silently ignored', async () => {
  const runner = TestRunner.create(makeConfig());
  runner.registerJsReporter(defineReporter({}));
  runner.registerTestsBatch([
    {
      meta: makeMeta('quiet'),
      callback: async () => {
        // pass
      },
    },
  ]);
  const summary = await runner.run();
  expect(summary.failed).toBe(0);
});
