import assert from 'node:assert/strict';
import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test, _android } from '@ferridriver/test';
import { McpClient } from './mcp-client.mjs';
import { repo, workspace } from './support.mjs';

const text = bytes => new TextDecoder().decode(bytes);

// Playwright's `_android` against an ADB server that has no Android
// behind it: the fixture lists one usable device and scripts its shell.
async function device(body) {
  await commands.start('adb', { binary: join(repo, 'target/debug/ferridriver-adb-fixture') });
  try {
    const ready = await commands.waitForOutput('adb', '\n');
    const port = Number(ready.match(/listening (\d+)/)[1]);
    const devices = await _android.devices({ port });
    assert.equal(devices.length, 1, 'the unauthorized device is not listed');
    await body(devices[0], port);
  } finally { await commands.stop('adb'); }
}

test('devices lists what the server can use, and a device keeps its object until closed', async () => {
  await device(async (phone, port) => {
    assert.equal(phone.serial(), 'emulator-5554');
    assert.equal(phone.model(), 'Fixture Phone');
    assert.equal((await _android.devices({ port }))[0], phone);

    await phone.close();
    await assert.rejects(phone.shell('true'), /Device is closed/);
    const again = (await _android.devices({ port }))[0];
    assert.notEqual(again, phone);
    assert.equal(text(await again.shell('id -u')), 'id -u');
  });
});

test('shell and screenshot return bytes, and a screenshot path is written', async () => {
  await device(async phone => {
    const cwd = await workspace({});
    assert.equal(text(await phone.shell('echo "quoted $HOME"')), 'echo "quoted $HOME"');
    const image = await phone.screenshot({ path: join(cwd, 'screen.png') });
    assert.deepEqual([...image], [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
    assert.deepEqual([...await readFile(join(cwd, 'screen.png'))], [...image]);
  });
});

test('push takes bytes or a path, with the mode it is given', async () => {
  await device(async phone => {
    const cwd = await workspace({});
    await phone.push(Buffer.from('Salama'), '/data/local/tmp/bytes.txt', { mode: 0o600 });
    assert.equal(text(await phone.shell('cat /data/local/tmp/bytes.txt')), '600:Salama');

    await writeFile(join(cwd, 'note.txt'), 'from a file');
    await phone.push(join(cwd, 'note.txt'), '/data/local/tmp/file.txt');
    assert.equal(text(await phone.shell('cat /data/local/tmp/file.txt')), '644:from a file');

    await assert.rejects(phone.push(Buffer.from('x'), '/system/denied.txt'), /Read-only file system/);
    await assert.rejects(phone.push(join(cwd, 'missing.txt'), '/data/local/tmp/x'), /missing\.txt/);
  });
});

test('installApk streams the package with the arguments it is given', async () => {
  await device(async phone => {
    await phone.installApk(Buffer.from('APK'));
    assert.equal(text(await phone.shell('cat installed')), '0:-r -t -S:3');
    await phone.installApk(Buffer.from('APK2'), { args: ['-g', '--user', '0'] });
    assert.equal(text(await phone.shell('cat installed')), '0:-g --user 0:4');
    await assert.rejects(phone.installApk(Buffer.from('BAD package')), /INSTALL_PARSE_FAILED_NOT_APK/);
  });
});

test('devices says where it looked when no ADB server answers', async () => {
  await assert.rejects(_android.devices({ port: 1 }), /no ADB server at 127\.0\.0\.1:1/);
});

test('a device answers to the realm: its server to net, a file to read, a screenshot path to write', async () => {
  await device(async (_phone, port) => {
    const cwd = await workspace({ 'note.txt': 'from a file' });
    const client = await McpClient.launch();
    try {
      const refused = await client.script(`
        const [port, cwd] = args;
        const shape = error => ({ name: error.name, code: error.code });
        const [phone] = await _android.devices({ port });
        const allowed = new TextDecoder().decode(await phone.shell('still allowed'));

        process.permission.drop('read');
        const read = await phone.push(cwd + '/note.txt', '/data/local/tmp/x').then(() => 'pushed', shape);
        const bytes = await phone.push(new Uint8Array([1]), '/data/local/tmp/y').then(() => 'pushed', shape);

        process.permission.drop('write');
        const write = await phone.screenshot({ path: cwd + '/screen.png' }).then(() => 'written', shape);
        const unwritten = (await phone.screenshot()).length;

        process.permission.drop('net');
        const net = await _android.devices({ port }).then(() => 'listed', shape);
        return { allowed, read, bytes, write, unwritten, net };
      `, [port, cwd]);
      const denied = { name: 'PermissionDeniedError', code: 'ERR_ACCESS_DENIED' };
      assert.deepEqual(refused, {
        allowed: 'still allowed', read: denied, bytes: 'pushed', write: denied, unwritten: 8, net: denied,
      });
    } finally { await client.close(); }
  });
});
