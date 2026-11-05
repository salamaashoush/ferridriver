// A fixture function's third argument: the running test's info for a
// test-scoped fixture, the worker's for a worker-scoped one.

import { test as base, expect } from '@ferridriver/test';
import type { WorkerInfo } from '@ferridriver/test';

const test = base.extend<{ titled: string; outputDir: string; worker: WorkerInfo }>({
  titled: async ({}, use, testInfo) => {
    await use(testInfo.title);
  },
  outputDir: async ({}, use, testInfo) => {
    await use(testInfo.outputPath('fixture.txt'));
  },
  worker: [
    async ({}, use, workerInfo) => {
      await use(workerInfo);
    },
    { scope: 'worker' },
  ],
});

test('a test fixture reads the test it runs for', async ({ titled, outputDir }, testInfo) => {
  expect(titled).toBe('a test fixture reads the test it runs for');
  expect(outputDir).toBe(testInfo.outputPath('fixture.txt'));
});

test('a worker fixture reads its worker', async ({ worker }, testInfo) => {
  expect(worker.workerIndex).toBe(testInfo.workerIndex);
  expect(worker.parallelIndex).toBe(testInfo.parallelIndex);
});
