import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { passed, repo, run, workspace } from './support.mjs';

test('exported test declarations include the native module declarations they reference', async () => {
  const root = await workspace({});
  passed(await run(['ext', 'types', '--no-inherit', '--out', root]));
  for (const name of ['index.d.ts', 'node.d.ts']) {
    assert.equal(await readFile(join(root, '@ferridriver/test', name), 'utf8'),
      await readFile(join(repo, 'packages/ferridriver-test', name), 'utf8'));
  }
});
