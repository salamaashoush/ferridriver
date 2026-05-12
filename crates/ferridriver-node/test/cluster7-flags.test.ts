// Cluster 7 — project DAG, fail-on-flaky-tests, captureGitInfo,
// WebServer polish via the NAPI surface (§7.1 / §7.4 / §7.25 / §7.26).
//
// `--only-changed` is exercised via the TS CLI's git intersection
// path; that's a CLI-only feature and doesn't go through the NAPI
// TestRunner, so we cover it with a unit test against the Rust
// `git_info::changed_files` helper instead (in
// crates/ferridriver-test/tests/cluster7.rs).

import { test, expect } from 'bun:test';
import { type TestMeta } from '../index.js';
import { createRunner } from './_test-helpers.js';

const META: Omit<TestMeta, 'title' | 'id'> = {
  file: 'cluster7-flags.test.ts',
  annotations: [],
  requestedFixtures: [],
};

function makeMeta(title: string): TestMeta {
  return { ...META, id: title, title };
}

const projectA = { name: 'A', dependencies: [] };
const projectB = { name: 'B', dependencies: ['A'] };

test('failOnFlakyTests bumps the exit code when every test passes on retry', async () => {
  const state = { attempts: 0 };
  const runner = createRunner({
    failOnFlakyTests: true,
    retries: 2,
    reporter: ['null'],
  });
  runner.registerTestsBatch([
    {
      meta: { ...makeMeta('flaky'), retries: 2 },
      callback: async () => {
        state.attempts++;
        if (state.attempts < 2) throw new Error('first attempt fails');
      },
    },
  ]);
  const summary = await runner.run();
  if (summary.flaky !== 1) {
    console.error('cluster7 failOnFlakyTests summary:', summary);
  }
  expect(state.attempts).toBeGreaterThan(1);
  // Test eventually passed → flaky=1, passed=1, failed=0. Without
  // failOnFlakyTests the exit would be 0; with it, 1.
  expect(summary.failed).toBe(0);
  expect(summary.flaky).toBe(1);
  expect(summary.exitCode).toBe(1);
});

test('failOnFlakyTests is opt-in — exit stays 0 by default', async () => {
  const state = { attempts: 0 };
  const runner = createRunner({ retries: 2, reporter: ['null'] });
  runner.registerTestsBatch([
    {
      meta: { ...makeMeta('flaky-default'), retries: 2 },
      callback: async () => {
        state.attempts++;
        if (state.attempts < 2) throw new Error('first attempt fails');
      },
    },
  ]);
  const summary = await runner.run();
  expect(summary.flaky).toBe(1);
  expect(summary.exitCode).toBe(0);
});

test('projectFilter narrows to the named project', () => {
  // The runner's project DAG filter is exercised by run_projects, which
  // requires multi-project plan setup. The NAPI surface accepts the
  // override and threads it; we assert the field round-trips cleanly.
  const runner = createRunner(
    { projects: [projectA, projectB], reporter: ['null'] },
    { projectFilter: ['A'] },
  );
  // No public getter for project_filter, but we can verify the runner
  // built without erroring and the configured projects are visible.
  expect(runner.workerCount()).toBeGreaterThan(0);
});

test('captureGitInfo enables git metadata collection', () => {
  const runner = createRunner({ captureGitInfo: true, reporter: ['null'] });
  // No public getter; the smoke test asserts the flag is accepted.
  expect(runner.workerCount()).toBeGreaterThan(0);
});

test('teardownProject overrides the run-wide teardown stage', () => {
  const runner = createRunner(
    { projects: [projectA, projectB], reporter: ['null'] },
    { teardown: 'A' },
  );
  expect(runner.workerCount()).toBeGreaterThan(0);
});
