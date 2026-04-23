# Handover — next Playwright-parity session

Read-first for any session continuing work. Overwrite this file with a
fresh summary at the end of each block.

## Cross-device setup

1. `CLAUDE.md` — rules + lessons.
2. `PLAYWRIGHT_COMPAT.md` — gap tracker. Tier 1 done. §3.1, §3.12,
   §2.9, §2.11, §2.10, §2.12, §2.13, §2.14, §4.1 (18/28 fields), and
   now §2.15 landed.
3. This file — block summary below.
4. `docs/NEXT_SESSION.md` — next-block brief + prompt.

`git clone https://github.com/microsoft/playwright /tmp/playwright` if missing.

## Landed this session

### Single commit — `feat: BrowserType class (§2.15)`

`Browser::launch` / `Browser::connect` are gone. The Playwright-shaped
`BrowserType` factory is the sole entry point in all three layers
(Rust core / NAPI / QuickJS). Three top-level factories — `chromium`,
`firefox`, `webkit` — mirror Playwright's
`import { chromium, firefox, webkit } from 'playwright'`.

#### Public surface (matches Playwright verbatim)

```rust
use ferridriver::{chromium, firefox, webkit};
use ferridriver::options::{LaunchOptions, ConnectOverCdpOptions, LaunchPersistentContextOptions};

let browser = chromium().launch(LaunchOptions::default()).await?;
let firefox_browser = firefox().launch(LaunchOptions::default()).await?;
let attached = chromium()
  .connect_over_cdp("ws://127.0.0.1:9222/...", ConnectOverCdpOptions::default())
  .await?;
let persistent = chromium()
  .launch_persistent_context(Path::new("/tmp/profile"), LaunchPersistentContextOptions::default())
  .await?;
```

`chromium({ transport: 'ws' })` (NAPI/QuickJS) and
`BrowserType::chromium_with(BrowserTypeOptions { transport: Some(Ws) })`
(Rust) drive the CDP-WebSocket transport instead of the pipe default —
ferridriver's only deviation from Playwright's pipe-only `chromium`,
required to keep the `cdp-raw` backend coverage matrix usable.

#### Public `LaunchOptions` is now Playwright-shaped

Dropped: `backend`, `browser`, `viewport`, `user_data_dir`,
`ws_endpoint`, `auto_connect`. Those are now internal — the
`crate::options::LaunchPlan` struct (consumed only by
`BrowserState::with_plan`) carries them. Kept (Playwright fields):
`headless`, `executable_path`, `args`, `channel`, `env`, `slow_mo`,
`timeout`, `downloads_path`, `ignore_default_args`,
`handle_sighup`, `handle_sigint`, `handle_sigterm`,
`chromium_sandbox`, `firefox_user_prefs`, `proxy`, `traces_dir`.

Viewport now lives where Playwright puts it: on
`BrowserContextOptions::viewport`. `recorder.rs` was the only caller
that used to pass viewport via `LaunchOptions`; it now constructs a
`BrowserContextOptions` and passes it to `browser.new_context(...)`.

#### Persistent-context wiring

- `BrowserState::persistent_context: bool` set by
  `launch_persistent_context`. `ContextRef::close` reads it and calls
  `state.shutdown()` so closing the persistent default context
  terminates the underlying browser too — Playwright's contract at
  `types.d.ts:15199`.
- `CdpBrowser::launch_with_flags_in_dir` (pipe + ws) accepts a
  borrowed `&Path` for `--user-data-dir` so the dir survives across
  re-launches. `state.rs::ensure_instance` switches between the
  TempDir and explicit-path variants based on `BrowserState.user_data_dir`.

#### Migration scope

| layer | files | sites |
|---|---|---|
| Rust tests / runners / MCP / codegen | ~30 | every `Browser::launch` / `Browser::connect` site |
| TypeScript bun tests | ~26 | every `Browser.launch({ backend })` site |
| MCP server | 2 (`server.rs`, `config.rs`) | `BrowserState::with_options` -> `with_plan` |
| Test runner / fixture | 2 (`runner.rs`, `fixture.rs`) | `build_launch_options` -> `build_launch_plan` |

Bun tests gained a tiny `_helpers.ts` with `launchForBackend(backend)`
that maps `cdp-pipe`→`chromium().launch()`,
`cdp-raw`→`chromium({transport:'ws'}).launch()`,
`bidi`→`firefox().launch()`, `webkit`→`webkit().launch()` so the
existing per-backend test matrix keeps working unchanged.

### Baseline (must stay green)

```
cargo clippy --workspace --all-targets -- -D warnings            # clean
cargo test -p ferridriver --lib                                   # 125 pass
cargo test -p ferridriver-script --lib                            # 13 pass
cargo test -p ferridriver-mcp --lib                               # 38 pass
cd crates/ferridriver-node && bun run build:debug
cd <repo root> && bun test                                        # 859 pass
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1
# cdp-pipe 164, cdp-raw 164, bidi 159, webkit 161  (+5 §2.15 tests on each chromium-capable backend)
```

CI note: a stale-Chrome-process leak from earlier failed runs can
bind random high ports (e.g. 65531) and make
`navigation_response::test_goto_network_failure` falsely pass on a
"refused" goto. Kill leftover `chrome-headless-shell` processes
between runs if you hit that.

## Next priorities

See `docs/NEXT_SESSION.md` for the full next-session prompt.

Top picks (in rough order of unblock-value):

1. **§2.6 HAR recording** — unblocks `BrowserContextOptions::recordHar`
   (one of the four §4.1 deferred fields).
2. **§2.3 Tracing** — unblocks `context.tracing` (§4.5).
3. **§4.1 closing fields**: `recordHar`, `clientCertificates`,
   `httpCredentials.send`, `strictSelectors` — each its own session.
4. **§3.17 Auto-waiting deadline parity** — small surface, fully
   independent.
5. **`launchServer` / BrowserServer protocol** — ferridriver has no
   equivalent today; needed only for distributed-test workflows.

## Carried-forward backend gaps (real protocol limits)

- **BiDi**: response body unavailable for non-intercepted responses;
  multi-`Set-Cookie` collapses; `request.postData()` null for
  fetch-with-body; `Download.cancel` typed `Unsupported`; spurious
  page-init `"Permission denied"` cross-origin error; `userAgent`,
  media overrides, geolocation+permissions, `setNetworkConditions`
  shape — Firefox BiDi protocol gaps.
- **WebKit** (stock `WKWebView`): no public API for main-doc
  Response, redirect chain, response body bytes, browser-set request
  headers, `Set-Cookie`, WebSocket frames, dialog intercept,
  download intercept, console args+location, WebError stack frames,
  screencast, multiple browser contexts.

## Key source locations (§2.15)

| area | path |
|---|---|
| `BrowserType` core | `crates/ferridriver/src/browser_type.rs` |
| Public `LaunchOptions` / `ConnectOptions` / `LaunchPersistentContextOptions` | `crates/ferridriver/src/options.rs` |
| Internal `LaunchPlan` | `crates/ferridriver/src/options.rs::LaunchPlan` |
| `BrowserState::with_plan` | `crates/ferridriver/src/state.rs` |
| `persistent_context` flag + `ContextRef::close` shutdown | `crates/ferridriver/src/state.rs`, `crates/ferridriver/src/context.rs` |
| CDP `launch_with_flags_in_dir` | `crates/ferridriver/src/backend/cdp/mod.rs` |
| NAPI `BrowserType` + `chromium`/`firefox`/`webkit` exports | `crates/ferridriver-node/src/browser_type.rs` |
| NAPI `LaunchOptions` shape | `crates/ferridriver-node/src/types.rs` |
| QuickJS `BrowserTypeJs` + `install_browser_type` | `crates/ferridriver-script/src/bindings/browser_type.rs` |
| Rust integration tests | `crates/ferridriver-cli/tests/backends_support/browser_type.rs` (6 tests) |
| NAPI test helper | `crates/ferridriver-node/test/_helpers.ts::launchForBackend` |
| Rules + lessons | `CLAUDE.md` |
