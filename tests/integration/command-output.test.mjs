import assert from 'node:assert/strict';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { quote, workspace } from './support.mjs';

test('persistent output waiters observe readiness across chunks without consuming captured output', async () => {
  const root = await workspace({});
  const fifo = quote(join(root, 'signal'));
  await commands.start('stdio', { command: `mkfifo ${fifo}; exec 3<> ${fifo}; printf rea; read signal <&3; printf 'dy\n'; read signal <&3` });
  try {
    const first = commands.waitForOutput('stdio', 'ready');
    const second = commands.waitForOutput('stdio', 'ready');
    await commands.waitForOutput('stdio', 'rea');
    await commands.exec('probe', { command: `printf 'continue\n' > ${fifo}` });
    const results = await Promise.all([first, second]);
    assert.deepEqual(results, ['ready\n', 'ready\n']);
    assert.equal(commands.status('stdio').stdout, 'ready\n');
    assert.equal(await commands.waitForOutput('stdio', 'ready'), 'ready\n');
  } finally { await commands.stop('stdio'); }
});

test('persistent output waiters reject EOF when the readiness marker never arrives', async () => {
  await commands.start('stdio', { command: "printf 'done\n'" });
  try {
    assert.equal(await commands.waitForOutput('stdio', 'done'), 'done\n');
    await assert.rejects(() => commands.waitForOutput('stdio', 'missing'), /closed stdout before emitting/);
  } finally { await commands.stop('stdio'); }
});

test('persistent output waits enforce their deadline and reject interactive streams', async () => {
  await commands.open('stdio', { command: 'cat' });
  try {
    await assert.rejects(() => commands.waitForOutput('stdio', 'missing'), /commands.read/);
  } finally { await commands.stop('stdio'); }
  const root = await workspace({});
  const fifo = quote(join(root, 'signal'));
  await commands.start('stdio', { command: `mkfifo ${fifo}; exec 3<> ${fifo}; echo ready; read signal <&3` });
  try {
    await commands.waitForOutput('stdio', 'ready');
    await assert.rejects(() => commands.waitForOutput('stdio', 'missing', 50), /within 50ms/);
    assert.equal(commands.status('stdio').running, true);
  } finally { await commands.stop('stdio'); }
});
