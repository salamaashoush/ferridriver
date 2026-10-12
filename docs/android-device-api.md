# Android device API

Playwright's `_android`: the devices behind an ADB server, driven through
that server's own wire protocol. No `adb` binary runs; an ADB server has to
be listening already (`adb start-server`), and a device or emulator has to be
attached to it.

It is the same surface in all three layers. A test or a script imports it
as Playwright names it:

```ts
import { _android } from '@ferridriver/test';

const [device] = await _android.devices();
console.log(device.serial(), device.model());

const sdk = new TextDecoder().decode(await device.shell('getprop ro.build.version.sdk'));
const png = await device.screenshot({ path: 'screen.png' });
await device.push(Buffer.from('note'), '/data/local/tmp/note.txt', { mode: 0o644 });
await device.installApk('app.apk');

const context = await device.launchBrowser({ args: ['--lang=en-US'] });
const page = context.pages()[0];
await page.goto('https://example.com');
await context.close();
await device.close();
```

The Node addon exports the same `_android` object, and Rust has it as
`ferridriver::android::{Android, AndroidDevice}`, where `open(command)` also
returns an `AndroidSocket` on a device service such as `shell:logcat`.

`devices` returns the devices the server reports as usable, and the same
object for a device listed twice until it is closed. Its options take the
server's `host` and `port`, `127.0.0.1:5037` when absent.

`launchBrowser` starts the browser `pkg` (`com.android.chrome` when absent)
afresh and returns its context, the one the device has: the options are a
context's, plus `pkg` and `args`. The package is made the device's debug app,
so a release build reads the command-line file without the browser's own
"command line on non-rooted devices" flag. Closing the context disconnects;
closing the device disconnects every browser it launched.

## Native views

The selector actions and `input` go through Playwright's own driver, two APKs
that run UI Automator on the device. Fetch them once:

```bash
ferridriver install android-driver
```

The first call that needs the driver installs and starts it; pass
`omitDriverInstall: true` to `devices()` for a device that has it already.

```ts
await device.wait({ pkg: 'com.android.settings' });
const info = await device.info({ text: /Network.+/ });
await device.tap({ text: /Network.+/ });
await device.fill({ clazz: 'android.widget.EditText' }, 'wifi');
await device.scroll({ scrollable: true }, 'down', 1);
await device.input.press('Back');
await device.input.tap({ x: 540, y: 1200 });
```

`wait`, `fill`, `press`, `tap`, `longTap`, `drag`, `fling`, `pinchClose`,
`pinchOpen`, `scroll`, `swipe` and `info` take an `AndroidSelector`;
`input` has `type`, `press`, `tap`, `swipe` and `drag`. Each call is sent as
Playwright's own server sends it, so the driver's behaviour is upstream's,
including three things worth knowing:

- A `RegExp` is matched against the whole value, as a string is: `/Network/`
  does not find "Network & internet", `/Network.+/` does.
- `wait` with `state: 'gone'` is sent, and the driver (revision 1001) does not
  read it: it waits for a match either way.
- A `timeout` of `0` reaches the driver as zero, which it reads as "look
  once".

In a script a device answers to the realm's permissions: the ADB server's
host and port to its network grant, a file named by path to its read grant,
and a screenshot `path` to its write grant.

Not there yet: `open` in the two bindings, `webView` and `webViews`, the
device's events, and `android.connect` / `launchServer`.

## Checking it on a device

The protocol and both bindings are covered against a scripted ADB server
(`ferridriver-adb-fixture`), which needs no Android. A real device has a
suite of its own, kept out of the default run, which takes the first usable
device of the server at `ANDROID_ADB_SERVER_PORT` (it needs Chrome there):

```bash
ANDROID_ADB_SERVER_PORT=5037 target/debug/ferridriver test --no-inherit \
  --config tests/device/ferridriver.toml
```

The Rust core alone is reached through the runtime probe:

```bash
echo '[{"op":"android-device","port":5037,"actions":[
  {"action":"shell","command":"getprop ro.build.version.sdk"},
  {"action":"screenshot"}]}]' | target/debug/ferridriver-runtime-probe
```

`push` asks for a mode and the device's ADB daemon widens it: the owner's
bits are copied to group and other, so `0o600` arrives as `0o666`. That is
the daemon's rule for every pushed file, the `adb push` command included.
