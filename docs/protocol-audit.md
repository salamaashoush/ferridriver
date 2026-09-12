# Protocol coverage audit

This records the protocol boundary that the current scripting API actually
implements. It is based on the [WebDriver BiDi specification](https://www.w3.org/TR/webdriver-bidi/),
[Appium session capabilities](https://appium.io/docs/en/3.2/guides/caps/), and
[Apple's Safari WebDriver documentation](https://developer.apple.com/documentation/webkit/about-webdriver-for-safari).

Ferridriver accepts a WebDriver HTTP endpoint through `BrowserType.connect`.
The BiDi path creates one W3C session with `webSocketUrl: true`, preserves
caller supplied standard and namespaced capabilities, and attaches to the
returned BiDi socket.
This is the low-latency path used for Chromium, Firefox, WebKit, and Appium
servers that expose BiDi. It avoids creating a second session after the HTTP
handshake.

Android Chrome through UiAutomator2 uses an HTTP-created Appium session and
attaches the existing Rust CDP backend to the debugger address returned by
`mobile: getChromeCapabilities`. Its device viewport survives attachment, and
closing the browser deletes the owned Appium session. The local Android run
and the required correction to Android driver 14.2.0 are recorded in
[Android verification](android-appium-verification.md).

The Classic WebDriver backend remains incomplete. The shared HTTP session
layer serializes window/frame selection with commands and preserves that lock
when a caller is cancelled. Its page and element implementations are not yet
wired into the public page dispatch. Classic-only Safari and native application
contexts therefore remain unsupported; passing device capabilities does not
establish that those targets work. No iOS device or BrowserStack session has
been verified.

The scripting connection shape for a BiDi-capable driver is:

```ts
const browser = await webkit().connect('http://127.0.0.1:4444', {
  headers: { authorization: `Bearer ${process.env.APPIUM_TOKEN}` },
  timeout: 30_000,
  capabilities: {
    platformName: 'iOS',
    'appium:options': {
      automationName: 'Safari',
      deviceName: 'example-device',
    },
  },
});
```

## Select targets outside the script

The existing instance registry accepts `connectOptions` beside `connectUrl`.
`run --instance target` provisions the target and binds `browser`, `context`,
and `page`; it closes a session it created after the script ends, including
script failures. The script itself needs no protocol or device branch.
The native test runner selects the same registry entry with
`test.browser.instance`. Backend selection also selects its browser product,
so a configured Firefox target launches Firefox rather than inheriting the
default Chromium executable. Remote creation and deletion errors fail the run.

```toml
[browser.instances.target]
connectUrl = "http://127.0.0.1:4725"
[browser.instances.target.connectOptions]
timeout = 120000
[browser.instances.target.connectOptions.capabilities]
browserName = "Chrome"
platformName = "Android"
[browser.instances.target.connectOptions.capabilities."appium:options"]
automationName = "UiAutomator2"
udid = "emulator-5580"
```

```js
await page.goto(args[0]);
await page.getByLabel('Name').fill('sashoush');
await page.getByRole('button', { name: 'Save' }).click();
```

`connectOptions` selects managed WebDriver session creation. A bare
`connectUrl` keeps the existing attachment behavior. Standard and vendor
capabilities remain configuration data, including BrowserStack options.

The native scripting `commands` surface owns local driver and device processes,
so a test can start Appium, `xcrun simctl`, or an Android emulator from the
same session that connects the browser. The process group is reaped with the
session, and `waitForOutput` replaces fixed readiness sleeps:

```ts
const driver = commands.start('appium', { device: 'example-device' });
await commands.waitForOutput('appium', 'listener started', 30_000);
const browser = await webkit().connect('http://127.0.0.1:4723', {
  capabilities: {
    platformName: 'iOS',
    'appium:options': { automationName: 'Safari', deviceName: 'example-device' },
  },
});
```

The driver command must be declared in the script's command allow-list. The
gate verifies process ownership and WebDriver capability construction with a
local mock; physical simulator and device sessions still require their native
drivers and are not claimed by the headless gate.

## Remote session ownership

An HTTP-created session is owned by the connected browser. `browser.close()`
deletes it through the original WebDriver endpoint and reports deletion
failures. Rejected Classic-only sessions and failed BiDi initialization are
deleted before connect returns. Cancellation after receiving a session ID
starts bounded cleanup on the runtime; process termination or cancellation
before the server reveals its ID cannot guarantee deletion. Providers should
also enforce their own idle-session limit.

The connection timeout covers HTTP creation and BiDi attachment, including
initial event subscriptions. Zero disables that connection deadline. Cleanup
has a separate five-second request budget, so failure reporting can take
longer than the connection timeout. No automatic session-creation retries
are made.

Caller headers are used for creation and deletion. They also reach a
negotiated socket on the same origin, mapping HTTPS to WSS and HTTP to WS.
They are not forwarded to another host or port, or to a downgraded transport.
A provider returning another origin must provide an independently authorized
socket URL. HTTP redirects are rejected. Supply credentials through private
environment values, not checked-in configuration or fixture data.

BrowserStack's namespaced `bstack:options` capabilities pass through the same
connection API. Local protocol servers verify their preservation, authenticated
upgrades, cleanup, and failures. This does not establish BrowserStack device
compatibility: no paid sessions or live cloud credentials were used.

The provider's [Automate API limits](https://www.browserstack.com/docs/automate/api-reference/selenium/introduction)
describe its management REST API, not a BiDi command quota. This implementation
does not introduce a concurrent cloud management client or assume that those
limits apply to WebDriver commands. Any future cloud workload scheduler must
use the provider's actual quota and available parallel-session allowance.

## Repeatable local driver probe

With matching Chromium and ChromeDriver binaries installed:

```bash
python3 scripts/probe-webdriver.py
python3 scripts/probe-webdriver.py --mobile
```

The probe runs native scripting through an HTTP-created BiDi session and
verifies fill, click, and text retrieval. Its mobile variant checks the
driver's existing page for width 390, scale factor 3, and one touch point.
HTTP attachment preserves the driver's viewport rather than applying a
desktop default. `browser.newPage()` creates a new isolated context; use
`browser.contexts()[0]` to access the device session's existing pages.

These probes passed locally with Chromium/ChromeDriver 152.0.7977.82 on
Linux. ChromeDriver mobile emulation is browser emulation, not an Android
device run. Rust lifecycle tests and scripting/NAPI protocol fixtures cover
the generic remote connection path independently of a cloud account.
