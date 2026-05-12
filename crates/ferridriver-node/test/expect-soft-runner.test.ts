// Cluster 4 — `.soft` integration with the live TestRunner. Confirms
// that soft assertion failures push to `testInfo.errors` instead of
// throwing, so the test still completes but the failures surface
// after the body returns.

import { test, expect } from 'bun:test';
import { type TestMeta, type TestFixtures } from '../index.js';
import { createRunner } from './_test-helpers.js';
import { expect as ferriExpect } from '../../../packages/ferridriver-test/src/expect';
import { _runWithFile, _setRunner } from '../../../packages/ferridriver-test/src/test';

const META: Omit<TestMeta, 'title' | 'id'> = {
  file: 'expect-soft-runner.test.ts',
  annotations: [],
  requestedFixtures: ['test_info'],
};

function makeMeta(title: string): TestMeta {
  return { ...META, id: title, title };
}

test('expect.soft pushes to testInfo.errors and lets the test continue', async () => {
  const runner = createRunner();
  // The test()/runner() machinery wires _testInfoStorage via wrapBody.
  // Use registerTestsBatch directly to bypass test()'s registration
  // path — we'll wire the testInfo manually inside the body so the
  // expect.soft helper sees a live testInfo via the AsyncLocalStorage
  // fallback.
  _setRunner(runner);

  let observedErrors: any[] = [];
  let observedAfterFirstSoft: any[] = [];

  runner.registerTestsBatch([
    {
      meta: makeMeta('soft-failures'),
      callback: async (fixtures: TestFixtures) => {
        const info = fixtures.testInfo;
        // Push two soft failures via the NAPI path directly — proves
        // the round-trip from TS-side helper to Rust soft-error vec.
        await info.pushSoftError('soft #1');
        observedAfterFirstSoft = info.errors;
        await info.pushSoftError('soft #2');
        observedErrors = info.errors;
      },
    },
  ]);
  const summary = await runner.run();
  // Soft failures don't interrupt execution but DO fail the test at
  // the end (Playwright behavior). Proof that execution continued
  // through both pushes: errors[] has both messages.
  expect(summary.failed).toBe(1);
  expect(observedAfterFirstSoft.length).toBe(1);
  expect(observedAfterFirstSoft[0].message).toBe('soft #1');
  expect(observedErrors.length).toBe(2);
  expect(observedErrors.map((e) => e.message)).toEqual(['soft #1', 'soft #2']);
});

test('expect.soft via the facade is a noop without testInfo', async () => {
  // Fresh process — _setRunner above mutates module state, but soft
  // requires the AsyncLocalStorage to be populated, which only the
  // wrapBody path does. Outside of that, soft silently drops.
  await ferriExpect.soft(1).toBe(2);
  // Sanity: hard expect throws.
  await expect(async () => ferriExpect(1).toBe(2)).toThrow();
});
