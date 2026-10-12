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
await device.close();
```

The Node addon exports the same `_android` object, and Rust has it as
`ferridriver::android::{Android, AndroidDevice}`, where `open(command)` also
returns an `AndroidSocket` on a device service such as `shell:logcat`.

`devices` returns the devices the server reports as usable, and the same
object for a device listed twice until it is closed. Its options take the
server's `host` and `port`, `127.0.0.1:5037` when absent.

In a script a device answers to the realm's permissions: the ADB server's
host and port to its network grant, a file named by path to its read grant,
and a screenshot `path` to its write grant.

Not there yet: `open` in the two bindings, `launchBrowser`, `webView` and
`webViews`, the device's events, the selector actions (`tap`, `fill`, `wait`
and the rest) and `input`, which need Playwright's driver APK on the device,
and `android.connect` / `launchServer`.

## Checking it on a device

The protocol and both bindings are covered against a scripted ADB server
(`ferridriver-adb-fixture`), which needs no Android. A real device is reached
with any script, or through the runtime probe, with an ADB server on `port`:

```bash
echo '[{"op":"android-device","port":5037,"actions":[
  {"action":"shell","command":"getprop ro.build.version.sdk"},
  {"action":"screenshot"}]}]' | target/debug/ferridriver-runtime-probe
```

`push` asks for a mode and the device's ADB daemon widens it: the owner's
bits are copied to group and other, so `0o600` arrives as `0o666`. That is
the daemon's rule for every pushed file, the `adb push` command included.
