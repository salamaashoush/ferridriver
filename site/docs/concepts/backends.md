# Backends

ferridriver has four browser backends behind one API. All of them speak the same `Browser` / `Page` / `Locator` surface — you switch backends with a single flag and every test keeps working. This page helps you pick one.

## TL;DR

| Backend | Browser | When to use | Platform |
|---|---|---|---|
| `cdp-pipe` (default) | Chromium / Chrome | Most things. Fastest. | macOS, Linux, Windows |
| `cdp-raw` | Chromium / Chrome | Attach to an already-running Chrome, or when you can't inherit fds | macOS, Linux, Windows |
| `webkit` | WebKit / WKWebView | Safari-like behavior, native a11y tree, no Chrome download | macOS 11+ only |
| `bidi` | Firefox | Firefox coverage, `BiDi` interop | macOS, Linux, Windows |

## `cdp-pipe` — the default

Chromium via CDP over Unix pipes (file descriptors 3 and 4). ferridriver launches Chrome itself and wires stdin/stdout-like pipes for the protocol.

**Strengths:**
- Lowest-latency transport — no WebSocket frame overhead, no TCP.
- Deterministic process lifecycle: when your test ends, the pipe closes and Chrome exits.
- Fully parallel across tests on the same worker (each worker gets its own Chrome).

**Limits:**
- You must let ferridriver launch Chrome. It can't attach to a running instance.
- Chrome's remote debugging port flag is ignored; the pipe is the only channel.

**Pick this unless you have a specific reason not to.**

## `cdp-raw` — attach to a running Chrome

Same CDP protocol, but over a WebSocket. ferridriver can either launch Chrome and dial its `ws://...` endpoint, or you can point it at an existing browser.

```rust
// launch
let b = Browser::launch(LaunchOptions { backend: BackendKind::CdpRaw, ..Default::default() }).await?;

// attach
let b = Browser::connect("ws://localhost:9222/devtools/browser/abcd-1234").await?;
```

**Use when:**
- You want to automate a **persistent** Chrome profile started outside the test runner.
- Your environment doesn't allow inherited file descriptors (some container setups, some CI runners).
- You want to share one browser across several runners / languages.

Slightly slower than `cdp-pipe` — WebSocket framing and an OS-level loopback hop — but the difference is in the low single-digit ms per CDP call and usually irrelevant.

## `webkit` — native macOS WKWebView

This backend does **not** use CDP. ferridriver spawns a small Objective-C subprocess (`fd_webkit_host`) that drives a WKWebView via native Cocoa APIs and talks to Rust over IPC.

**Strengths:**
- **Instant startup.** No browser download; WKWebView is already on every Mac.
- **Native accessibility tree** — `snapshot`, `getByRole`, and `toMatchAriaSnapshot` use the same tree VoiceOver sees.
- Native mouse events — `mouse.click` / `click_at` fire through AppKit, not synthesized DOM events.

**Limits:**
- **macOS 11+ only.** Zero support on Linux or Windows.
- **Headless only** today; a headful mode is in progress.
- Smaller feature surface than Chromium — some Playwright features aren't implemented yet (network interception is more limited, CDP tracing isn't available).
- Ships as an extra binary inside the `@ferridriver/node-darwin-arm64` package.

**Use when:** you want Safari-equivalent rendering, need the native accessibility tree for AI/a11y workflows, or can't install Chromium on your CI runner.

## `bidi` — Firefox via WebDriver BiDi

Native Firefox driver. ferridriver launches `firefox --remote-debugging-port=PORT --wait-for-browser`, writes a fresh profile with test-appropriate preferences, and speaks **WebDriver BiDi** (not Marionette, not CDP) over a WebSocket.

**Strengths:**
- Real Firefox coverage — rendering, JS engine, HTTP stack are Gecko's.
- Standards-based: same BiDi protocol Playwright, Selenium 4, and Puppeteer can drive.

**Limits:**
- Firefox must be installed — ferridriver does not download it. Set `executable_path` if it's not on `PATH`.
- Feature completeness is lower than the CDP backends. Some Page methods — notably newer CDP-specific screenshots / tracing — return a clear "not supported on Bidi" error.

**Use when:** you need cross-browser coverage, specifically Firefox.

## How backends relate to `--browser`

The `--browser` flag is a higher-level selector. It sets a sensible default backend:

| `--browser` | Default backend |
|---|---|
| `chromium` | `cdp-pipe` |
| `firefox` | `bidi` |
| `webkit` | `webkit` |

You can mix — `--browser chromium --backend cdp-raw` is valid. `--browser firefox --backend webkit` is not.

## Cross-backend matrix testing

Run the same test suite against multiple backends:

```ts
// ferridriver.config.ts
import { defineConfig } from '@ferridriver/test/config';

export default defineConfig({
  projects: [
    { name: 'chromium', use: { browser: 'chromium' } },
    { name: 'firefox',  use: { browser: 'firefox',  backend: 'bidi' } },
    { name: 'webkit',   use: { browser: 'webkit',   backend: 'webkit' } },
  ],
});
```

Then `--project firefox` runs only that slice. CI typically runs all three in parallel shards.

## Performance notes

Rough CDP round-trip budget per action on a warm machine:

| Backend | Per-action CDP RTT (p50) | Notes |
|---|---|---|
| `cdp-pipe` | ~0.7 ms | fd reads, no framing |
| `cdp-raw` | ~1.2 ms | WebSocket over loopback |
| `webkit` | ~0.8 ms | native IPC, fewer protocol round-trips per action |
| `bidi` | ~1.5 ms | BiDi event loop is slightly chattier than CDP |

These numbers are *transport cost*, not test cost. An assertion that polls `to_be_visible` at 100 ms / 250 ms / ... intervals is dominated by the interval, not the transport.

## Picking in practice

- **Writing a new suite today:** start on `cdp-pipe`. Add `webkit` and `bidi` projects when you need cross-browser coverage.
- **Debugging a single test:** `--headed --backend cdp-pipe -j 1` — one browser, visible, linear output.
- **Attaching to a logged-in profile:** `cdp-raw` with `Browser::connect`.
- **A11y / LLM-driven automation on macOS:** `webkit`. The accessibility tree is the biggest win.
- **Firefox regression:** `bidi`.
