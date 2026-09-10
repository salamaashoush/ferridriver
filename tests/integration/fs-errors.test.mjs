import assert from 'node:assert/strict';
import { access, readFile } from 'node:fs/promises';
import { accessSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { workspace } from './support.mjs';

test('filesystem access identifies a missing path in sync and async errors', async () => {
  const path = join(await workspace({}), 'missing');
  const check = error => {
    assert.ok(error instanceof Error);
    assert.equal(error.code, 'ENOENT');
    assert.equal(error.syscall, 'access');
    assert.equal(error.path, path);
    assert.ok(error.errno < 0);
    return true;
  };
  let syncError;
  try { accessSync(path); } catch (error) { syncError = error; }
  check(syncError);
  let asyncError;
  try { await access(path); } catch (error) { asyncError = error; }
  check(asyncError);
});

test('filesystem access preserves a non-directory error', async () => {
  const path = join(await workspace({ file: 'content' }), 'file', 'child');
  const expected = { code: 'ENOTDIR', syscall: 'access', path };
  assert.throws(() => accessSync(path), expected);
  await assert.rejects(access(path), expected);
});

for (const [name, suffix, code, syscall] of [
  ['missing file', 'missing', 'ENOENT', 'open'],
  ['non-directory parent', 'file/child', 'ENOTDIR', 'open'],
  ['directory', '', 'EISDIR', 'read'],
]) {
  test(`filesystem reads preserve ${name} errors in sync and async calls`, async () => {
    const path = join(await workspace({ file: 'content' }), suffix);
    const expected = error => {
      assert.ok(error instanceof Error);
      assert.equal(error.code, code);
      assert.equal(error.syscall, syscall);
      assert.equal(error.path, path);
      assert.ok(error.errno < 0);
      return true;
    };
    assert.throws(() => readFileSync(path), expected);
    await assert.rejects(() => readFile(path), expected);
  });
}
