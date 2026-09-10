import assert from 'node:assert/strict';
import { stat, lstat } from 'node:fs/promises';
import { statSync, lstatSync } from 'node:fs';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { workspace } from './support.mjs';

for (const [name, operation, syscall] of [
  ['stat', stat, 'stat'], ['lstat', lstat, 'lstat'],
  ['statSync', statSync, 'stat'], ['lstatSync', lstatSync, 'lstat'],
]) {
  test(`${name} preserves system error metadata for missing paths and non-directory parents`, async () => {
    const cwd = await workspace({ 'file.txt': 'content' });
    for (const [path, code] of [[join(cwd, 'missing'), 'ENOENT'], [join(cwd, 'file.txt/child'), 'ENOTDIR']]) {
      await assert.rejects(async () => operation(path), error => {
        assert.equal(error.code, code);
        assert.equal(error.path, path);
        assert.equal(error.syscall, syscall);
        assert.ok(Number.isInteger(error.errno) && error.errno < 0);
        assert.ok(error.message.includes(code));
        return true;
      });
    }
  });
}
