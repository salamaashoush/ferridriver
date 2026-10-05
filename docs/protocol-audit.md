# Protocol coverage audit

This records the protocol boundary that the current scripting API actually
implements. It is based on the [WebDriver BiDi specification](https://www.w3.org/TR/webdriver-bidi/),
[Appium session capabilities](https://appium.io/docs/en/3.2/guides/caps/), and
[Apple's Safari WebDriver documentation](https://developer.apple.com/documentation/webkit/about-webdriver-for-safari).

Ferridriver accepts a WebDriver HTTP endpoint through `BrowserType.connect`.
Chromium and Firefox request BiDi by default and adopt the same session through
Classic WebDriver if the server returns no BiDi socket. An advertised socket
that fails to connect or initialize fails the connection and releases the
session; it does not silently downgrade or create another session.

Set `capabilities.webSocketUrl` to `true` to require BiDi or `false` to use
Classic. Explicit instance backends also select the protocol; conflicting
capabilities fail before session creation. Safari defaults to Classic, while
Playwright WebKit remains a separate browser product and cannot connect to a
WebDriver endpoint. Standard and namespaced capabilities retain their values.
Servers that reject a BiDi capability during session creation are not retried;
select Classic explicitly for those servers.

Android Chrome through UiAutomator2 creates one Appium session. Automatic
selection uses CDP when `mobile: getChromeCapabilities` supplies a usable
debugger address. Missing discovery support or a provider's remote loopback
address selects Classic WebDriver on that same session. Authentication errors,
provider failures, timeouts, and failed CDP attachment fail the connection;
they do not trigger another session or silently downgrade. Explicit Classic
selection avoids CDP attachment. The device viewport survives CDP attachment,
and closing the browser deletes the owned Appium session. The local Android run
and the required correction to Android driver 14.2.0 are recorded in
[Android verification](android-appium-verification.md).

The Classic WebDriver backend is wired into the shared page dispatch. Its HTTP
session layer serializes window/frame selection with commands and preserves
that lock when a caller is cancelled. Desktop Safari and iOS simulator Safari
have passed the scoped workflows recorded below through public bindings.
Coverage remains incomplete; these results do not establish native application
automation, physical iOS device support, or BrowserStack verification.

The scripting connection shape for a BiDi-capable driver is:

```ts
const browser = await safari().connect('http://127.0.0.1:4444', {
  headers: { authorization: `Bearer ${process.env.APPIUM_TOKEN}` },
  timeout: 30_000,
  capabilities: {
    webSocketUrl: true,
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

## Hosted local sessions

`session open --instance` uses the same configured browser or device target as
`run --instance`. Browser product, protocol negotiation, capabilities and
ownership stay in Rust core. Separate script calls retain the page and JavaScript
state:

```sh
ferridriver session open desktop --instance desktop-safari
ferridriver run --session desktop --eval 'return await page.title();'
ferridriver session close desktop
```

`session close` requests cleanup through the session connection and waits for its
result. Successful cleanup removes the descriptor; a failed provider deletion
keeps the owner and endpoint available for another close attempt. New scripts
are rejected while cleanup is pending. Close does not signal a PID from a stale
descriptor. Session names have safe storage keys, publication holds an exclusive
file claim, and each local binding owns a distinct socket directory.

Real Safari and managed Android passed the same portable form through configured
session hosts. Safari deletion completed before the close response. Android's
owned emulator, private ADB and temporary profile were absent in the immediate
post-close inventory; existing devices, forwards and authentication remained
unchanged.

These are local session foundations. TCP endpoints currently carry framed JSON
despite their `ws://` spelling; they are not an authenticated WebSocket service.
Startup cancellation, authenticated remote transport, worker isolation and durable
cleanup recovery remain required for the hosted cloud platform.

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

## Safari and managed Android verification

Safari support remains incomplete. A native macOS 15.7.9 build and
`ferridriver install safari` passed against Safari 26.6.1
(20624.5.1.18.3). After clearing an abandoned pairing and disabling automatic
desktop locking in the test VM, the Classic WebDriver core probe passed through
a remote endpoint and then passed five consecutive native macOS runs. Each
native run automatically launched its driver, exercised trusted input, frame
and window selection, captured a 15,824-byte PNG, and deleted its session.
No driver from those five runs remained afterward. The initial locked-desktop
run had timed out and failed cleanup. These results are not public scripting
API or Safari parity proof.
Those probe runs predated integration into the shared backend dispatch;
the Safari factory's experimental BiDi path was insufficient.

On 2026-09-13, the updated remote probe passed six consecutive runs with shared
window lifecycle checks against that VM. Repeated enumeration now reuses each Classic page's
state and event emitter. Closing through one handle closes its peers without
closing another tab; enumeration retires windows closed remotely, and session
shutdown closes all retained handles. A local protocol regression covers those
transitions and rejection of commands on a closed page. The real Safari probe
returned `sharedWindowLifecycle: true`, with exit code 0 in every run. This remains an
internal backend probe, not public scripting coverage.

The subsequent Classic dispatcher integration passed all 17 tests in
`tests/e2e/handles.test.ts` through the native QuickJS runner on desktop
Safari 26.6.1 (9.5 seconds, exit 0). It exposed and fixed stale current-window
selection after fixture teardown and missing selectors throwing instead of
returning an absent handle. The same file passed on Chromium pipe, Chromium
WebSocket, Firefox BiDi, and WebKit. Firefox initially rejected a Safari-only
event subscription; the explicit event workaround is now confined to Safari.

The real iOS 26.2 simulator on the Xcode 26.3 VM ran the same 17 tests:
14 passed and three input tests failed (34.9 seconds, exit 1). Object handles,
rich-value serialization, disposal, scoped queries, evaluation, and waits
passed. Checkbox input, right-click options, and a button click assertion
failed. Direct WebDriver pointer actions and native element clicks also
produced trusted `touchstart` without `touchend` or `click`, including after
a one-second observation window. This is a reproducible input-delivery gap,
not full iOS support. No assertions were skipped or weakened.
Repeating this direct `safaridriver` run after the Appium adapter changes still
gave 14 passes and the same three failures (44.3 seconds, exit 1). The Appium
workflow below uses XCUITest's native input service and does not repair this
separate Safari driver input path.

To repeat these public scripting checks, create a config using an already
running WebDriver endpoint:

```json
{
  "browser": {"instances": {"target": {
    "browser": "safari", "headless": false,
    "connectUrl": "http://127.0.0.1:4450",
    "connectOptions": {"timeout": 30000, "capabilities": {
      "platformName": "ios", "safari:useSimulator": true
    }}
  }}},
  "test": {"workers": 1, "browser": {"instance": "target"}}
}
```

```bash
target/debug/ferridriver test --no-inherit --config safari-probe.json \
  tests/e2e/handles.test.ts --workers 1
```

Add `safari:deviceUDID` when multiple simulators are available. For desktop
Safari, use its endpoint and an empty capabilities object. Neither config
requests a viewport override: iOS advertises `setWindowRect: false` and keeps
its native viewport. These commands require an available device and driver;
automatic iOS installation and device launch remain unfinished.

The rebuilt NAPI addon also passed its five WebDriver connection tests and a
real desktop Safari workflow covering trusted fill/click, rich object handles,
and opening a tab after closing the preceding one. The two native scripting
connection regressions passed through the rebuilt runtime-probe executable.

An independent Appium comparison used Node 24.21.0, Appium 3.7.0, XCUITest
12.12.3, and WebDriverAgent 16.12.8 in the iOS VM. Driver diagnostics reported
zero required fixes. The first session exceeded the client's 180-second
startup deadline; the driver subsequently started, and its session was
explicitly deleted. A warm session started in 2.36 seconds. With
`appium:nativeWebTap: true`, a form probe observed trusted `touchstart`,
`touchend`, `mousedown`, `mouseup`, and `click` events, and submitted the typed
value. The event handler was installed by script execution: writing an inline
handler into the initial Google document inherited its CSP and produced an
invalid first comparison. The successful probe deleted its session too.

The subsequent shared Rust implementation discovers Safari's actual version
from its user agent when XCUITest omits `browserVersion`, awaits promises through
Appium's callback script endpoint, and preserves the simulator viewport. Native
pointer coordinates come from the target's accessibility rectangle, including
Safari's input zoom. The session lock covers temporary label changes, native
lookup, and restoration even when the caller is cancelled. Empty modifier lists
no longer send empty WDA action sequences, which took about 5.3 seconds each in
the recorded failing probe.

`scripts/probe-ios-safari.py` runs the identical
`scripts/fixtures/portable-form-workflow.js` through NAPI or native scripting.
Both passed five consecutive simulator runs: fill, positioned trusted clicks,
original accessibility-label restoration, promise resolution and rejection,
rich handles, screenshot, and session cleanup. The script target only names
Safari and its endpoint; backend selection occurs in Rust. Recorded samples and
repeatable commands are in [iOS verification](ios-safari-verification-2026-09-13.json).
These are warm simulator/server measurements including session creation and
cleanup, not cold device launch measurements or comparisons with other tools.
Automatic iOS provisioning, additional tabs through XCUITest, and the full
pointer/event surface remain unverified or incomplete.

The full readiness run at `target/gate/1789259442-306187` exited 1 after
710.11 seconds: 114 checks, two failed groups. Format, workspace Clippy,
Rust tests, browser e2e, BDD, types, docs, and all 50 NAPI files passed.
Acceptance rejected the existing mobile screenshot baseline; the actual image
matched the independent Playwright reference pixel for pixel. The baseline was
preserved. Integration had 823 passes and one Rust UI timeout while concurrent
Cargo jobs changed the shared build artifacts. The gate now reserves its Cargo
slot for integration, whose UI harness invokes Cargo for discovery and runs.
The final scoped `just ready --only integration --only format` exited 0:
five checks passed in 76.33 seconds, including workspace Clippy and 824
integration tests (one existing skip). This rerun used warm build artifacts;
it does not measure a clean-cache build. The screenshot baseline still prevents
a green full gate and remains unchanged.

Further XCUITest tab probes confirmed that `POST window/new` returns HTTP 405
with `unknown method`, Appium's `NotYetImplementedError` wire value. The shared
decoder now maps that response to `Unsupported` while preserving the operation
and driver message. A regression test pins the observed response. The final
`just ready --only format --only rust-build` passed 52 checks in 53.02 seconds,
including workspace Clippy and Rust test executables.

With the existing Safari preferences, `window.open('about:blank', '_blank')`
returned false and the window list stayed unchanged. A separate session with
`appium:safariAllowPopups: true` failed during execution; the next connection
also timed out. Restarting only the probe Appium server and simulator Safari
restored the shared workflow, which passed with exit 0 and successful session
deletion (`/tmp/ferridriver-ios-safari-f2g3ijgj`). The popup preference was set
back to false. No popup-based tab fallback was added, and XCUITest
`context.newPage()` remains incomplete.

Classic page creation now owns its result until the caller receives it. If
the caller is cancelled while the driver creates the tab, dropping the
unclaimed result closes that tab. Failed initial navigation also closes only
the new window. Protocol regressions pause creation before its response and
check the exact deleted handle. The public Rust `new_page_with_url` helper
likewise closes its newly created page when navigation fails.

The NAPI page-count regression exposed an event-reference bug: temporary
`context.pages()` wrappers replaced a persistent page's weak reference, then
console delivery stopped when those temporary wrappers disappeared. Backends
now keep weak fallbacks and resolve the newest live wrapper. The previously
failing listener-identity and prepend-order assertions pass unchanged.
Malformed navigation URLs are validated in Rust after base-URL resolution,
with localhost completion matching Playwright's `server/helper.ts`.

Final verification on this state used:

```bash
just ready --only rust-build --only napi --only integration --only e2e --only bdd --only format
```

It exited 0: 110 checks in 174.03 seconds, including 2,267 e2e tests,
824 integration tests, and 637 BDD scenarios. Existing skips remained unchanged.
Logs are in `target/gate/1789262663-950302`. Desktop Safari's public NAPI
workflow rejected an invalid URL, kept its original tab count at one, and
preserved the original document. The native scripting test
`navigation-url-order.test.ts` filtered to `invalid navigation` also passed
on desktop Safari (one test, 768 ms, exit 0). WebKit passed the three cleanup/listener
checks from `test/browser.test.ts`; an independent Firefox BiDi workflow
passed cleanup and console delivery after page enumeration and garbage
collection. Running that general NAPI test file on BiDi still fails its
media-reset teardown, which requests unsupported media emulation. That
limitation was not skipped or changed. The native iOS workflow passed again
with artifacts at `/tmp/ferridriver-ios-safari-gswc2kvt`.

Base-URL resolution now uses the existing URL parser instead of concatenating
paths. Query-only and fragment-only references preserve the base document;
parent paths, host-relative references, and URL normalization match JavaScript
`new URL()`. Invalid input retains Playwright's fallback behavior. Rust, NAPI,
and native scripting regressions cover the change. The broad scoped gate
passed 110 checks in 150.09 seconds, including 2,271 browser e2e tests
(`target/gate/1789263191-1159481`).

The real Safari base-URL probe then exposed a navigation ordering bug:
`goto()` returned before its synthesized frame event reached the state
observer, so synchronous `page.url()` was empty. Classic navigation now waits
for that existing event to be observed before returning. A protocol regression
asserts both page and main-frame URLs immediately, using the driver's redirected
URL rather than the requested one. The same real Safari probe passed afterward
in 1.2 seconds (`/tmp/ferridriver-safari-baseurl-vn3rxaoj`). Final verification
with `just ready --only rust-build --only napi --only integration --only format`
passed 108 checks in 168.07 seconds (`target/gate/1789263591-1368061`). The iOS
native workflow passed again at `/tmp/ferridriver-ios-safari-tk3ypvhq`.

Successful Classic navigation now refreshes the shared frame cache, as BiDi
and WebKit already do. Frame-discovery errors propagate instead of leaving a
silently incomplete cache. The native scripting and NAPI regressions assert
that a named child appears after navigation, then disappears and reports
detached after navigating to another document. Both passed on real desktop
Safari. The scoped gate including Rust, NAPI, integration, e2e, BDD and format
completed with exit 0: 110 checks, zero failures, 146.23 seconds
(`target/gate/1789263974-1457930`).

On the booted iOS 26.2 simulator, the same frame assertions passed over HTTP
through Appium, with a 402 ms script body
(`/tmp/ferridriver-ios-frames-8rvncdku`). The equivalent data-URL navigation
timed out after 30 seconds before frame discovery; Appium returned success
for the subsequent session deletion. This remains an unresolved navigation
gap, not a passing iOS data-URL test. The portable HTTP form workflow also
passed again with trusted taps, Promise evaluation and screenshot capture
(`/tmp/ferridriver-ios-safari-0zzbfv0j`, exit 0, 3,158 ms script body):

```bash
python3 scripts/probe-ios-safari.py \
  --device 938B5F8C-C87D-4120-A17D-61163AC1C024 \
  --ssh-command /home/sashoush/Workspace/apple-vms/scripts/macos-ssh
```

Extending the iOS frame probe to read a child-frame locator exposed missing
selector-engine injection in that frame. Classic frame evaluation now injects
into the selected frame before executing selector-engine expressions, matching
the existing WebKit behavior. The unchanged failing probe then passed five
consecutive runs each through native scripting and NAPI. Desktop Safari also
passed the stronger native regression (one test, 1.3 seconds, exit 0).

Repeat the frame workflow with the same device and SSH arguments above, adding
`--scenario frames --runtime script` or `--scenario frames --runtime napi`.
The fixture checks child discovery, locator text, old-frame detachment and
screenshot capture. Its SHA256 is
`74fce27f9502a656bb26abc10529be37356ae3b8c07cfb278a7bb6e3d9fa5fbd`.
Sequential warm-device wall times, including session creation and cleanup,
were 3758.39, 3261.34, 3199.01, 3536.77 and 3102.00 ms for native scripting;
3682.22, 3091.66, 3169.79, 3331.36 and 3364.88 ms for NAPI. Every process
exited 0. These samples establish repeatability, not performance superiority.
The data-URL gap remains: a plain data URL also timed out, the simulator URL
opener rejected it with OSStatus -10814, and Appium's native deep-link command
returned success without changing the current page's location.

Native inspection subsequently found that deep linking had opened a second
tab titled `Native data`. The form tab's `document.visibilityState` was
`hidden`, even after Appium accepted a window switch to it. The unchanged
form workflow then failed twice at native accessibility lookup
(`/tmp/ferridriver-ios-safari-74g4n91g` and `-11dp3nzt`). Closing only the
diagnostic tab through its native tab-overview Close button and selecting the
form tab restored the workflow: exit 0, 2,940 ms script body
(`/tmp/ferridriver-ios-safari-bsbj80b7`). Thus native deep linking can load the
data URL in a new tab; it is not a same-tab navigation replacement. Appium's
inspection-context selection does not establish foreground-tab selection,
which remains an input-routing gap. Frame-probe screenshot checks establish
capture success, not agreement between the visible and inspected tabs.

The final frame-injection state passed
`just ready --only rust-build --only napi --only integration --only e2e --only bdd --only format`:
110 checks, zero failures, exit 0, 619.54 seconds
(`target/gate/1789264615-1668547`). This scoped gate does not cover the native
multi-tab gap above or resolve the pending acceptance screenshot baseline.

`bringToFront()` previously evaluated `window.focus()` and discarded every
error, including activation of a closed page. It now delegates to
`Page.bringToFront` for CDP, `browsingContext.activate` for BiDi and
`Target.activate` on WebKit's page-proxy session. These match Playwright's
`crPage.ts`, `bidiPage.ts` and `wkPage.ts`; the public signature remains
`page.ts::bringToFront(): Promise<void>`. Classic WebDriver selects its window
through the existing serialized session. XCUITest reports typed Unsupported
if the inspected Safari document remains hidden, rather than claiming that
inspection-context selection activated the tab.

`tests/e2e/page-activation.test.ts` reproduced the old closed-page false
success, then passed on all four backend projects. NAPI covers the same API
through `test/browser.test.ts`. Real desktop Safari passed both native tests
(1.2 seconds, exit 0) and an equivalent NAPI workflow. A live iOS fixture
opened two HTTP tabs, identified the background tab by URL and observed
`visibilityState: hidden`; its activation was rejected as expected
(`/tmp/ferridriver-ios-activation-x22juyp7`, exit 0). This establishes honest
failure reporting, not successful background-tab activation on iOS.

The simulator-only Appium Automation session experiment found two additional
constraints. After `mobile: startAutomationSession`, Appium automatically
changed its context. Restoring the context saved before startup selected its
Automation backend, which returned an Automation page handle and successfully
navigated to a visible data-URL document. Without that restoration, commands
continued through atoms and evaluated a different document. However,
`mobile: stopAutomationSession` with `closeAllWindows: true` timed out after
45 seconds. Explicit session deletion succeeded, but the following session
could not finish startup until the isolated Appium server and simulator
Safari process were restarted. This experimental path is not enabled in
ferridriver; lifecycle cleanup must be resolved before adopting it.

Final activation verification used the same scoped `just ready` command:
110 checks, zero failures, exit 0, 178.25 seconds
(`target/gate/1789266024-1890223`), including 2,283 e2e passes. The earlier
attempt failed compilation because the new error reference lacked its crate
qualification; the final gate includes that correction.
After closing the diagnostic tabs and selecting the original simulator tab,
the unchanged native form workflow passed again, including trusted positioned
taps and screenshot capture (`/tmp/ferridriver-ios-safari-ccb911fk`, exit 0,
3,208 ms script body).

History navigation exposed another stale-cache path: desktop Safari restored
the document but `page.frames()` still contained only the main frame.
Successful back, forward and reload now share the existing navigation frame
refresh for Classic WebDriver, BiDi and WebKit. CDP retains its event-driven
frame updates. Both JS bindings delegate through this core implementation.

On iOS, Appium sometimes returned HTTP 200 with the Inspector error
`Missing injected script for given promiseObjectId` while navigation replaced
the callback realm. The callback adapter now preserves that protocol error.
Navigation retries only its read-only document metadata query, within one
deadline; it never replays navigation or user evaluation. Four Rust regressions
cover recovery, unrelated errors, no user-script replay and the shared deadline.
The deadline regression passed ten consecutive runs. Its mock accepts an
incomplete HTTP request only where cancellation deliberately closes the socket;
existing strict request-parser callers still reject incomplete requests.

The shared history fixture checks URLs, restored child-frame locator text,
reload frame discovery, old-child detachment after forward navigation and PNG
capture. Repeat on the booted iOS 26.2 simulator:

```bash
python3 scripts/probe-ios-safari.py \
  --device 938B5F8C-C87D-4120-A17D-61163AC1C024 \
  --ssh-command /home/sashoush/Workspace/apple-vms/scripts/macos-ssh \
  --scenario history --runtime script
# Repeat with --runtime napi for the Node binding.
```

`scripts/fixtures/portable-history-workflow.js` has SHA256
`8bd0b1d535f600ea0bbefb42118eed703fead96134c3b87f5e4ea52b5ef85ccf`.
Five sequential native runs exited 0 in 4739.87, 3899.57, 3902.25, 3903.58
and 3967.61 ms; five NAPI runs exited 0 in 4031.51, 4051.77, 3931.98,
4564.77 and 3940.47 ms. These are warm-device wall times including session
creation and cleanup, not comparative performance claims. Resumption checks
also exited 0 through both bindings: `/tmp/ferridriver-ios-safari-x79p0uqq`
(native, 4058.72 ms wall, 1534 ms script body) and
`/tmp/ferridriver-ios-safari-5kfnpw_n` (NAPI).

The initial regression used a data-URL parent with a srcdoc iframe. Chromium
153.0.8010.36 rendered `chrome-error://chromewebdata/` in that child after
back navigation. Playwright 1.60 reproduced the same failure using the exact
same Chromium executable. Serving the same HTML over HTTP passed through
both libraries. The shared e2e and NAPI regressions therefore use HTTP with
unchanged behavior assertions; the Chromium data-URL case remains unresolved.

Final history verification passed the scoped gate above: 110 checks, zero
failures, exit 0, 135.79 seconds (`target/gate/1789267917-2325951`). The earlier
attempt failed the two Chromium data-URL tests described above. This result
does not resolve the separately pending acceptance screenshot baseline or
establish complete mobile API parity.

Native Safari activation now uses the existing serialized WebDriver session
operation. When the inspected document is hidden, it temporarily assigns a
unique title, opens Safari's tab overview with an address-field drag and
selects the matching native tab. It verifies document visibility afterward.
Both `bringToFront()` and native pointer resolution use this path. The owned
operation restores the title, accessibility label and web context before
releasing its selection lock, including when the caller drops its future.
The public signature remains Playwright `client/page.ts::bringToFront()`;
NAPI and QuickJS continue to delegate to Rust core.

The live workflow exposed a second core defect: `context.pages()` only read
its cached list, while Classic WebDriver has no page-created event pump.
Classic contexts now use the existing discovery path. Refresh adopts pages
by stable handle, excluding handles already owned by another context, instead
of assuming the protocol's list shares the cache's order. It also preserves
the connected WebDriver session's viewport. NAPI and native integration mocks
verify reordered handles, removals and replacement at unchanged page count.
The NAPI test failed on the previous build with two expected pages versus one
returned, then passed with seven assertions after rebuilding.

Repeat the mobile tab workflow with the device and SSH arguments above and
`--scenario tabs --runtime script` or `--scenario tabs --runtime napi`.
Start with the single test tab. The script opens a second tab through a trusted
link click, assigns both tabs the same title, tests explicit and automatic
activation, verifies trusted clicks on visible documents, checks marker
restoration, captures a screenshot and closes its extra tab. Fixture SHA256:
`8db4543b2781b46f9447bfd8b056614bf4eebbd51cee5363dd1d9417818a86bf`.

On iOS 26.2, four native runs passed in 17904.05, 15136.36, 15402.44 and
15031.65 ms wall time; one NAPI run passed in 45957.57 ms. Each verified two
trusted clicks and one remaining tab, with exit 0. Artifact suffixes under
`/tmp/ferridriver-ios-safari-` are `0b1acqve`, `uc3ks7_8`, `d6l4r6_w`,
`qkt8pk29` and `5qdre1r2`, respectively. The NAPI outlier is not explained;
these are functional probes, not equivalent-tool performance comparisons.

The fifth native run failed, exit 1 after 3942.52 ms
(`/tmp/ferridriver-ios-safari-0ccxu6y2`). Appium returned HTTP 500 with
`Missing target for given targetId` during evaluation immediately after
opening the popup. Earlier reconnect probes also timed out while selecting
a tab created by a previous Appium session. User evaluation is not retried.
The upstream `appium-remote-debugger` source at `64e4914` uses one pending
page-selection record and dispatches target-created events using the app ID
without a page ID. That is a routing hypothesis to
test, not yet a confirmed cause or a fixed provider defect. Multi-tab
reliability, other Safari toolbar layouts and background-frame input remain
unfinished. The desktop Safari attempt discovered the popup but failed this
mobile fixture's hidden-document precondition; desktop popup windows can both
remain visible (`/tmp/ferridriver-desktop-tabs-e5el7d4z`, exit 1).

The final scoped gate, including both discovery regressions and seven native
operation tests, exited 0: 110 checks, zero failures, 179.45 seconds
(`target/gate/1789269616-2740429`). An earlier lint run rejected an oversized
test helper; extracting its request assertions fixed it without suppression.
The green gate does not cover the intermittent live Appium target failure
above, and does not resolve the pending acceptance screenshot baseline.
Individual observations and repeat commands are recorded in
[tab verification](ios-safari-tabs-verification-2026-09-13.json).

Recovery required more than restarting Appium. A native-only XCUITest session
closed the extra test tab, but the next Safari session still timed out at
120,000 ms (`/tmp/ferridriver-ios-safari-3h2pp3gb`, exit 1). The driver log
showed socket setup sent with no target-created response, followed by its
216-second page-selection wait. Restarting the simulator's Safari process
and the isolated Appium process then restored the unchanged form workflow:
exit 0, 4,069 ms script body, trusted positioned input and screenshot capture
(`/tmp/ferridriver-ios-safari-8mcwztd2`). The simulator, WDA and unrelated
Appium servers stayed running. Persistent guest debug logs are
`/Users/sashoush/ferridriver-appium.hlHcHv/tabs-routing.KN19ma` (failed recovery)
and `tabs-routing.XRpSj9` (current server and successful recovery).

Before the frame-injection change, `cargo test -p ferridriver --lib` passed 406 tests.
The one separately marked test is an event-dispatch microbenchmark. Running
`cargo test -p ferridriver --lib bench_routed_event_dispatch_wakeups -- --ignored --nocapture`
also passed. Its single debug-build sample measured global broadcast at
9.927761 ms with 16,000 listener wakeups and routed dispatch at 16.190477 ms
with 2,000 wakeups (zero unrelated listener wakeups). This is evidence for
reduced dispatch fan-out, not a latency improvement or a comparison with
another automation tool.

The probe can exercise an existing endpoint without credentials in source:

```bash
FERRIDRIVER_WEBDRIVER_ENDPOINT=http://127.0.0.1:4448 \
  cargo run -p ferridriver --example webdriver_classic_probe
```

Without that environment variable, the example launches the bundled
`safaridriver` locally on macOS. A successful result requires trusted input,
frame and window selection, PNG capture, and session cleanup.

Managed Android Chrome 124 on API 35 currently fails during startup.
The 2026-09-12 trace shows `Inspector.detached`, `Target.detachedFromTarget`,
and `Target.targetDestroyed` for the initial tab, followed by a replacement
target. An outstanding evaluation on the original session timed out instead
of reporting closure. Separate protocol regression coverage also reproduced
an attach event lost before `Target.setAutoAttach` returned.

Repeat the device workflow with transport evidence:

```bash
RUST_LOG='ferridriver::cdp::send=debug,ferridriver::cdp::recv=trace,ferridriver::android=info' \
  python3 scripts/probe-android-appium.py --managed-android --sdk "$ANDROID_HOME"
```

The early failures left no emulator or ADB forward behind. After fixing
detached-session cancellation and startup target adoption, the next run reached
the 180-second launch deadline: the image had Wi-Fi disabled and no saved
network. That timeout also returned before ADB registration and forwarding
disappeared, so its cleanup assertion failed. Later inspection confirmed both
were gone, but delayed cleanup is not a passing result.

The owned launcher now enables Wi-Fi and requests the emulator's documented
[AndroidWifi access point](https://developer.android.com/studio/run/emulator-networking).
Launch failure and timeout also await the owned device's cleanup outside the
launch deadline. Five native script runs and one native test-runner run then
passed the shared workflow, with matching empty before/after device and
forwarding snapshots. Script runs took 27.33–33.18 seconds including startup
and cleanup; the test-runner run took 27.73 seconds. The SDK was already
installed, but each run created a fresh emulator.

A deliberately short 5,000ms launch deadline returned the expected timeout
after 5.37 seconds with cleanup already complete. Commands and individual
results are in `managed-android-verification-2026-09-12.json`. These runs
precede the notification-explainer suppression change and do not establish
coverage of every browser API or macOS Android hosts.

The notification-explainer fix subsequently passed a fresh Android workflow.
A separate native script opened `chrome://version` and confirmed that all
three first-run switches and the custom debugging-socket switch were effective.
ADB reported `POST_NOTIFICATIONS: granted=true`, and a device screenshot showed
Chrome's page without the explainer. Both runs completed cleanup. This follows
Chromium's [notification permission controller](https://github.com/chromium/chromium/blob/376197203665f50f913785b56ba477516998defe/chrome/browser/notifications/android/java/src/org/chromium/chrome/browser/notifications/permissions/NotificationPermissionController.java),
which suppresses the request when the OS permission is already granted.

Two verification attempts also exposed an unrelated 15-second cap on
`adb wait-for-device`. It now shares the overall launch deadline; regular
ADB commands retain their own time limits. Cancellation coverage checks that
an interrupted wait kills its process group.

### iOS Inspector page routing and reconnect

The isolated Appium test installation now carries
`scripts/patches/appium-remote-debugger-17.4.2-page-routing.patch`, generated
against appium-remote-debugger `64e4914bb763be134ec577176118ecda242b04d6`.
It is not automatically installed by ferridriver. The original VM installation
is preserved; the patched server is forwarded to host port 4728.

Two defects have direct reproductions. Overlapping page selection assigned
the first page's target to the second page because Appium kept one pending
selection. Inspector replies contain `WIRDestinationKey`, not a page ID.
The patch assigns each page a sender and routes target creation by that
destination. The same two-tab workload produced one destination before the
patch and two after it. Five native-script and five NAPI runs then passed
consecutively, including trusted input and closing the extra tab. Median wall
times were 15,519.55 ms and 15,217.05 ms respectively, including session
creation and teardown on the booted simulator. The first native run took
44,256.60 ms; this unexplained outlier remains in the recorded results.

Reconnect still failed after that change: the second preserved tab timed out
at context selection after 30 seconds. WebKit's `receivedConnectionDiedMessage`
removes only the first matching target connection, while `receivedSetupMessage`
rejects a target that still has a connection. Appium disconnected its transport
without detaching every selected page. The patch now sends `forwardDidClose`
for each selected page, attempts remaining detaches after a failure, and closes
the transport in a finally block. The identical reconnect fixture subsequently
passed six consecutive runs, three through native scripts and three through
NAPI, retaining both documents and verifying trusted clicks on both.

Protocol reference: the cloned WebKit source at
`73d6001e409c45f30c9a254ebcfbba30a67f57c4`,
`Source/JavaScriptCore/inspector/remote/cocoa/RemoteInspectorCocoa.mm`:
`sendMessageToRemote`, `receivedSetupMessage`, `receivedDidCloseMessage`, and
`receivedConnectionDiedMessage`. Upstream patch build and lint exited zero;
the unit suite passed 390 tests with no failures or skips. Real-device behavior
and other Safari layouts remain unverified.

Repeat the live workflow with the same fixture through either binding:

```bash
python3 scripts/probe-ios-safari.py --endpoint http://127.0.0.1:4728 \
  --device 938B5F8C-C87D-4120-A17D-61163AC1C024 \
  --ssh-command /home/sashoush/Workspace/apple-vms/scripts/macos-ssh \
  --scenario reconnect --runtime script
```

Use `--runtime napi` for NAPI and `--log-protocol` for raw Inspector envelopes.
Start with one owned test tab; a successful run closes its extra tab. The
fixture checks that reconnect preserves document state rather than reloading
pages. Individual measurements, including the failed run, are recorded in
`ios-safari-tabs-verification-2026-09-13.json`. These results establish specific
simulator workflows, not full API parity or a speed comparison with other tools.

To rebuild the test dependency, apply the patch to the pinned clone, then run
`npm install`, `npm run build`, `npm run lint`, `npm test`, and
`npm pack --pack-destination <new-directory>`. Install that tarball into the
isolated XCUITest driver's own package directory using
`npm install --prefix <isolated-root>/node_modules/appium-xcuitest-driver <tarball>`.
A root-level npm override did not replace the nested dependency in this setup;
verify the module resolved from XCUITest contains `_pageTargetsBySenderId` and
the explicit `forwardDidClose` loop before starting Appium. The tested tarball
SHA-256 is `bae54085b01f5de4ae7b62c78d62e7acc2947f95486efb9eccd5dc762fe519eb`.

### Managed iOS dependency provisioning

`BrowserInstaller::install_ios` and `ferridriver install ios` now provision a
private Node 24.21.0, Appium 3.7.0, and XCUITest 12.12.3 installation on macOS.
The installer validates Xcode's first-launch setup and checks CoreSimulator's
available runtimes for the host architecture. If none matches, it requests
an iOS runtime with `xcodebuild -downloadPlatform iOS`. It does not accept
Xcode licenses or install Xcode. Non-macOS hosts return a typed unsupported
error before creating a cache or downloading dependencies.

The Inspector patch is embedded from
`crates/ferridriver/src/install/appium-remote-debugger.patch`. Installation
downloads the pinned upstream source archive, verifies its checksum, applies
the source patch, builds it, and installs the resulting package. The published
XCUITest package lists `appium-remote-debugger` in `bundleDependencies`, so npm
root overrides do not replace its copy. The installer updates the bundled
dependency using XCUITest's explicit npm prefix and verifies the module that
XCUITest actually resolves before marking the cache ready.

The first real VM installation exposed this bundled-dependency issue and
exited 1 without a ready marker. After the correction, the same CLI command
created a separate successful installation while preserving the failed one.
That fresh attempt took 50,851.90 ms; a second invocation returned the same
installed path in 569.08 ms. Both timings include SSH invocation overhead.
Xcode and an iOS 26.2 runtime were already present, so these are dependency
installation measurements, not a fresh-machine setup benchmark. Runtime
download behavior and Apple Silicon hosts still need live verification.

The macOS CLI was built on Linux using the VM's MacOSX26.2 SDK, Clang, and
`ld64.lld`. The stable Rust linker flavor is `gcc` with a Clang wrapper that
passes `--target=x86_64-apple-macos15.0`, `-isysroot <sdk>`, and
`-fuse-ld=lld`. C compilation uses the same wrapper without the linker option.
The repeatable Cargo invocation is:

```bash
SDKROOT=<sdk> MACOSX_DEPLOYMENT_TARGET=15.0 \
CC_x86_64_apple_darwin=<clang-cc-wrapper> AR_x86_64_apple_darwin=llvm-ar \
CARGO_TARGET_X86_64_APPLE_DARWIN_LINKER=<clang-link-wrapper> \
CARGO_TARGET_X86_64_APPLE_DARWIN_RUSTFLAGS='-C linker-flavor=gcc' \
  cargo build --target x86_64-apple-darwin --bin ferridriver
```

The resulting binary ran `install ios` in the VM and then passed the shared
reconnect workflow through the newly installed driver, including preserved
documents, trusted clicks, screenshot capture, and closing the extra tab.
Two further passes of the form, frame, history, tab, and reconnect workflows
all exited zero on that Mac binary. Individual results and installation
measurements are in `managed-ios-verification-2026-09-13.json`.
The final binary also reused the installed cache and passed the form workflow;
its cache check took 2,153.35 ms including SSH overhead. Final scoped
`just ready --only rust-build --only integration --only format` passed
54 checks with no failures in 115.38 seconds, exit 0
(`target/gate/1789274187-3295644`). The broader browser gate before the bundled
dependency correction passed 110 checks in 183.45 seconds, exit 0. The owned
verification server was stopped after its last session returned DELETE 200;
Appium reported no active sessions during shutdown.
`scripts/probe-ios-safari.py --remote-binary <guest-cli>` runs the unchanged
workflow source on the SSH host and retrieves the screenshot. The endpoint
then refers to an address reachable from the guest.

Dependency provisioning is implemented; managed iOS device selection, simulator
ownership, and automatic Appium launch still need to be connected to the
existing browser target configuration. The verification server was started
explicitly. These runs do not prove automatic device launch or full Safari
API coverage.

The managed installer also builds `appium-ios-simulator` 9.1.3 from revision
`75a05126445ad079d9b33fed4cd4a736f3ab3ee5` with a source patch that prevents
headless startup with an explicit device set from killing the global Simulator
UI process. Both stopped and already-booted cases have upstream regression
tests; all 41 upstream tests and lint passed. The installation cache key now
includes the Node version, both dependency revisions, and both source patches.

The Mac VM installed that revision in 62.12 seconds, exit 0. A private-set
device booted, survived a repeated start, and was deleted afterward while the
existing simulator and its UI process stayed running. This does not isolate
Simulator preference writes, which the upstream helper still performs.
`scripts/probe-ios-device-isolation.mjs` makes the state and cleanup checks
repeatable on a Mac with an existing booted simulator:

```bash
<installation>/node/bin/node scripts/probe-ios-device-isolation.mjs \
  <installation> <existing-device-udid> <existing-ui-pid> \
  com.apple.CoreSimulator.SimDeviceType.iPhone-16 \
  com.apple.CoreSimulator.SimRuntime.iOS-26-2
```

A subsequent Safari session on a private-set device failed: Xcode exited 70
because its WebDriverAgent destination lookup could not find that UUID.
Appium's `simulatorDevicesSetPath` capability is therefore insufficient for
this launch path. The failed probe cleaned up its owned device and left the
original simulator booted. The cloned idb source at revision
`a59d7172a3e094706dca92d2815fe3a7ce90df01` handles custom sets by interposing
Xcode's device-set lookup in `Shims/Shimulator/TestReporterShim/XCTestReporterShim.m`;
that mechanism is not part of ferridriver. Measurements and the failed run are
recorded in `ios-device-ownership-verification-2026-09-13.json`.

Creating an owned device in Xcode's standard set passed the next live probe.
With the standard set explicitly passed to Appium, the same startup patch kept
the existing Simulator UI running. WebDriverAgent built into a fresh derived-data
directory, launched on the new device, created a Safari session, and evaluated
the page title and user agent. Session deletion returned HTTP 200; cleanup shut
down and deleted only the returned device UUID. The original simulator remained
booted. The full cold probe took 142.59 seconds, exit 0. This establishes a
workable device-ownership path without requiring Xcode interposition; it is not
yet wired into automatic target launch.

The unchanged `portable-form-workflow.js` then passed on another newly created
standard-set device using the Mac ferridriver binary. It verified filling,
trusted clicks at the requested coordinates, accessibility-label restoration,
promise fulfillment and rejection, rich handle serialization, and a screenshot.
The script body took 4,294 ms, the CLI invocation took 10,888.06 ms, and the
complete cold device/build/session/workflow/cleanup probe took 137,349.07 ms.
All exited zero. The screenshot was inspected; cleanup removed the owned device
and left the existing device and Simulator UI running.

Managed iOS launch now uses the shared browser target configuration:
`device: { platform: "ios", model: "iPhone 16" }` selects Safari and WebDriver.
Rust selects a compatible installed runtime, creates an owned simulator in
Xcode's standard set, starts the private Appium installation, and connects the
existing WebDriver backend. The same page and locator script bindings are used.
The owner retains the device UUID across cancellation and deletes that device
during teardown. `headless: false` additionally requests the Simulator UI.
An already-running Simulator application may reuse its existing process and
automatically display booted devices in the standard set, including devices
started with `headless: true`. The flag suppresses ferridriver's UI launch; it
does not hide devices from an existing Apple Simulator window. Appium's
upstream simulator helper still writes shared Simulator preferences; complete
GUI isolation is not established.

Testing a completely fresh session exposed a failure that the earlier
preinitialized-session probe missed. Safari 26's toolbar tutorial consumed the
first tap. The page received trusted touch events on the correct button but no
click. Reversing normal and forced clicks, removing input focus, and adding a
press duration did not fix it. A native screenshot taken before the first tap
revealed the tutorial. Managed startup now passes Safari's existing
`WBSOnboardingStatesDefaultsKeyV0.2: { TipForMoreButton: 3 }` preference through
Appium's `safariGlobalPreferences` capability before opening Safari. This applies
only to ferridriver-owned simulators.

The unchanged form workflow then passed on a fresh iPhone 16 with iOS 26.2:
130,413.16 ms cold launch/workflow/cleanup, 3,817 ms script body, exit 0.
This is one successful sample, not a latency distribution or full API proof.
`managed-ios-launch-verification-2026-09-13.json` retains the failed probes and
the successful result. Five follow-up runs also passed: two headless iPhone 16
runs, one headed iPhone 16, automatic model selection, and iPad mini (6th
generation). Each preserved the pre-existing device and Simulator UI inventory.
The first repetition overlapped the Linux gate, so these mixed-mode cold timings
are not a controlled latency distribution.
The probe now compares device UUID/name/state and Simulator UI process inventories
before and after every managed run, and fails if teardown changes them.

```bash
python3 scripts/probe-ios-safari.py --managed --model 'iPhone 16' --headless \
  --ssh-command /path/to/macos-ssh --remote-binary /path/on/mac/ferridriver \
  --remote-cache /path/on/mac/browser-cache --diagnostics
```

The scoped gate for that state passed 110 checks, zero failed or blocked, in 197.04
seconds, exit 0 (`target/gate/1789281722-3736619`):

```bash
just ready --only rust-build --only napi --only integration --only e2e \
  --only bdd --only format
```

Startup interruption then exposed a detached-build cleanup gap. One-second and
five-second launch deadlines returned timeout errors with the original device,
UI, and build-process inventories intact. At 50 seconds, the simulator was
removed but `xcodebuild` remained running outside Appium's process group.
WebDriverAgent explicitly requests both detached spawning and detachment at
startup. The private dependency build now makes those choices conditional on
`APPIUM_WDA_INHERIT_PROCESS_GROUP`, which Rust enables for owned launches.
The regression failed before the patch; all 66 upstream unit tests and lint
passed afterward. Both repeated 50-second interruption probes passed on the VM.
Live process observations confirmed that `xcodebuild` inherited Appium's owned
process group, and the pre-existing device, UI, and build-process inventories
were restored before each probe returned. The normal headed form workflow also
passed with that dependency. The probe now checks the `xcodebuild` process
inventory as well as devices and UI.

The final Mac binary, SHA-256
`f61a63acb71a92f81e7cc84e9f24f3dec46b914492784779a7017e279d971944`,
reused the verified installation and passed the unchanged headless form workflow
with cleanup verification. The final scoped `just ready` passed 110 checks,
zero failed or blocked, in 215.86 seconds, exit 0
(`target/gate/1789283444-3965457`). These runs establish the managed-launch and
startup-cancellation milestone, not complete Safari API parity or performance
superiority over another tool.

The native test-runner probe (`scripts/probe-ios-safari.py --runtime test`)
then exposed a different startup boundary. A fresh iPhone took longer than
the test's 30-second body budget; cancellation abandoned lazy browser setup
and returned before device cleanup completed. The runner now resolves required
worker prerequisites through the existing fixture graph before starting the
body budget. Browser startup keeps its own launch deadline. `BrowserHandle`
retains an in-flight launch task and awaits it during teardown, including when
the fixture caller was cancelled. Auto-fixture setup errors also fail the test.

A local Classic WebDriver regression allows a 1.5-second launch with a 500 ms
test budget, still rejects a hung body, and observes session deletion after
both runs. The updated Mac build reached page setup after device startup and
restored the original simulator, UI, and build-process inventories. The actual
test workflow still failed: Appium's normal Safari backend does not implement
`POST /window/new`, which `context.newPage()` needs. This is an open driver gap,
not a passing native iOS test milestone. Commands, failures, and cleanup evidence
are recorded in `ios-test-runner-verification-2026-09-13.json`.

The scoped gate also caught 67 BDD failures caused by the public example site
returning an HTML error page headed `Error 1200`. Those scenarios now use the
repository fixture server's `example-domain.html` and `domain-info.html`.
Their content and element-count assertions remain intact; exact URL and link
destination assertions now name the local resources. Navigation, assertions,
and interaction passed all 40 targeted cases across the four backend projects.
Other existing BDD scenarios still contact external sites.

The final scoped gate passed 110 checks, zero failed or blocked, in 122.83
seconds, exit 0 (`target/gate/1789286006-217504`), including 2,287 e2e,
826 integration, and 637 BDD passes. This gate does not include the protected
acceptance snapshot baseline awaiting approval, and does not turn the separate
real iOS `context.newPage()` failure into a pass.

The subsequent pinned XCUITest build implements `createNewWindow` through its
existing web execution backend. The normal Safari backend opens a tab using a
scoped WebKit user gesture, identifies it by a unique temporary name, clears
the name and opener, and restores the original selected window. A failed
identification closes the owned tab. Normal script evaluation keeps its prior
gesture behavior. The installer builds the patched source before packing it;
local Appium registration links that compiled package without rerunning a
prepare script whose TypeScript configuration is absent from the tarball.

The first real run caught a result-envelope mismatch missed by the initial
driver mocks. The helpers now return the remote-debugger value envelope, with
a regression exercising its actual converter. The remote-debugger suite passed
369 tests and XCUITest passed 510. The native iOS test runner then passed the
shared form workflow in 143.91 seconds and the repeated new-page workflow in
139.89 seconds, both exit 0 with simulator, UI-process, and build-process cleanup
verified. The latter created and closed two tabs with trusted clicks. Its
unchanged script also passed CDP pipe, CDP WebSocket, BiDi, and WebKit desktop
runs. These results close the specific `context.newPage()` gap recorded above
for the private driver build, not for an unpatched Appium installation. Details
are in `ios-new-window-verification-2026-09-13.json`; full API parity remains
unproven.

The final scoped `just ready` passed 110 checks, zero failed or blocked, in
153.79 seconds, exit 0 (`target/gate/1789288304-655166`), with 2,287 e2e,
826 integration, and 637 BDD passes. An earlier gate crashed in a linked NAPI
library containing a zeroed machine-code region. The standard rebuild and 259
targeted browser/WebDriver tests passed; three repeat links were identical and
contained no such region. This establishes recovery but leaves the original
artifact-corruption cause undetermined. The protected acceptance snapshot
baseline is still outside this gate and awaits approval.

The pinned Inspector navigation implementation swallowed RPC failures and
always assigned `window.location.href`. It now preserves numeric protocol error
codes, attempts `Page.navigate`, and uses script navigation only when Inspector
returns method-not-found (`-32601`). Transport failures and other protocol
errors reject the navigation and remove its readiness listeners. The fallback
accounts for the removal documented in
[Appium issue 21976](https://github.com/appium/appium/issues/21976); it is covered
by protocol tests, not a live iOS 26.4 run. The upstream suite passed 397 tests
and lint. The private installer verifies that XCUITest resolves this implementation.

Real iOS 26.2 HTTP navigation, form input, Promise evaluation, screenshot capture,
and cleanup passed with the updated driver in 132.63 seconds, exit 0. The separate
managed history workflow passed back, reload, forward, and frame-tree assertions
before this change. Data-URL navigation still fails: both builds exceeded the
same 10-second deadline. Driver tracing shows `Page.navigate` acknowledged in
5 ms, followed by unchanged page listings and no load event. This does not prove
data-URL support, even though the unchanged data/frame/history script passes
the four desktop backends. All three failed iOS data probes verified cleanup.
Commands and results are in `ios-navigation-verification-2026-09-13.json`.

The final scoped gate passed 110 checks, zero failed or blocked, in 168.49
seconds, exit 0 (`target/gate/1789288964-871045`). The data-URL failure remains
outside that passing gate and blocks a complete mobile navigation claim.

Two Automation routing defects are fixed in the private XCUITest build. Startup
restores the inspection-context anchor if tab discovery changed it; later page
listings still update discovery without replacing the active Automation backend.
Both regressions failed before the fixes. The XCUITest suite passed 512 tests
and lint, and the final scoped gate passed 110 checks, zero failed or blocked,
in 156.74 seconds, exit 0 (`target/gate/1789290628-1306197`).

A protocol probe ran two Automation sessions on the same owned simulator and
Appium server. Both loaded a data URL, evaluated its document, captured a
screenshot, detached without closing all windows, and deleted their WebDriver
session. Final device deletion preserved the original device inventory. This
establishes a usable navigation path, but not remote cleanup: detached tabs
remain until the owned simulator is deleted.

The stronger probe found incomplete click completion before it could test extra
tab closure. A missing startup preference initially left Safari's toolbar
tutorial over the page. With the same onboarding preference as managed launch,
the click assertion still failed immediately, while the subsequent diagnostic
read observed the trusted click. The assertion remains unchanged. Rust already
maps XCUITest input to native coordinates, whereas Appium's atoms and Automation
backends send actions through different input services. That routing and the
independent Automation window/context identifiers need resolution before enabling
this path automatically. Detailed commands, failures, screenshots and cleanup
evidence are in `ios-automation-lifecycle-verification-2026-09-13.json`.

XCUITest actions now enter the native context under the existing Rust session
selection lock and restore the original web target afterward. A cancelled
queued operation previously still sent `/actions`; a regression reproduces
that request, and caller cancellation now prevents dispatch before selection.
In-flight cancellation retains context and marker restoration. Native-operation
tests, NAPI and QuickJS input tests, managed iOS Safari keyboard and tab workflows,
and the same keyboard workflow on all four desktop backends passed. The scoped
gate passed 111 checks with exit code 0. Commands, measured durations, cleanup
checks and remaining limits are recorded in
`ios-native-input-verification-2026-09-13.json`.

Classic WebDriver commands now use the same cancellation-aware selection lock
as native mobile operations. A regression caught an ordinary queued `/actions`
request still reaching the server after cancellation. Both cases passed ten
consecutive runs after sharing the lock, and the updated macOS binary passed
the managed Safari keyboard workflow with cleanup. The new scoped gate passed
111 checks with exit code 0; the native input record includes its exact command.

Further iOS probes rule out enabling the experimental Automation path with
native WDA input: Safari treats that input as an interruption and displays its
automation-control overlay. Automation can create and close an additional tab,
but its touch command still returns before the trusted DOM click. Native deep
links load data URLs in the original tab but fail to navigate a second tab even
after explicit activation. These failures and their cleanup results are retained
in the Automation lifecycle and navigation verification records.
