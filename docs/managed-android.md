# Managed Android browser targets

Select an Android browser through the same instance registry used by desktop
browsers and remote sessions:

```toml
[browser.instances.phone]
headless = false

[browser.instances.phone.device]
platform = "android"

[test.browser]
instance = "phone"
```

```bash
ferridriver run --instance phone workflow.js
ferridriver test
```

The host provisions dependencies, creates a fresh Android virtual device,
launches the selected Chromium browser package, and supplies the existing
`browser`, `context`, and `page` objects. Closing the owned browser stops the emulator and removes its temporary
profile. Each launch owns its own device; existing emulators are not selected
or stopped.

Each launch also owns a private ADB server and authentication directory. Its
server disables automatic USB, emulator, and mDNS discovery. The emulator uses
ports outside the shared ADB scan range, and closing the browser stops both
owned processes. The normal ADB server, connected devices, and authentication
keys remain separate.

Chrome starts with `--disable-fre`, `--no-first-run`, and
`--no-default-browser-check`. On Android 13 and newer, the launcher grants
Chrome's OS notification permission before starting it. This prevents Chrome's
notification explainer from covering the browser; website permissions remain
controlled by the browser context API.

The launcher disables Chromium's native navigation blur animation. That animation
can discard trusted CDP input even after the destination document has loaded and
its elements pass actionability checks. Custom `--disable-features` lists are
merged with this default, preserving the caller's disabled features and native
input reliability. Arguments after `--` remain positional arguments.

Startup waits for Chrome to complete a bounded request to a temporary endpoint
on the host loopback interface, reached through the emulator's `10.0.2.2`
alias. Wi-Fi association and `navigator.onLine` can precede installation of
Android's IPv4 routes. The probe shares the launch deadline, closes its listener
on completion or cancellation, and verifies the emulator-to-host path. DNS and
external Internet availability remain properties of the host network. See the
[emulator address-space documentation](https://developer.android.com/studio/run/emulator-networking-address).

The default image is Android API 35 with Google APIs and the `pixel_7` hardware
profile. The device options are `apiLevel`, `model`, `sdkPath`, `timeout` in
milliseconds, `acceptLicenses`, `pkg`, and `apkPath`. The launch deadline includes provisioning;
zero disables that deadline. `headless = true` hides the emulator window.

`pkg` selects an installed Chromium application package and defaults to
`com.android.chrome`. `apkPath` optionally installs a local APK into the fresh
owned emulator. Its manifest package must match `pkg`; the APK must support
the selected Android API and emulator ABI. The supplied APK is not modified.
Browser flags use the existing instance `args` setting:

```toml
[browser.instances.phone]
headless = true
args = ["--enable-features=WebMCP,WebMCPTesting,DevToolsWebMCPSupport"]

[browser.instances.phone.device]
platform = "android"
pkg = "org.chromium.chrome"
apkPath = "/path/to/ChromePublic.apk"
```

The selected package is designated as the debug app on the owned emulator so
release builds can read Chromium's command-line file. Arguments are quoted for
Chromium's Android parser and transferred as a file through private ADB.
Debugging socket/port arguments are reserved by the managed launcher.

The SDK is resolved from `sdkPath`, `ANDROID_HOME`, `ANDROID_SDK_ROOT`, the
standard Android Studio SDK location, then ferridriver's browser cache.
Missing command-line tools, Java runtime, platform tools, emulator, and system
image are installed as needed. Downloads of command-line tools and Java are
checked against their published SHA-256 digests. Existing SDK packages are reused.
SDK licenses must already be accepted or explicitly accepted by the caller:

```bash
ferridriver install android --accept-licenses
ferridriver install android --android-api-level 35 --android-sdk "$ANDROID_HOME"
```

Local Android uses ADB and the existing CDP backend, following
[Playwright's Android browser implementation](https://github.com/microsoft/playwright/blob/main/packages/playwright-core/src/server/android/android.ts).
It does not require a local Appium server. Remote Appium targets continue to use
`connectUrl` and `connectOptions` in the instance registry.

This is Chrome running on an Android emulator, not native application automation
or desktop mobile emulation. Android Chrome has a persistent browser context;
independent incognito contexts are not supported by its CDP implementation.
The launcher supports Linux x86-64 and macOS Intel/Apple Silicon host layouts.
Installed Safari through `safaridriver`, iOS simulator lifecycle, Windows hosts,
and native Appium application views are not implemented by this launcher.

Primary protocol references:

- [SDK manager](https://developer.android.com/tools/sdkmanager)
- [AVD manager](https://developer.android.com/tools/avdmanager)
- [Emulator command line and console-port reporting](https://developer.android.com/studio/run/emulator-commandline)
- [Command-line tools downloads and checksums](https://developer.android.com/studio#command-tools)

The shared workflow and device cleanup can be checked without starting Appium
or an emulator beforehand:

```bash
python3 scripts/probe-android-appium.py --managed-android --sdk "$ANDROID_HOME"
python3 scripts/probe-android-appium.py --managed-android --sdk "$ANDROID_HOME" --runtime test
```

The probe writes the process exit code, workflow result, screenshot, and device,
ADB-forward, and emulator/ADB process snapshots into its printed artifact
directory. A script result alone does not pass the probe if resources remain
after teardown.
