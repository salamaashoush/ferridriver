# Android device API

Playwright's `_android`: the devices behind an ADB server, driven through
that server's own wire protocol. No `adb` binary runs; an ADB server has to
be listening already (`adb start-server`), and a device or emulator has to be
attached to it.

What exists today is the Rust core, `ferridriver::android`:

```rust
use ferridriver::android::{Android, AndroidDevicesOptions, AndroidFile};

let android = Android::new();
let devices = android.devices(AndroidDevicesOptions::default()).await?;
let device = &devices[0];
println!("{} {}", device.serial(), device.model());

let sdk = device.shell("getprop ro.build.version.sdk").await?;
let png = device.screenshot(None).await?;
device.push(AndroidFile::Bytes(b"note".to_vec()), "/data/local/tmp/note.txt", None).await?;
device.install_apk(AndroidFile::Path("app.apk".into()), None).await?;
let logcat = device.open("shell:logcat").await?;
while let Some(chunk) = logcat.next_data().await { /* ... */ }
device.close();
```

`devices` returns the devices the server reports as usable, and the same
object for a device listed twice until it is closed. `AndroidDevicesOptions`
takes the server's `host` and `port`, `127.0.0.1:5037` when absent.

Not there yet: the Node and script bindings, `launchBrowser`, `webView` and
`webViews`, the selector actions (`tap`, `fill`, `wait` and the rest) and
`input`, which need Playwright's driver APK on the device, and
`android.connect` / `launchServer`.

## Checking it on a device

The protocol is covered by unit tests against a scripted ADB server. A real
device is reached through the runtime probe, with an ADB server on `port`:

```bash
echo '[{"op":"android-device","port":5037,"actions":[
  {"action":"shell","command":"getprop ro.build.version.sdk"},
  {"action":"screenshot"}]}]' | target/debug/ferridriver-runtime-probe
```

`push` asks for a mode and the device's ADB daemon widens it: the owner's
bits are copied to group and other, so `0o600` arrives as `0o666`. That is
the daemon's rule for every pushed file, the `adb push` command included.
