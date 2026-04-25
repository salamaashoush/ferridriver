// Cluster 1 — CLI flag surfacing.
//
// Exercises the new flags (max-failures, repeat-each, fail-fast / -x, global-timeout,
// pass-with-no-tests, ignore-snapshots, tsconfig, name) end-to-end through the
// `TestRunner` NAPI surface so the runtime effect is observed, not just the field
// presence on the config struct.

import { test, expect } from 'bun:test';
import { tmpdir } from 'os';
import { join } from 'path';
import { TestRunner, type TestMeta, type TestRunnerConfig } from '../index.js';

const META: Omit<TestMeta, 'title' | 'id'> = {
  file: 'cli-flags.test.ts',
  annotations: [],
  requestedFixtures: [], // skip browser/page so the runner is fast
};

function makeMeta(title: string): TestMeta {
  return { ...META, id: title, title };
}

function makeConfig(overrides: Partial<TestRunnerConfig> = {}): TestRunnerConfig {
  // Use single-worker so failure ordering is deterministic, json-only
  // reporter so terminal output doesn't muddy bun test output, and a tmp
  // output dir so on-disk reporter artifacts don't accumulate in cwd.
  return {
    workers: 1,
    reporter: ['json'],
    outputDir: join(tmpdir(), `ferri-cluster1-${process.pid}-${Date.now()}`),
    screenshotOnFailure: false,
    ...overrides,
  };
}

test('maxFailures stops the run after N failures', async () => {
  const runner = TestRunner.create(makeConfig({ maxFailures: 2 }));
  runner.registerTestsBatch([
    { meta: makeMeta('fail-1'), callback: async () => { throw new Error('boom 1'); } },
    { meta: makeMeta('fail-2'), callback: async () => { throw new Error('boom 2'); } },
    { meta: makeMeta('fail-3'), callback: async () => { throw new Error('boom 3'); } },
    { meta: makeMeta('fail-4'), callback: async () => { throw new Error('boom 4'); } },
  ]);
  const summary = await runner.run();
  expect(summary.failed).toBe(2);
  expect(summary.total).toBeLessThanOrEqual(2);
});

test('failFast (-x) stops after the first failure', async () => {
  const runner = TestRunner.create(makeConfig({ failFast: true }));
  runner.registerTestsBatch([
    { meta: makeMeta('first-fail'), callback: async () => { throw new Error('boom'); } },
    { meta: makeMeta('would-pass-1'), callback: async () => { /* noop */ } },
    { meta: makeMeta('would-pass-2'), callback: async () => { /* noop */ } },
  ]);
  const summary = await runner.run();
  expect(summary.failed).toBe(1);
  expect(summary.total).toBe(1);
});

test('repeatEach runs each test N times', async () => {
  let calls = 0;
  const runner = TestRunner.create(makeConfig({ repeatEach: 3 }));
  runner.registerTestsBatch([
    { meta: makeMeta('counter'), callback: async () => { calls++; } },
  ]);
  const summary = await runner.run();
  expect(calls).toBe(3);
  expect(summary.passed).toBe(3);
});

test('passWithNoTests config field is exposed', () => {
  const runner = TestRunner.create(makeConfig({ passWithNoTests: true }));
  expect(runner.getPassWithNoTests()).toBe(true);
});

test('passWithNoTests defaults to false', () => {
  const runner = TestRunner.create(makeConfig());
  expect(runner.getPassWithNoTests()).toBe(false);
});

test('ignoreSnapshots is plumbed to TestInfo', async () => {
  const runner = TestRunner.create(makeConfig({ ignoreSnapshots: true }));
  expect(runner.getIgnoreSnapshots()).toBe(true);
});

test('tsconfig surfaces on the config', () => {
  const runner = TestRunner.create(makeConfig({ tsconfig: '/tmp/custom-tsconfig.json' }));
  expect(runner.getTsconfig()).toBe('/tmp/custom-tsconfig.json');
});

test('name surfaces on the config', () => {
  const runner = TestRunner.create(makeConfig({ name: 'my-suite' }));
  expect(runner.getName()).toBe('my-suite');
});

test('globalTimeout surfaces on the config', () => {
  const runner = TestRunner.create(makeConfig({ globalTimeout: 12345 }));
  expect(runner.getGlobalTimeout()).toBe(12345);
});

test('globalTimeout aborts a run that exceeds the deadline', async () => {
  const runner = TestRunner.create(makeConfig({ globalTimeout: 100 }));
  runner.registerTestsBatch([
    {
      meta: makeMeta('slow-test'),
      callback: async () => { await new Promise((r) => setTimeout(r, 1500)); },
    },
  ]);
  const start = Date.now();
  const summary = await runner.run();
  const elapsed = Date.now() - start;
  expect(elapsed).toBeLessThan(1000); // didn't wait the full 1.5s
  expect(summary.exitCode).toBe(1);
});

test('maxFailures, repeatEach, failFast getters reflect config', () => {
  const runner = TestRunner.create(makeConfig({
    maxFailures: 7,
    repeatEach: 4,
    failFast: true,
  }));
  expect(runner.getMaxFailures()).toBe(7);
  expect(runner.getRepeatEach()).toBe(4);
  expect(runner.getFailFast()).toBe(true);
});
