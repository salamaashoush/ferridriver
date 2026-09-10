import assert from 'node:assert/strict';
import { readFile, readdir, stat, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { passed, repo, run, workspace } from './support.mjs';

const fixture = name => readFile(join(repo, 'tests/integration/fixtures/native-plan', `${name}.ts`), 'utf8');

async function suite(name, source) {
  return workspace({
    [`${name}.test.ts`]: source,
    'ferridriver.toml': `[test]
testDir = "."
testMatch = ["**/*.test.ts"]
workers = 1
outputDir = "test-results"
snapshotDir = "__snapshots__"
reporter = [{ name = "list" }]
[test.browser]
headless = true
`,
  });
}

const execute = cwd => run(['test', '--no-inherit', '--headless'], { cwd });

test('native plan runs hooks, fixtures, steps, attachments and runtime modifiers', async () => {
  const cwd = await suite('suite', await fixture('suite'));
  passed(await execute(cwd));
});

test('native plan reports a failing body at its original TypeScript location', async () => {
  const cwd = await suite('red', await fixture('red'));
  const result = await execute(cwd);
  assert.notEqual(result.code, 0, result.text);
  assert.ok(result.text.includes('deliberate red'), result.text);
  assert.ok(result.text.includes('red.test.ts:8'), result.text);
});

test('native plan writes correctly named baselines and matches them on a second run', async () => {
  const cwd = await suite('snap', await fixture('snap'));
  passed(await execute(cwd));
  const root = join(cwd, '__snapshots__');
  assert.ok((await stat(root)).isDirectory());
  const perSpec = join(root, 'snap.test.ts-snapshots');
  const entries = await readdir(perSpec);
  for (const name of ['main-shot', 'page-shot', 'header-text', 'literal']) {
    assert.ok(entries.includes(name), JSON.stringify(entries));
    assert.ok((await stat(join(perSpec, name))).isFile());
  }
  assert.deepEqual(await readdir(root), ['snap.test.ts-snapshots']);
  passed(await execute(cwd));
});

test('native plan fails changed content against an existing snapshot baseline', async () => {
  const cwd = await suite('mismatch', await fixture('baseline'));
  passed(await execute(cwd));
  await writeFile(join(cwd, 'mismatch.test.ts'), await fixture('mismatch'));
  const result = await execute(cwd);
  assert.notEqual(result.code, 0, result.text);
});
