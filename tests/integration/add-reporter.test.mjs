import assert from 'node:assert/strict';
import { access } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { run, workspace } from './support.mjs';

// `--add-reporter` (Playwright 1.63) adds to the configured reporters;
// `--reporter` replaces them.
async function reports(flag) {
  const cwd = await workspace({
    'ferridriver.toml': '[test]\ntestMatch = ["*.test.mjs"]\nworkers = 1\nreporter = [{ name = "json" }]\n',
    'a.test.mjs': `import { test } from '@ferridriver/test';
      test('passes', () => {});`,
  });
  const result = await run(['test', '--no-inherit', '--config', 'ferridriver.toml', flag, 'junit'], { cwd });
  assert.equal(result.code, 0, result.text);
  const exists = name => access(join(cwd, 'test-results', name)).then(() => true, () => false);
  return { json: await exists('results.json'), junit: await exists('junit.xml'), text: result.text };
}

test('--add-reporter keeps the configured reporter and adds one', async () => {
  const written = await reports('--add-reporter');
  assert.deepEqual({ json: written.json, junit: written.junit }, { json: true, junit: true }, written.text);
});

test('--reporter replaces the configured reporter', async () => {
  const written = await reports('--reporter');
  assert.deepEqual({ json: written.json, junit: written.junit }, { json: false, junit: true }, written.text);
});
