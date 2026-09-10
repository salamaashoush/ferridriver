import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';

test('interactive read timeout preserves partial output for the next read', async () => {
  await commands.open('stdio', { command: "printf head; read signal; printf 'tail\\n'" });
  try {
    await assert.rejects(() => commands.read('stdio', 50), /stdout timed out after 50ms/);
    assert.equal(commands.status('stdio').running, true);
    await commands.write('stdio', 'release\n');
    assert.equal(await commands.read('stdio', 1000), 'headtail');
    assert.equal(await commands.read('stdio', 1000), null);
  } finally { await commands.stop('stdio'); }
});

test('interactive commands preserve long Unicode lines, trailing output and EOF', async () => {
  await commands.open('stdio', { command: 'cat' });
  try {
    const text = 'sashoush café 日本語 '.repeat(4096);
    await commands.write('stdio', text + '\n');
    assert.equal(await commands.read('stdio'), text);
    await commands.write('stdio', 'tail');
    await commands.write('stdio', null);
    assert.equal(await commands.read('stdio'), 'tail');
    assert.equal(await commands.read('stdio'), null);
    await assert.rejects(() => commands.write('stdio', 'closed'));
  } finally { await commands.stop('stdio'); }
});

test('noninteractive persistent commands still drain output into their status tail', async () => {
  await commands.start('stdio', { command: 'printf sashoush' });
  try {
    assert.equal(await commands.wait('stdio'), 0);
    const status = await commands.status('stdio');
    assert.equal(status.running, false);
    assert.equal(status.exitCode, 0);
    assert.equal(status.stdout, 'sashoush');
    await assert.rejects(() => commands.read('stdio'));
  } finally { await commands.stop('stdio'); }
});

test('interactive commands require an allowlisted persistent command', async () => {
  assert.throws(() => commands.open('undeclared'));
  assert.throws(() => commands.open('probe', { command: 'true' }));
});

test('process exit wakes concurrent waiters and remains observable after completion', async () => {
  await commands.open('stdio', { command: 'read signal; exit 7' });
  try {
    const first = commands.wait('stdio');
    const second = commands.wait('stdio');
    await commands.write('stdio', 'release\n');
    assert.deepEqual(await Promise.all([first, second]), [7, 7]);
    assert.equal(await commands.wait('stdio'), 7);
  } finally { await commands.stop('stdio'); }
});

test('a process wait timeout leaves the process available for a subsequent wait', async () => {
  await commands.open('stdio', { command: 'read signal; exit 0' });
  try {
    await assert.rejects(() => commands.wait('stdio', 1), /did not exit within 1ms/);
    assert.equal((await commands.status('stdio')).running, true);
    await commands.write('stdio', 'release\n');
    assert.equal(await commands.wait('stdio'), 0);
  } finally { await commands.stop('stdio'); }
});
