# Managed iOS Safari targets

Configure an iOS target in the shared instance registry:

```toml
[browser.instances.phone]
headless = false

[browser.instances.phone.device]
platform = "ios"
model = "iPhone 16"
```

Run the same browser script against that instance:

```bash
ferridriver run --instance phone workflow.js
```

```js
await page.goto('https://example.com');
await page.getByRole('heading', {name: 'Example Domain'}).textContent();
await page.screenshot({path: 'phone.png'});
```

Rust selects Safari and its WebDriver backend, creates a fresh simulator,
starts the private Appium/XCUITest installation, and supplies the existing
`browser`, `context`, and `page` objects. No endpoint, device UUID, or driver
selection is needed in the script. Closing the owned browser shuts down and
deletes its simulator. Interrupted startup also terminates its driver build.
Existing devices are preserved.

Local launch requires macOS and configured Xcode. Missing Node, Appium, and
driver dependencies are provisioned privately. If Xcode has no compatible iOS
runtime, the installer requests one through `xcodebuild -downloadPlatform iOS`.
Provisioning can also be run separately:

```bash
ferridriver install ios
```

`model` accepts a simulator model name or device-type identifier, including
iPad models. Omit it to select a compatible iPhone. Optional `version` selects
an exact runtime version. `timeout` is a positive launch deadline in milliseconds,
including provisioning, and defaults to 300000. Increase it or install first
when downloading an entire runtime.

`headless = true` suppresses ferridriver's Simulator UI launch. Apple's existing
Simulator application can still display newly booted devices in its standard
device set. Appium also writes shared Simulator preferences, so full GUI
isolation is not established.

This target runs mobile Safari in an iOS simulator. Native application views,
physical devices, and complete Playwright API coverage are not established by
the managed-launch verification. The recorded form workflows cover iPhone,
iPad, default model selection, headed/headless execution, trusted positioned
clicks, evaluation, screenshots, and cleanup. See
[the verification record](managed-ios-launch-verification-2026-09-13.json).

The native test runner also runs the shared form and tab workflows against
mobile Safari. `context.newPage()` creates a blank tab without an opener;
the same locator calls navigate, fill, and click within it, and `page.close()`
removes it. The private driver build supplies this operation through Appium's
existing WebDriver backend. Two successive tab creations, trusted clicks, and
cleanup passed on iOS 26.2, alongside the four desktop backend runs of the same
script. See [the tab verification record](ios-new-window-verification-2026-09-13.json)
for commands, upstream revisions, failures, and the tested scope.

Keyboard and pointer actions run through XCUITest's native input endpoint,
then restore the selected web context. Cancelling input while it is queued
prevents dispatch; cancelling in-flight input still allows context restoration.
The unchanged keyboard workflow passed on iOS Safari 26.2, Chromium over pipe
and WebSocket, Firefox/BiDi, and WebKit. The iOS run also checked trusted
positioned clicks and owned-device cleanup. See
[the native input verification record](ios-native-input-verification-2026-09-13.json).

## How Safari on iOS differs

The session is Classic WebDriver through Appium, which reports no events and
sees one document at a time. Where that shows:

- **History.** Appium opens a tab with `window.open('about:blank')`. WebKit
  makes the first navigation out of that document replace its history entry,
  and Safari's own copy of the entry keeps `about:blank`, so `goBack()` to the
  first page a tab visited used to land on a blank document. A new tab is
  therefore given one entry underneath before it is handed over: the first
  navigation is an ordinary push, `history.length` on a new page is 2, and one
  more `goBack()` than on a desktop browser is needed to run out of history.
- **`data:` URLs.** Safari refuses a scripted top-level navigation to one, so
  it is typed into the address field. Text with a space in it would be taken
  for a search, so the URL is percent-encoded first and `page.url()` reports
  that encoded form.
- **Popups.** A window the page opened is found by asking for the window
  handles every 250 ms. Its opener is taken to be the page the session last
  drove, since the protocol does not say. Safari blocks `window.open()` that
  no tap led to, so only a popup opened by a click or a tap arrives.
- **Cookies.** `context.cookies()` and `clearCookies()` reach the cookies the
  current document can see: its host, and the paths above its own.
  `addCookies()` for another host is refused, since the cookie is written
  through the document and would be dropped. Between tests, the cookies of
  each origin the last test visited are cleared from that origin's root, which
  leaves one set for a deeper path.

`tests/device/ios` holds the specs for these, run against a simulator:

```bash
ferridriver test --no-inherit --config tests/device/ios/ferridriver.toml
```

