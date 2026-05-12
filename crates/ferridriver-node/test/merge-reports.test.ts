// Cluster 6 — merge-reports NAPI surface (§7.21).
//
// Writes two blob zips by hand via the NAPI TestRunner + blob
// reporter, then calls mergeReports() and asserts the unified
// summary aggregates both shards.

import { test, expect } from 'bun:test';
import { mkdtempSync, rmSync, readdirSync } from 'fs';
import { tmpdir } from 'os';
import { join } from 'path';
import { mergeReports, type TestMeta } from '../index.js';
import { createRunner } from './_test-helpers.js';

const META: Omit<TestMeta, 'title' | 'id'> = {
  file: 'merge-reports.test.ts',
  annotations: [],
  requestedFixtures: [],
};

function makeMeta(title: string): TestMeta {
  return { ...META, id: title, title };
}

async function runShard(outputDir: string, label: string, shouldFail: boolean): Promise<void> {
  const runner = createRunner({ reporter: ['blob'], outputDir });
  runner.registerTestsBatch([
    {
      meta: makeMeta(label),
      callback: async () => {
        if (shouldFail) throw new Error(`${label} boom`);
      },
    },
  ]);
  await runner.run();
}

test('mergeReports aggregates blob shards into a unified summary', async () => {
  const blobDir = mkdtempSync(join(tmpdir(), 'ferri-merge-test-'));
  try {
    // First shard: passing test.
    await runShard(join(blobDir, 'shard-1'), 'pass-shard', false);
    // Second shard: failing test.
    await runShard(join(blobDir, 'shard-2'), 'fail-shard', true);

    // Collect every blob zip into one directory so mergeReports can
    // pick them up.
    const allBlobs = join(blobDir, 'all');
    require('fs').mkdirSync(allBlobs, { recursive: true });
    let i = 0;
    for (const sub of ['shard-1', 'shard-2']) {
      for (const name of readdirSync(join(blobDir, sub))) {
        if (name.endsWith('.zip')) {
          require('fs').copyFileSync(
            join(blobDir, sub, name),
            join(allBlobs, `report-${i++}.zip`),
          );
        }
      }
    }
    const blobCount = readdirSync(allBlobs).filter((n) => n.endsWith('.zip')).length;
    expect(blobCount).toBeGreaterThanOrEqual(2);

    const mergedOut = join(blobDir, 'merged');
    const summary = await mergeReports(allBlobs, ['null'], mergedOut);

    expect(summary.total).toBe(2);
    expect(summary.passed).toBe(1);
    expect(summary.failed).toBe(1);
    expect(summary.exitCode).toBe(1);
    const titles = summary.results.map((r) => r.title).sort();
    expect(titles).toEqual(['fail-shard', 'pass-shard']);
  } finally {
    rmSync(blobDir, { recursive: true, force: true });
  }
});

test('mergeReports rejects a missing directory', async () => {
  await expect(async () => mergeReports('/nonexistent/ferri-merge-missing')).toThrow();
});
