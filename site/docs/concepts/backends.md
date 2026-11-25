# Backends

ferridriver has five backends, one per protocol, behind one API. Every
browser runs through one of them; none has a backend of its own. They all
speak the same `Browser` / `Page` / `Locator` surface, so a test moves
between browsers by changing its project.

## At a glance

| Backend     | Protocol                    | Browsers                            | When to use |
|-------------|-----------------------------|-------------------------------------|-------------|
| `cdp-pipe` (default) | CDP over pipes     | Chromium                            | Most things. Fastest. |
| `cdp-raw`   | CDP over WebSocket          | Chromium, Lightpanda                | Attach to a running Chrome; Lightpanda |
| `webkit`    | Playwright WebKit Inspector | Playwright WebKit                   | Safari-family rendering coverage |
| `bidi`      | WebDriver BiDi              | Firefox, Safari, Ladybird, Chromium | Firefox, Ladybird, BiDi interop |
| `webdriver` | Classic WebDriver           | Safari, Ladybird, any W3C driver    | Real Safari, Selenium grids, Appium |

## Browsers

| Browser           | Default backend | Others                  | Platforms             |
|-------------------|-----------------|-------------------------|-----------------------|
| `chromium`        | `cdp-pipe`      | `cdp-raw`, `bidi`, `webdriver` (connect) | Linux, macOS, Windows |
| `firefox`         | `bidi`          | `webdriver` (connect)   | Linux, macOS, Windows |
| `webkit`          | `webkit`        |                         | Linux, macOS, Windows |
| `safari`          | `webdriver`     | `bidi`                  | macOS |
| `ladybird`        | `bidi`          | `webdriver`             | Linux, macOS |
| `lightpanda`      | `cdp-raw`       |                         | Linux, macOS |

A backend a browser lacks a protocol command for answers with a typed
`Unsupported` error. Where the same session serves another protocol that
has the feature, the backend uses it: a `BiDi` session created through
Classic WebDriver reads cookies, sets file inputs and dispatches input
through Classic when the browser has no `BiDi` command for them. A browser
with no `BiDi` user contexts has one browsing context per session, and the
test runner reuses it across tests, starting a fresh browser after a test
fails in it. A test that times out gets the worker a fresh browser on every
backend, since it may still be acting on the one it had.

## `cdp-pipe` — the default

Chromium via CDP over Unix pipes (file descriptors 3 and 4). ferridriver
launches Chrome itself and wires stdin/stdout-like pipes for the
protocol.

**Strengths**

- Lowest-latency transport — no WebSocket framing, no TCP loopback.
- Deterministic process lifecycle: when the test ends the pipe closes
  and Chrome exits.
- Fully parallel across tests on the same worker (each worker has its
  own Chrome).

**Limits**

- You must let ferridriver launch Chrome. It cannot attach to a running
  instance.
- The remote-debugging-port flag is ignored; the pipe is the only
  channel.

Pick this unless you have a specific reason not to.

## `cdp-raw` — attach to a running Chrome

Same CDP protocol over a WebSocket. ferridriver can launch Chrome and
dial its `ws://...` endpoint, or you can point it at an existing browser.

```rust
use ferridriver::browser_type::{chromium, BrowserType};
use ferridriver::options::{BrowserTypeOptions, ChromiumTransport};

// launch
let browser = BrowserType::chromium_with(&BrowserTypeOptions {
    transport: Some(ChromiumTransport::Ws),
    ..Default::default()
}).launch(Default::default()).await?;

// attach
let browser = chromium()
    .connect("ws://localhost:9222/devtools/browser/abcd-1234", Default::default())
    .await?;
```

**Use when**

- You want to automate a persistent Chrome profile started outside the
  test runner.
- Your environment does not allow inherited file descriptors (some
  container setups, some CI runners).
- You want to share one browser across several runners or languages.

Slightly slower than `cdp-pipe` — WebSocket framing plus a loopback hop
— but the difference is in the low single-digit ms per CDP call and is
usually irrelevant.

## `webkit` — Playwright WebKit (Linux + macOS)

Speaks Playwright's WebKit Inspector protocol over a NUL-byte-delimited
JSON pipe to a `pw_run.sh` child process. Same code on Linux and macOS.

**Strengths**

- Real WebKit / JavaScriptCore rendering and JS engine.
- Cross-platform — Linux and macOS (the `--inspector-pipe` transport
  relies on a Unix fork/dup model, so there is no Windows support).
- Closes most Playwright feature gaps that a public-API WKWebView build
  cannot (PDF, video, network interception, per-frame execution context,
  main-document response, etc.).

**Limits**

- Requires the Playwright WebKit binary. Download it with
  `ferridriver install webkit`, or provide it via `FERRIDRIVER_WEBKIT`
  or `npx playwright install webkit`.
- A few hundred MB on disk per platform.

**Raw protocol access**

`context.new_webkit_session(&page)` is the WebKit counterpart of
`new_cdp_session`: a raw WebKit Inspector Protocol session on the page's
main target, for domains Playwright does not wrap, such as `LayerTree`.
Playwright keeps the same session private, as `WKPage._session`.

```rust
let session = context.new_webkit_session(&page)?;
session.send("LayerTree.enable", json!({})).await?;
let document = session.send("DOM.getDocument", json!({})).await?;
let layers = session
  .send("LayerTree.layersForNode", json!({ "nodeId": document["root"]["nodeId"] }))
  .await?;
```

`send` always reaches the target that is live at the call, so it follows a
cross-process navigation. `session.events()` streams the live target's
events in wire order, and ends when that target goes away; take another
after a cross-process navigation. Once the page closes, `send` is refused
and its events have ended.

## `bidi` — Firefox via WebDriver BiDi

Native Firefox driver. ferridriver launches `firefox` with a fresh
profile and speaks **WebDriver BiDi** (not Marionette, not CDP) over a
WebSocket.

**Strengths**

- Real Firefox coverage — rendering, JS engine, HTTP stack are Gecko's.
- Standards-based: the same BiDi protocol Playwright, Selenium 4, and
  Puppeteer can drive.

**Limits**

- Firefox must be installed — ferridriver does not bundle it. Set
  `executable_path` if it is not on `PATH`.
- HAR recording and download interception return
  `FerriError::Unsupported` (BiDi protocol gap).

## `ladybird` — Ladybird over WebDriver BiDi

ferridriver starts the `WebDriver` service Ladybird ships beside its
browser, creates a session with `webSocketUrl: true`, and drives the
`BiDi` socket it returns through the `bidi` backend; `webdriver` drives the
same session over Classic instead. `ferridriver install ladybird` fetches
the official build, or point `LADYBIRD_PATH` / `executablePath` at the
`Ladybird` executable of any build that includes `WebDriver`.

**Limits**

- One browsing context per session (no `BiDi` user contexts), so
  `browser.newContext()` is unavailable and a test reuses the default
  context.
- No browser arguments or launch proxy: the `WebDriver` service builds
  Ladybird's command line itself.
- Ladybird is in alpha. A protocol command it has not implemented
  (emulation, permissions, dialog events, sandbox realms) answers with
  `Unsupported` or, for sandboxes, runs the script in the page's realm.

## `lightpanda` — Lightpanda over CDP

Lightpanda is a headless browser built for automation that serves CDP
itself. ferridriver runs `lightpanda serve` on a free loopback port and
drives it through the `cdp-raw` backend. Launch arguments are passed to
`lightpanda serve` (`--obey-robots`, `--log-level`, ...); `proxy` maps to
`--http-proxy`, and a `userDataDir` keeps cookies across launches (the only
state Lightpanda persists). Lightpanda's usage telemetry is switched off
unless the launch's own `env` sets `LIGHTPANDA_DISABLE_TELEMETRY`.

Lightpanda holds one browser context per CDP connection, so every context
ferridriver creates gets a connection of its own to the same server, and
the server's connection cap is raised to 128 (`--cdp-max-connections` in
the launch arguments overrides it). `serve` fetches no sub-resources by
default; ferridriver has it load iframes, workers, stylesheets, images and
beacons as a browser does, unless the launch arguments name
`--load-resources` themselves.

**Limits**

- Headless only. Screenshots are PNG only and drawn by Lightpanda's own
  layout, so pixel comparisons against other browsers will differ; there
  is no screencast, so no video.
- Not yet implemented by Lightpanda: popups that open a new window,
  JavaScript dialogs, file choosers, downloads, colour-scheme and device
  emulation, offline and throttling emulation.
- No Windows build; run it in WSL.

## How browsers relate to backends

Each browser has a default backend, chosen by the factory you call
(`chromium()`, `firefox()`, `webkit()`, `safari()`, `ladybird()`,
`lightpanda()`) or by `[[test.projects]]` config; the table under
[Browsers](#browsers) lists them.

Chromium also supports `cdp-raw` (`chromium({ transport: 'ws' })` in JS,
`BrowserType::chromium_with` in Rust). Pairings like Firefox-on-webkit or
WebKit-on-bidi are not valid.

## Cross-backend matrix

```toml
# ferridriver.toml
[[test.projects]]
name = "chromium"
[test.projects.browser]
browser = "chromium"

[[test.projects]]
name = "firefox"
[test.projects.browser]
browser = "firefox"
backend = "bidi"

[[test.projects]]
name = "webkit"
[test.projects.browser]
browser = "webkit"
backend = "webkit"
```

Then `--project firefox` runs only that slice. CI typically runs all
three in parallel shards.

## Performance notes

Rough CDP round-trip budget per action on a warm machine:

| Backend     | Per-action RTT (p50) | Notes |
|-------------|----------------------|-------|
| `cdp-pipe`  | ~0.7 ms              | fd reads, no framing |
| `cdp-raw`   | ~1.2 ms              | WebSocket over loopback |
| `webkit`    | ~1.0 ms              | NUL-delimited JSON over pipe |
| `bidi`      | ~1.5 ms              | BiDi event loop is slightly chattier |

These are transport costs, not test costs. An assertion polling at
100/250/500/1000 ms intervals is dominated by the interval, not the
transport.

## Picking in practice

- **Writing a new suite today**: start on `cdp-pipe`. Add `webkit` and
  `bidi` projects when you need cross-browser coverage.
- **Debugging a single test**: `--backend cdp-pipe` with `--workers 1` —
  one browser, linear output; add `--headed` to watch it.
- **Attaching to a logged-in profile**: `cdp-raw` with
  `chromium().connect(...)`.
- **Safari-family regression**: `webkit`.
- **Firefox regression**: `bidi`.
