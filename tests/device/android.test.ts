// Playwright's `_android` on a real device, the first one the ADB server at
// ANDROID_ADB_SERVER_PORT (5037 when unset) reports as usable. Needs Chrome
// on the device for the browser tests.
import { test, expect, _android } from '@ferridriver/test';
import type { AndroidDevice } from '@ferridriver/test';

const port = Number(process.env.ANDROID_ADB_SERVER_PORT ?? 5037);
const text = (bytes: Uint8Array) => new TextDecoder().decode(bytes);

async function device(): Promise<AndroidDevice> {
  const devices = await _android.devices({ port });
  expect(devices.length).toBeGreaterThan(0);
  return devices[0];
}

test.describe('an Android device', () => {
  test('answers for itself and runs shell commands', async () => {
    const phone = await device();
    expect(phone.serial().length).toBeGreaterThan(0);
    expect(phone.model()).toBe(text(await phone.shell('getprop ro.product.model')).trim());
    expect(text(await phone.shell('echo $((6 * 7)); id -un'))).toBe('42\nshell\n');
    expect((await _android.devices({ port }))[0] === phone).toBe(true);
  });

  test('takes a screenshot as a PNG', async () => {
    const phone = await device();
    const image = await phone.screenshot();
    expect([...image.slice(0, 8)]).toEqual([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
    expect(image.length).toBeGreaterThan(10_000);
  });

  test('pushes a file the device can read back', async () => {
    const phone = await device();
    const path = '/data/local/tmp/ferridriver-device-test.txt';
    await phone.push(new TextEncoder().encode('Salama\n'), path);
    try {
      expect(text(await phone.shell(`cat ${path}`))).toBe('Salama\n');
    } finally {
      await phone.shell(`rm -f ${path}`);
    }
  });

  test('launches Chrome with its arguments and drives a page in it', async () => {
    const phone = await device();
    const context = await phone.launchBrowser({ args: ['--user-agent=ferridriver-device-test'] });
    try {
      expect(context.pages().length).toBe(1);
      const page = context.pages()[0];
      await page.goto(
        "data:text/html,<title>phone</title><button id='b' onclick='document.title = String(event.isTrusted)'>tap</button>",
      );
      await page.locator('#b').click();
      expect(await page.title()).toBe('true');
      expect(await page.evaluate(() => navigator.userAgent)).toBe('ferridriver-device-test');
      expect(await page.evaluate(() => navigator.maxTouchPoints)).toBeGreaterThan(0);

      const second = await context.newPage();
      await second.goto('data:text/html,<title>second</title>');
      expect(context.pages().length).toBe(2);
      expect(text(await phone.shell('ls /data/local/tmp/chrome-command-line 2>&1'))).toContain('No such file');
    } finally {
      await context.close();
    }
  });

  // Needs Playwright's driver in the cache: `ferridriver install android-driver`.
  test('finds and acts on native views through the driver', async () => {
    const phone = await device();
    await phone.shell('am force-stop com.android.settings');
    await phone.shell('am start -a android.settings.SETTINGS');
    try {
      await phone.wait({ pkg: 'com.android.settings' });
      const info = await phone.info({ text: /Network.*/ });
      expect(info.pkg).toBe('com.android.settings');
      expect(info.text).toContain('Network');
      expect(info.bounds.width).toBeGreaterThan(0);

      await phone.tap({ text: /Network.*/ });
      await phone.wait({ text: /Internet|SIMs|Airplane mode/ }, { timeout: 15_000 });
      await phone.input.press('Back');
      await phone.wait({ text: /Network.*/ }, { timeout: 15_000 });

      await phone.tap({ text: /Search.*/ });
      await phone.fill({ clazz: 'android.widget.EditText' }, 'wifi');
      expect((await phone.info({ clazz: 'android.widget.EditText' })).text).toBe('wifi');
      await phone.input.press('Back');
      await phone.input.press('Back');

      const missing = await phone
        .wait({ text: 'No such view anywhere' }, { timeout: 1_500 })
        .then(() => 'found', (error: Error) => error.message);
      expect(missing).toContain('Timed out waiting for selector');
      await phone.scroll({ scrollable: true }, 'down', 1);
    } finally {
      await phone.input.press('Home');
    }
  });

  test('opens a socket on a device service and talks both ways', async () => {
    const phone = await device();
    const log = await phone.open('shell:logcat -d -t 20');
    let bytes = 0;
    const ended = new Promise<void>(resolve => log.on('close', () => resolve()));
    log.on('data', chunk => {
      bytes += chunk.length;
    });
    await ended;
    await log.close();
    expect(bytes).toBeGreaterThan(0);

    const shell = await phone.open('shell:read line; echo "heard $line"');
    const said = new Promise<string>(resolve => shell.once('data', chunk => resolve(text(chunk))));
    await shell.write(new TextEncoder().encode('salama\n'));
    expect((await said).trim()).toBe('heard salama');
    await shell.close();
  });

  // The HTML viewer every image ships shows its file in a web view.
  test('announces a web view an application shows and drives the page in it', async () => {
    const phone = await device();
    const viewer = 'com.android.htmlviewer';
    await phone.shell(`am force-stop ${viewer}`);
    const announced = phone.waitForEvent('webview', { timeout: 20_000, predicate: view => view.pkg() === viewer });
    await phone.shell(
      `am start -n ${viewer}/.HTMLViewerActivity -a android.intent.action.VIEW -t text/html -d file:///sdcard/Download/none.html`,
    );
    const view = await announced;
    try {
      expect(view.pid()).toBeGreaterThan(0);
      expect(phone.webViews().includes(view)).toBe(true);
      expect((await phone.webView({ pkg: viewer })) === view).toBe(true);

      const page = await view.page();
      await page.goto("data:text/html,<title>inside a web view</title><h1 id='h'>Salama</h1>");
      expect(await page.title()).toBe('inside a web view');
      expect(await page.locator('#h').textContent()).toBe('Salama');

      const dropped = new Promise<void>(resolve => view.once('close', () => resolve()));
      await phone.shell(`am force-stop ${viewer}`);
      await dropped;
      expect(phone.webViews().includes(view)).toBe(false);
    } finally {
      await phone.shell(`am force-stop ${viewer}`);
    }
  });

  test('refuses every call once closed, and is listed anew', async () => {
    const phone = await device();
    await phone.close();
    const refused = await phone.shell('true').then(() => 'ran', (error: Error) => error.message);
    expect(refused).toContain('Device is closed');
    expect((await device()) === phone).toBe(false);
  });
});
