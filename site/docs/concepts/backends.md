# Backends

ferridriver has four browser backends behind one API. They all speak the
same `Browser` / `Page` / `Locator` surface — switch with a single flag
and every test keeps working.

## At a glance

| Backend     | Browser            | When to use                                           | Platform |
|-------------|--------------------|-------------------------------------------------------|----------|
| `cdp-pipe` (default) | Chromium / Chrome | Most things. Fastest.                          | Linux, macOS |
| `cdp-raw`   | Chromium / Chrome  | Attach to an already-running Chrome, or when fd inheritance is awkward | Linux, macOS |
| `webkit`    | Playwright WebKit  | Safari-family rendering coverage                       | Linux, macOS |
| `bidi`      | Firefox            | Firefox coverage, BiDi interop                         | Linux, macOS |

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

## How browsers relate to backends

Each browser has a sensible default backend, chosen by the Rust factory
you call (`chromium()` / `firefox()` / `webkit()`) or by `[[test.projects]]`
config:

| Browser    | Default backend |
|------------|-----------------|
| `chromium` | `cdp-pipe`      |
| `firefox`  | `bidi`          |
| `webkit`   | `webkit`        |

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
  one browser, linear output. (MCP / `run` launch headed by default.)
- **Attaching to a logged-in profile**: `cdp-raw` with
  `chromium().connect(...)`.
- **Safari-family regression**: `webkit`.
- **Firefox regression**: `bidi`.
