import assert from 'node:assert/strict';
import { existsSync } from 'node:fs';
import { readdir, readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { run, workspace } from './support.mjs';

const spec = `import { test, expect } from '@ferridriver/test';
test('snap', async ({}, info) => {
  await import('node:fs').then(fs => fs.appendFileSync('attempts.log', 'x'));
  await expect('current value').toMatchSnapshot('value.txt');
});`;

async function suite(mode) {
  return workspace({
    'snap.test.ts': spec,
    'ferridriver.toml': `[test]
      testMatch = ["*.test.ts"]
      retries = 2
      reporter = [{ name = "list" }]
      ${mode ? `updateSnapshots = "${mode}"` : ''}`,
  });
}

// Wherever the snapshot path template puts it.
async function baseline(root) {
  const found = (await readdir(root, { recursive: true })).find(f => f.endsWith('value.txt'));
  return join(root, found ?? 'snap.test.ts-snapshots/value.txt');
}

test('by default a missing baseline is written and the test fails once, without a retry', async () => {
  const root = await suite(null);
  const first = await run(['test', '--no-inherit'], { cwd: root });
  assert.notEqual(first.code, 0, first.text);
  assert.match(first.text, /A snapshot doesn't exist at .*value\.txt, writing actual\./);
  assert.equal(await readFile(await baseline(root), 'utf8'), 'current value');
  assert.equal(await readFile(join(root, 'attempts.log'), 'utf8'), 'x', 'a written baseline is not retried');
  const second = await run(['test', '--no-inherit'], { cwd: root });
  assert.equal(second.code, 0, second.text);
});

test('missing writes the baseline and passes', async () => {
  const root = await suite('missing');
  const result = await run(['test', '--no-inherit'], { cwd: root });
  assert.equal(result.code, 0, result.text);
  assert.equal(await readFile(await baseline(root), 'utf8'), 'current value');
  // `snapshotDir` defaults to `testDir`, which defaults to the config's
  // directory: the baseline sits beside its spec.
  assert.equal(await baseline(root), join(root, 'snap.test.ts-snapshots', 'value.txt'));
});

test('none fails on a missing baseline and writes nothing', async () => {
  const root = await suite('none');
  const result = await run(['test', '--no-inherit'], { cwd: root });
  assert.notEqual(result.code, 0, result.text);
  assert.match(result.text, /A snapshot doesn't exist at .*value\.txt\./);
  assert.equal(existsSync(await baseline(root)), false);
});

test('changed rewrites a mismatched baseline and passes; the default mode fails it', async () => {
  for (const [mode, code] of [['changed', 0], [null, 1]]) {
    const root = await suite(mode);
    await run(['test', '--no-inherit', '--update-snapshots', 'missing'], { cwd: root });
    await writeFile(await baseline(root), 'old value');
    const result = await run(['test', '--no-inherit'], { cwd: root });
    assert.equal(result.code === 0 ? 0 : 1, code, result.text);
    assert.equal(await readFile(await baseline(root), 'utf8'), code === 0 ? 'current value' : 'old value');
  }
});
