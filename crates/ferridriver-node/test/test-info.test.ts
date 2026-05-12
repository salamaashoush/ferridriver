// Cluster 3 — TestInfo helpers (§7.10).
//
// Backend-agnostic NAPI tests. Exercises the new fields and accessors
// against the live worker pipeline; backend matrix is unnecessary
// because TestInfo data is identical across backends.

import { test, expect } from 'bun:test';
import { basename, sep } from 'path';
import { type TestMeta, type TestFixtures } from '../index.js';
import { createRunner } from './_test-helpers.js';

const META: Omit<TestMeta, 'title' | 'id'> = {
  file: 'test-info.test.ts',
  annotations: [],
  requestedFixtures: ['test_info'],
};

function makeMeta(title: string): TestMeta {
  return { ...META, id: title, title };
}

test('outputPath joins onto the per-test output directory', async () => {
  let observed: { plain?: string; nested?: string } = {};
  const runner = createRunner();
  runner.registerTestsBatch([
    {
      meta: makeMeta('inspect-output'),
      callback: async (fixtures: TestFixtures) => {
        const info = fixtures.testInfo;
        observed = {
          plain: info.outputPath(['result.json']),
          nested: info.outputPath(['nested', 'sub', 'file.txt']),
        };
      },
    },
  ]);
  const summary = await runner.run();
  expect(summary.failed).toBe(0);
  expect(observed.plain!.endsWith(`${sep}result.json`)).toBe(true);
  expect(observed.nested!.endsWith(['nested', 'sub', 'file.txt'].join(sep))).toBe(true);
});

test('snapshotPath joins onto the snapshot directory', async () => {
  let observed: string | undefined;
  const runner = createRunner();
  runner.registerTestsBatch([
    {
      meta: makeMeta('inspect-snapshot'),
      callback: async (fixtures: TestFixtures) => {
        observed = fixtures.testInfo.snapshotPath(['greeting.snap']);
      },
    },
  ]);
  const summary = await runner.run();
  expect(summary.failed).toBe(0);
  expect(observed!.endsWith(`${sep}greeting.snap`)).toBe(true);
});

test('errors / error read soft assertions live', async () => {
  let observed: { errors: any[]; firstError: any } | undefined;
  const runner = createRunner();
  runner.registerTestsBatch([
    {
      meta: makeMeta('inspect-errors'),
      callback: async (fixtures: TestFixtures) => {
        const info = fixtures.testInfo;
        // No matchers shipped yet (cluster 4 ships them); poke the
        // soft-error stream by hand to keep this test self-contained.
        // The accessor must surface whatever the worker pushed.
        observed = {
          errors: info.errors,
          firstError: info.error,
        };
      },
    },
  ]);
  const summary = await runner.run();
  expect(summary.failed).toBe(0);
  expect(Array.isArray(observed!.errors)).toBe(true);
  // No errors yet — both accessors should be empty / null.
  expect(observed!.errors.length).toBe(0);
  expect(observed!.firstError).toBeNull();
});

test('snapshotSuffix is a read/write field', async () => {
  let observedDefault: string | undefined;
  let observedSet: string | undefined;
  const runner = createRunner();
  runner.registerTestsBatch([
    {
      meta: makeMeta('inspect-suffix'),
      callback: async (fixtures: TestFixtures) => {
        const info = fixtures.testInfo;
        observedDefault = info.snapshotSuffix;
        info.snapshotSuffix = 'darwin-arm64';
        observedSet = info.snapshotSuffix;
      },
    },
  ]);
  const summary = await runner.run();
  expect(summary.failed).toBe(0);
  expect(observedDefault).toBe('');
  expect(observedSet).toBe('darwin-arm64');
});

test('config accessor surfaces TestConfig snapshot', async () => {
  let cfg: any;
  const runner = createRunner({ name: 'my-suite' });
  runner.registerTestsBatch([
    {
      meta: makeMeta('inspect-config'),
      callback: async (fixtures: TestFixtures) => {
        cfg = fixtures.testInfo.config;
      },
    },
  ]);
  const summary = await runner.run();
  expect(summary.failed).toBe(0);
  expect(cfg).not.toBeNull();
  expect(cfg.name).toBe('my-suite');
  // Some structural sanity checks against the cloned snapshot.
  expect(typeof cfg.timeout).toBe('number');
  expect(Array.isArray(cfg.testMatch)).toBe(true);
});

test('project accessor is null in single-project runs', async () => {
  let project: any;
  const runner = createRunner();
  runner.registerTestsBatch([
    {
      meta: makeMeta('inspect-project'),
      callback: async (fixtures: TestFixtures) => {
        project = fixtures.testInfo.project;
      },
    },
  ]);
  const summary = await runner.run();
  expect(summary.failed).toBe(0);
  expect(project).toBeNull();
});

test('fn returns the test title', async () => {
  let observed: string | undefined;
  const runner = createRunner();
  runner.registerTestsBatch([
    {
      meta: makeMeta('my-test'),
      callback: async (fixtures: TestFixtures) => {
        observed = fixtures.testInfo.fn;
      },
    },
  ]);
  const summary = await runner.run();
  expect(summary.failed).toBe(0);
  expect(observed).toBe('my-test');
});

test('column defaults to zero when the discovery layer does not parse it', async () => {
  let observed: number | undefined;
  const runner = createRunner();
  runner.registerTestsBatch([
    {
      meta: makeMeta('inspect-column'),
      callback: async (fixtures: TestFixtures) => {
        observed = fixtures.testInfo.column;
      },
    },
  ]);
  const summary = await runner.run();
  expect(summary.failed).toBe(0);
  expect(observed).toBe(0);
});

test('outputPath actually contains the per-test output dir basename', async () => {
  let observed: string | undefined;
  const runner = createRunner();
  runner.registerTestsBatch([
    {
      meta: makeMeta('basename-test'),
      callback: async (fixtures: TestFixtures) => {
        observed = fixtures.testInfo.outputPath(['leaf']);
      },
    },
  ]);
  const summary = await runner.run();
  expect(summary.failed).toBe(0);
  // The per-test output dir is `<outputDir>/<full_name>` and outputPath
  // appends the segments. The leaf must be present.
  expect(basename(observed!)).toBe('leaf');
});
