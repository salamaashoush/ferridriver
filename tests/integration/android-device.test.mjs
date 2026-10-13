import assert from 'node:assert/strict';
import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test, _android } from '@ferridriver/test';
import { McpClient } from './mcp-client.mjs';
import { repo, workspace } from './support.mjs';

const text = bytes => new TextDecoder().decode(bytes);

// Playwright's `_android` against an ADB server that has no Android
// behind it: the fixture lists one usable device and scripts its shell.
async function device(body, options = {}) {
  await commands.start('adb', { binary: join(repo, 'target/debug/ferridriver-adb-fixture') });
  try {
    const ready = await commands.waitForOutput('adb', '\n');
    const port = Number(ready.match(/listening (\d+)/)[1]);
    const devices = await _android.devices({ port, ...options });
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

// The fixture plays Playwright's driver and records what it is sent, so
// each action is checked as the call upstream's dispatcher makes for it.
test('selector actions and input reach the driver as the calls Playwright makes', async () => {
  await device(async phone => {
    const wait = 30000;
    await phone.tap({ text: 'Save', clickable: true }, { duration: 50, timeout: 1234 });
    await phone.fill({ res: /id\/name$/ }, 'Salama');
    await phone.press({ desc: 'Search' }, 'Enter');
    await phone.longTap({ clazz: 'android.widget.Button' });
    await phone.drag({ text: 'Card' }, { x: 10, y: 20 }, { speed: 500 });
    await phone.fling({ scrollable: true }, 'down');
    await phone.pinchClose({ res: 'map' }, 0.5);
    await phone.pinchOpen({ res: 'map' }, 0.25, { speed: 100 });
    await phone.scroll({ scrollable: true }, 'up', 0.75);
    await phone.swipe({ hasChild: { selector: { text: 'Row' } } }, 'left', 0.5);
    await phone.wait({ text: 'Done' }, { state: 'gone' });
    const info = await phone.info({ hasDescendant: { selector: { text: 'Save' }, maxDepth: 2 } });
    await phone.input.type('a1');
    await phone.input.press('Back');
    await phone.input.tap({ x: 1, y: 2 });
    await phone.input.swipe({ x: 0, y: 0 }, [{ x: 5, y: 5 }, { x: 9, y: 9 }], 3);
    await phone.input.drag({ x: 1, y: 1 }, { x: 2, y: 2 }, 4);
    phone.setDefaultTimeout(777);
    await phone.longTap({ selected: false });

    assert.deepEqual(JSON.parse(text(await phone.shell('cat driver-calls'))), [
      { method: 'click', params: { selector: { text: '^Save$', clickable: true }, duration: 50, timeout: 1234 } },
      { method: 'click', params: { selector: { res: 'id\\/name$' }, timeout: wait } },
      { method: 'fill', params: { selector: { res: 'id\\/name$' }, text: 'Salama', timeout: wait } },
      { method: 'click', params: { selector: { desc: '^Search$' }, timeout: wait } },
      { method: 'inputPress', params: { keyCode: 66, timeout: 0 } },
      { method: 'longClick', params: { selector: { clazz: '^android\\.widget\\.Button$' }, timeout: wait } },
      { method: 'drag', params: { selector: { text: '^Card$' }, dest: { x: 10, y: 20 }, speed: 500, timeout: wait } },
      { method: 'fling', params: { selector: { scrollable: true }, direction: 'down', timeout: wait } },
      { method: 'pinchClose', params: { selector: { res: '^map$' }, percent: 0.5, timeout: wait } },
      { method: 'pinchOpen', params: { selector: { res: '^map$' }, percent: 0.25, speed: 100, timeout: wait } },
      { method: 'scroll', params: { selector: { scrollable: true }, direction: 'up', percent: 0.75, timeout: wait } },
      { method: 'swipe', params: { selector: { hasChild: { selector: { text: '^Row$' } } }, direction: 'left', percent: 0.5, timeout: wait } },
      { method: 'wait', params: { selector: { text: '^Done$' }, state: 'gone', timeout: wait } },
      { method: 'info', params: { selector: { hasDescendant: { selector: { text: '^Save$' }, maxDepth: 2 } }, timeout: 0 } },
      { method: 'inputPress', params: { keyCode: 29, timeout: 0 } },
      { method: 'inputPress', params: { keyCode: 8, timeout: 0 } },
      { method: 'inputPress', params: { keyCode: 4, timeout: 0 } },
      { method: 'inputClick', params: { point: { x: 1, y: 2 }, timeout: 0 } },
      { method: 'inputSwipe', params: { segments: [{ x: 0, y: 0 }, { x: 5, y: 5 }, { x: 9, y: 9 }], steps: 3, timeout: 0 } },
      { method: 'inputDrag', params: { from: { x: 1, y: 1 }, to: { x: 2, y: 2 }, steps: 4, timeout: 0 } },
      { method: 'longClick', params: { selector: { selected: false }, timeout: 777 } },
    ]);
    assert.deepEqual(info, {
      clazz: 'android.widget.Button', desc: '', res: 'com.example:id/save', pkg: 'com.example', text: 'Save',
      bounds: { x: 10, y: 20, width: 300, height: 120 },
      checkable: false, checked: false, clickable: true, enabled: true, focusable: true,
      focused: false, longClickable: false, scrollable: false, selected: false,
    });

    await assert.rejects(phone.tap({ text: 'Missing' }), /Timeout waiting for selector/);
    await assert.rejects(phone.input.press('NoSuchKey'), /Unknown key: NoSuchKey/);
    await assert.rejects(phone.input.type('é'), /No mapping for é found/);
    await assert.rejects(phone.fling({}, 'sideways'), /direction/);
    await assert.rejects(phone.wait({}, { state: 'visible' }), /state/);
    await assert.rejects(phone.tap({ text: 7 }), /selector\.text/);
  }, { omitDriverInstall: true });
});

test('web views are listed, announced and closed as the device shows and drops them', async () => {
  await device(async phone => {
    assert.deepEqual(phone.webViews(), []);
    const announced = phone.waitForEvent('webview');
    const seen = [];
    phone.on('webview', view => seen.push(view));
    await phone.push(Buffer.from('0000: 0002 0000 0001 0001 01 5841881 @webview_devtools_remote_4242\n'), 'webviews');

    const view = await announced;
    assert.equal(view.pid(), 4242);
    assert.equal(view.pkg(), 'com.example.app');
    assert.equal(phone.webViews().length, 1);
    assert.equal(phone.webViews()[0], view);
    assert.equal(await phone.webView({ pkg: 'com.example.app' }), view);
    assert.equal(await phone.webView({ socketName: 'webview_devtools_remote_4242' }), view);
    assert.deepEqual(seen, [view]);

    const dropped = new Promise(resolve => view.once('close', resolve));
    await phone.push(Buffer.from(''), 'webviews');
    await dropped;
    assert.deepEqual(phone.webViews(), []);

    await assert.rejects(phone.webView({ pkg: 'com.example.absent' }, { timeout: 700 }),
      /Timeout 700ms exceeded while waiting for event "webview"/);
    await assert.rejects(phone.waitForEvent('webview', { timeout: 600, predicate: () => false }), /Timeout 600ms/);
    await assert.rejects(phone.waitForEvent('tap'), /unknown event "tap"/);

    const closing = phone.waitForEvent('close');
    const heard = new Promise(resolve => phone.on('close', resolve));
    await phone.close();
    assert.equal(await closing, phone);
    assert.equal(await heard, phone);
    await assert.rejects(phone.waitForEvent('webview', { timeout: 1000 }), /Device is closed/);
  });
});

test('a socket carries bytes both ways and says when it closes', async () => {
  await device(async phone => {
    const socket = await phone.open('shell:upper');
    const chunks = [];
    const closed = new Promise(resolve => socket.on('close', resolve));
    socket.on('data', chunk => chunks.push(text(chunk)));
    await socket.write(Buffer.from('salama\n'));
    await closed;
    assert.equal(chunks.join(''), 'SALAMA\n');
    await socket.close();
    await assert.rejects(socket.write(Buffer.from('late')), /Socket is closed/);

    // What arrived before anyone listened is delivered to the first listener.
    const early = await phone.open('shell:held until asked');
    const first = await new Promise(resolve => early.once('data', chunk => resolve(text(chunk))));
    assert.equal(first, 'held until asked');
    await early.close();
  });
});

test('devices says where it looked when no ADB server answers', async () => {
  await assert.rejects(_android.devices({ port: 1 }), /no ADB server at 127\.0\.0\.1:1/);
});

test('a Playwright Android server is refused, with the way that does work', async () => {
  const why = /Playwright's own client\/server protocol.*android\.devices\(\{ host, port \}\)/s;
  await assert.rejects(_android.connect('ws://127.0.0.1:9/'), why);
  await assert.rejects(_android.launchServer(), why);
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
