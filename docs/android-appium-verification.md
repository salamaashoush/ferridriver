# Android Appium verification

On 2026-09-12, `python3 scripts/probe-android-appium.py` passed against a
visible Android 35 emulator running Chrome 124.0.6367.219. The script creates
the Appium session through `chromium().connect()`, drives the existing device
page, and deletes the session through `browser.close()`.

The observed checks were navigation, label and role locators, fill, select,
trusted click, iframe input, route fulfillment, fetch, JS handle evaluation
and disposal, and a PNG screenshot. Device metrics were width 412, pixel ratio
2.625, and five touch points. This is an Android browser run, not desktop
device emulation. It does not prove every Playwright API works on Android.

The complete script took 15,327 ms including session creation and deletion.
Other builds and virtual machines were running; this is a correctness result,
not a performance comparison. Appium recorded a successful DELETE and removal
of session `e34956d8-36df-48f4-89d9-9fcf6415ee6b`. Local artifacts from this
run are in `/tmp/ferridriver-android-appium-_w2bqoac/`.

## Reproduce

The tested components were Android emulator 37.1.11, the Android 35 Google APIs
x86_64 system image, Appium 3.7.0, UiAutomator2 8.6.4, Android driver 14.2.0,
and ChromeDriver 124.0.6367.207. Start one emulator, then an Appium server
listening on localhost. Set its Android SDK and Java environment variables.
Provide a ChromeDriver matching the device browser or explicitly enable the
driver's `uiautomator2:chromedriver_autodownload` feature.

```bash
python3 scripts/probe-android-appium.py \
  --endpoint http://127.0.0.1:4725 --device emulator-5580
```

The probe serves its fixture on the host, reaches it through Android emulator
address `10.0.2.2`, and writes the workflow, result, action trace, and screenshot
to a fresh temporary directory. It is a local emulator probe, not a cloud or
physical-device test.

## Required Appium correction in the tested version

Unmodified Android driver 14.2.0 failed its documented
`mobile: getChromeCapabilities` command with “No ChromeDriver session
capabilities found for context 'CHROMIUM'”. `startChromeSession` calls
`setupNewChromedriver` without a context, and `cacheChromedriverCaps` therefore
returns without caching anything. Ferridriver deleted this failed session.

The passing run used a local source correction in
[appium-android-driver](https://github.com/appium/appium-android-driver),
revision `ceca751e7d3b466be5496b37732b7de5dc3c5a3d`. In
`lib/commands/context/exports.ts`, `startChromeSession` now passes the existing
`CHROMIUM_WIN` constant as the third argument:

```ts
const chromedriver = await setupNewChromedriver.bind(this)(
  opts, this.adb.curDeviceId as string, CHROMIUM_WIN,
);
```

The source was rebuilt with `npm run build`, and the matching source and
compiled module were installed in the isolated local Appium installation.
No upstream patch was published. Reproducing this result on that version
requires the same correction; an unmodified driver is not claimed to pass.
The exact patch is
[`scripts/patches/appium-android-driver-14.2.0-chrome-context.patch`](../scripts/patches/appium-android-driver-14.2.0-chrome-context.patch).

Ferridriver uses the returned debugger address to attach its existing CDP
backend. A remote Appium server returning only a loopback address cannot be
used from another host. Native application contexts, Classic-only Safari,
and remote cloud device execution remain unverified or incomplete.

## One script, configured targets

The probe now uses the existing `run --instance` configuration path. Its
workflow is [`scripts/fixtures/browser-workflow.js`](../scripts/fixtures/browser-workflow.js);
the same source receives `page`, `context`, `browser`, and positional arguments
on every target. Target configuration and session ownership stay outside the
workflow. The script runtime's reported duration therefore excludes the host's
session setup and teardown, unlike the earlier explicit-connect run above.

```bash
python3 scripts/probe-android-appium.py
python3 scripts/probe-android-appium.py --backend cdp-pipe
python3 scripts/probe-android-appium.py --backend cdp-raw
python3 scripts/probe-android-appium.py --backend bidi
python3 scripts/probe-android-appium.py --backend webkit
```

After building the NAPI addon, `--runtime napi` exercises the same workflow
through the Node binding. This is a test harness for the public API; production
scripts use the native runtime and instance registry directly.

`--runtime test` runs that workflow through the native test runner, selecting
the instance with `test.browser.instance`. All fifteen combinations passed:

| Target | Native script | Native test | NAPI |
| --- | --- | --- | --- |
| Android Chrome through Appium | Passed | Passed | Passed |
| Chromium CDP pipe | Passed | Passed | Passed |
| Chromium CDP WebSocket | Passed | Passed | Passed |
| Firefox BiDi | Passed | Passed | Passed |
| Playwright WebKit | Passed | Passed | Passed |

Each run used workflow SHA-256
`3ecf95158be8df8f787d418666887e8ea406f5a4e7d46e72476e9cfc54af7d4f`.
The recorded exit codes and validation results are in
[browser-workflow-verification-2026-09-12.json](browser-workflow-verification-2026-09-12.json).
Those timings were collected during other builds and are correctness evidence
only. Repeat the matrix sequentially with:

```bash
for runtime in script test napi; do
  python3 scripts/probe-android-appium.py --runtime "$runtime" || exit
  for backend in cdp-pipe cdp-raw bidi webkit; do
    python3 scripts/probe-android-appium.py --runtime "$runtime" \
      --backend "$backend" || exit
  done
done
```

Android Chrome has one persistent device context. The runner preserves its
viewport when no viewport override is configured, and explicit `viewport: null`
also disables resizing. This does not provide fresh isolated device storage
for every test. Run one worker per device. Creating another isolated Android
context returns a typed unsupported error rather than a fictitious context.
Failure to delete an owned remote session makes the test run fail even when
the test body passed.
