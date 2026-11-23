import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { passed, repo, run, workspace } from './support.mjs';

test('exported test declarations include the native module declarations they reference', async () => {
  const root = await workspace({});
  passed(await run(['ext', 'types', '--no-inherit', '--out', root]));
  assert.equal(await readFile(join(root, '@ferridriver/test', 'index.d.ts'), 'utf8'),
    await readFile(join(repo, 'crates/ferridriver-types/packages/test/index.d.ts'), 'utf8'));
  // Where nothing is installed, the Node subset the runtime implements
  // answers the declarations' `/// <reference types="node" />`.
  assert.equal(await readFile(join(root, '@types/node/index.d.ts'), 'utf8'),
    await readFile(join(repo, 'crates/ferridriver-types/packages/test/node.d.ts'), 'utf8'));
});
