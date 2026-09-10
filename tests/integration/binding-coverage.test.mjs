import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { repo, script } from './support.mjs';

for (const group of ['page_basics', 'page_query', 'page_input', 'page_scripts', 'page_misc', 'locator', 'frame', 'frame_locator']) {
  test(`script bindings: ${group.replaceAll('_', ' ')}`, async () => {
    const fixture = await readFile(join(repo, 'tests/integration/fixtures/binding-coverage.mjs'), 'utf8');
    const result = await script(`import { coverage } from './coverage.mjs'; export default await coverage('${group}');`,
      { 'coverage.mjs': fixture }, { module: true });
    assert.deepEqual(result.value.failed, [], JSON.stringify(result.value));
    assert.ok(result.value.passed > 0);
    assert.equal(result.value.total, result.value.passed);
  });
}
