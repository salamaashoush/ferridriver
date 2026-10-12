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

  test('refuses every call once closed, and is listed anew', async () => {
    const phone = await device();
    await phone.close();
    const refused = await phone.shell('true').then(() => 'ran', (error: Error) => error.message);
    expect(refused).toContain('Device is closed');
    expect((await device()) === phone).toBe(false);
  });
});
