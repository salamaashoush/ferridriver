# Handover — next Playwright-parity session

Read-first for any session continuing work. Overwrite this file with a
fresh summary at the end of each block.

## Cross-device setup

1. `CLAUDE.md` — rules + lessons.
2. `PLAYWRIGHT_COMPAT.md` — gap tracker. Tier 1 done. §3.1, §3.12,
   §2.9, §2.11, §2.10, §2.12, §2.13, §2.14, and now §4.1 (partial)
   landed in recent sessions.
3. This file — block summary below.
4. `docs/NEXT_SESSION.md` — next-block brief.

`git clone https://github.com/microsoft/playwright /tmp/playwright` if missing.

## Landed this session — §4.1 BrowserContextOptions (partial)

Playwright-parity surface verified against
`/tmp/playwright/packages/playwright-core/types/types.d.ts:22229`
(`BrowserContextOptions`) and `:9851` (`browser.newContext`). Three
layers, four backends, with a documented per-backend skip matrix for
fields the underlying protocol cannot honour.

| Surface | Shape | Where wired |
|---|---|---|
| `Browser::new_context(Option<BrowserContextOptions>)` | core sync setter | `crates/ferridriver/src/browser.rs:142` |
| `BrowserContextOptions` | full struct + sub-types | `crates/ferridriver/src/options.rs::BrowserContextOptions` |
| `BrowserState::context_options` | per-composite-key registry | `crates/ferridriver/src/state.rs::context_options` |
| `apply_context_options(page, opts)` | per-field application | `crates/ferridriver/src/context.rs::apply_context_options` |
| `Browser.newContext(options?)` (NAPI) | typed TS via `ts_args_type` | `crates/ferridriver-node/src/browser.rs::new_context` |
| `NapiBrowserContextOptions::into_core` | NAPI → core lowering | `crates/ferridriver-node/src/context.rs:NapiBrowserContextOptions` |
| `browser.newContext(options?)` (QuickJS) | new global `browser` | `crates/ferridriver-script/src/bindings/browser.rs::BrowserJs` |
| `install_browser` | engine + MCP wiring | `crates/ferridriver-script/src/bindings/mod.rs::install_browser`, `crates/ferridriver-mcp/src/tools/script.rs` |

### Field coverage applied

* `userAgent`, `locale`, `timezoneId` — via existing per-page setters.
* `colorScheme`, `reducedMotion`, `forcedColors`, `contrast` — via
  `page.emulate_media`.
* `viewport`, `deviceScaleFactor`, `hasTouch`, `isMobile`,
  `javaScriptEnabled` — via `page.set_viewport` (the bag's
  `resolved_viewport()` folds dimensions + scale + touch into one
  `ViewportConfig`).
* `geolocation` + `permissions` — via `page.set_geolocation` +
  `page.grant_permissions`. **CDP fix landed this session**:
  `Browser.grantPermissions` now scoped to the page's
  `browserContextId` so grants on a fresh `browser.newContext()`
  actually apply (silent default-context-only fallback before).
* `extraHTTPHeaders` — via `page.set_extra_http_headers`.
* `offline` — via `page.set_network_state`.
* `recordVideo` — folded into the bag (transitional
  `BrowserContext.setRecordVideo` setter still works for back-compat).

### CDP-side fixes that ride along

* `Browser.grantPermissions` now sends with `browserContextId` so
  permissions actually apply to fresh contexts (was silently scoping
  to default-context-only).
* `Emulation.setTouchEmulationEnabled` now passes
  `maxTouchPoints: 5` so `navigator.maxTouchPoints` reports a
  non-zero value (Playwright uses 5 too — see
  `crEmulationManager._updateTouch`).
* Browser.grantPermissions dispatched at the browser level (no
  `sessionId`), not via the page session.

### Real backend gaps (typed skip in test matrix)

* **WebKit** — `WKWebView` is a single-context host. `Browser::new_context`
  rejects with `WebKit does not support multiple browser contexts`,
  so all `browser.newContext({...})` calls reject. The transitional
  per-page setters keep working on the default context. WebKit
  Rule-9 tests early-return via `skip_if_no_new_context`.
* **BiDi/Firefox** — `userAgent`, `colorScheme`, `reducedMotion`,
  `forcedColors`, `contrast`, `geolocation`+`permissions`,
  `setNetworkConditions` shape, `javaScriptEnabled` —
  Firefox BiDi is missing or not-yet-wired to the equivalent
  commands. Tracked as Section B gaps.

### Deferred to follow-up §4.1.x phase

Struct field present in Rust + NAPI + QuickJS, but
`apply_context_options` is a no-op for these:

* `acceptDownloads`, `baseURL`, `bypassCSP`, `ignoreHTTPSErrors`
  (each maps to an existing per-page setter; just needs the
  apply-helper line + Rule-9 test).
* `httpCredentials` (per-page setter exists; `origin` scoping +
  `send` policy via APIRequestContext still pending).
* `serviceWorkers` (per-page setter exists; just needs the
  apply-helper line + Rule-9 test).
* `proxy` (needs per-context proxy on `Browser::launch`; CDP supports
  it, BiDi has it via `network.addIntercept`-style overrides).
* `recordHar` (needs HAR writer crate — see §2.6).
* `storageState` (needs IndexedDB capture — see §4.2/§4.3).
* `screen` (CDP `screenWidth/screenHeight` already set from viewport,
  but the dedicated `screen` field deserves its own override).
* `strictSelectors` (core-side flag only — locator strict-mode is
  already on).

### Rule-9 tests

* `crates/ferridriver-cli/tests/backends_support/browser_context_options.rs`
  — **13 tests** with per-backend skip matrix. Each opens a fresh
  context via the new QuickJS `browser` global, applies one option,
  navigates, and observes a page-side effect produced ONLY when the
  option took effect. Tests for `extraHTTPHeaders` and
  `geolocation`+`permissions` spin up tiny one-shot HTTP servers on
  localhost (secure-context requirement for geolocation).
* `crates/ferridriver-node/test/browser-context-options.test.ts` —
  **9 tests** × `cdp-pipe` + `cdp-raw` = 18 NAPI assertions.

### Baseline after this commit (must stay green)

```
cargo clippy --workspace --all-targets -- -D warnings           # clean
cargo test -p ferridriver --lib                                  # 125 core
cargo test -p ferridriver-script --lib                           # 13 script
cargo test -p ferridriver-mcp --lib                              # 38 MCP
cd crates/ferridriver-node && bun run build:debug
cd <repo root> && bun test                                       # 871 (was 853)
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1
# cdp-pipe 153, cdp-raw 153, bidi 148, webkit 149  (+13 each)
```

## Next priorities

1. **§4.1.x — close the deferred subset of BrowserContextOptions**.
   Quick wins (small per-field PRs, each with a Rule-9 test):
   - `acceptDownloads` → `page.set_download_behavior`.
   - `baseURL` → store on `ContextRef`, apply in `page.goto` resolver.
   - `bypassCSP` → `page.set_bypass_csp`.
   - `ignoreHTTPSErrors` → `page.set_ignore_certificate_errors`.
   - `serviceWorkers` → `page.set_service_workers_blocked`.
   - `httpCredentials` → finish origin scoping + `send` policy.
   Larger: `proxy` (per-context launch flags), `recordHar` (writer
   crate, see §2.6), `storageState` (needs §4.2/§4.3 IndexedDB).
2. **§2.15 BrowserType class** — remove ad-hoc `Browser::launch` /
   `Browser::connect` on `Browser`, introduce `BrowserType` with
   `launch`/`connect`/`connectOverCdp`/etc.
3. **§3.17 Auto-waiting deadline parity** — replace fixed backoff
   with Playwright's exponential polling + deadline propagation.
4. **WebKit multi-context** — biggest §4.1 gap. Stock `WKWebView`
   can host multiple `WKWebViewConfiguration` instances each with
   their own `WKProcessPool` for cookie isolation. Plumbing this
   through our IPC unlocks §4.1 across the WebKit backend.
5. **BiDi parity** for the §4.1 fields above. `userAgent` and the
   media overrides are the most impactful.

## Carried-forward backend gaps (real protocol limits)

- **BiDi**: response body unavailable for non-intercepted responses
  (Firefox discards bytes; Playwright's own BiDi backend has the
  same limit). Multi-`Set-Cookie` collapses. `request.postData()`
  null for fetch-with-body. `Download.cancel` typed `Unsupported`
  (Firefox BiDi has no cancel primitive). Page-init emits a spurious
  `"Permission denied to access property 'length'"` cross-origin
  error observed by `pageerror` listeners — real Firefox behaviour,
  tests poll past it. **§4.1: `userAgent`, `colorScheme`,
  `reducedMotion`, `forcedColors`, `contrast`, `geolocation` +
  `permissions`, `offline` (setNetworkConditions shape mismatch),
  `javaScriptEnabled`** — protocol gaps documented in §4.1.
- **WebKit**: stock `WKWebView` exposes no public API for main-doc
  Response observability (§3.1), redirect chain, response body
  bytes, browser-set request headers, `Set-Cookie`, WebSocket frame
  events. Dialog accept/dismiss is decided by the host
  `WKUIDelegate` before the event reaches Rust (§2.9). File chooser
  cannot be intercepted (§2.11). Download events don't flow through
  our IPC (§2.10). **Console args + location** are not carried by
  our current `(level, text)` IPC payload (§2.12). **WebError stack
  richness** is bounded by the same IPC (§2.13). **Screencast** has
  no public API (§2.14). **Multiple browser contexts** unsupported
  (§4.1) — single-host limitation.

## Known flakes

- `context.setOffline toggles network` on WebKit bun occasionally
  fails under the full suite but passes in isolation. Pre-existing.

## Key source locations

| area | path |
|---|---|
| BrowserContextOptions struct | `crates/ferridriver/src/options.rs::BrowserContextOptions` |
| Geolocation / HttpCredentials / ProxyConfig / RecordHarOptions / StorageStateInput / ScreenSize / ServiceWorkerPolicy / ViewportOption | same file |
| context_options registry | `crates/ferridriver/src/state.rs::{context_options, set_context_options, get_context_options}` |
| Browser::new_context(opts) | `crates/ferridriver/src/browser.rs:142` |
| apply_context_options | `crates/ferridriver/src/context.rs::apply_context_options` |
| NAPI Browser.newContext | `crates/ferridriver-node/src/browser.rs::new_context` (with `ts_args_type` for Playwright unions) |
| NAPI options struct | `crates/ferridriver-node/src/context.rs::NapiBrowserContextOptions` |
| QuickJS BrowserJs | `crates/ferridriver-script/src/bindings/browser.rs` |
| QuickJS install_browser | `crates/ferridriver-script/src/bindings/mod.rs::install_browser` |
| MCP run_script wiring | `crates/ferridriver-mcp/src/tools/script.rs` (passes `Browser` handle) |
| Rust integration tests | `crates/ferridriver-cli/tests/backends_support/browser_context_options.rs` |
| NAPI tests | `crates/ferridriver-node/test/browser-context-options.test.ts` |
| Video handle + VideoSink | `crates/ferridriver/src/video.rs::{Video, VideoSink}` |
| WebError handle + ErrorDetails (§2.13) | `crates/ferridriver/src/web_error.rs` |
| ContextEvent + ContextEventEmitter | `crates/ferridriver/src/events.rs` |
| ConsoleMessage handle (§2.12) | `crates/ferridriver/src/console_message.rs` |
| Download handle (§2.10) | `crates/ferridriver/src/download.rs` |
| FileChooser handle (§2.11) | `crates/ferridriver/src/file_chooser.rs` |
| Dialog handle (§2.9) | `crates/ferridriver/src/dialog.rs` |
| Navigation `NavRequestSlot` (§3.1) | `crates/ferridriver/src/network.rs` |
| Rules + lessons | `CLAUDE.md` |
