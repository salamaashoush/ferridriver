import assert from 'node:assert/strict';
import { realpath, symlink } from 'node:fs/promises';
import { realpathSync } from 'node:fs';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { workspace } from './support.mjs';

// Node resolves the synchronous form itself, one lstat at a time, and hands
// the promise form to the system call, so the two name different syscalls.
for (const [name, operation, syscall] of [['realpath', realpath, 'realpath'], ['realpathSync', realpathSync, 'lstat']]) {
  test(`${name} answers the path behind a link and its dot segments`, async () => {
    const cwd = await workspace({ 'inner/real.txt': 'Salama' });
    await symlink(join(cwd, 'inner'), join(cwd, `alias-${name}`));
    assert.equal(await operation(join(cwd, `alias-${name}`, '..', `alias-${name}`, 'real.txt')), join(cwd, 'inner/real.txt'));
    assert.equal(await operation(cwd), cwd);
  });

  test(`${name} reports a missing path as Node does`, async () => {
    const missing = join(await workspace({}), 'missing');
    await assert.rejects(async () => operation(missing), error => {
      assert.equal(error.code, 'ENOENT');
      assert.equal(error.path, missing);
      assert.equal(error.syscall, syscall);
      return true;
    });
  });
}
