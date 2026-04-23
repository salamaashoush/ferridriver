# Handover — next Playwright-parity session

Read-first for any session continuing work. Overwrite this file with a
fresh summary at the end of each block.

## Cross-device setup

1. `CLAUDE.md` — rules + lessons.
2. `PLAYWRIGHT_COMPAT.md` — gap tracker. Tier 1 done. §3.1, §3.12,
   §2.9, §2.11, §2.10, §2.12, §2.13, §2.14 landed earlier. §4.1
   now at **18 of 28 fields** — the big BrowserContextOptions
   refactor + coverage landed across three commits.
3. This file — block summary below.
4. `docs/NEXT_SESSION.md` — next-block brief + prompt.

`git clone https://github.com/microsoft/playwright /tmp/playwright` if missing.

## Landed this session

### Commit 1 — `48cc794` feat(context): BrowserContextOptions bag

Struct + core wiring + NAPI + QuickJS + first cluster of 13 fields
with Rule-9 tests. See the commit message for the full list; summary:
`userAgent`, `locale`, `timezoneId`, `colorScheme`, `reducedMotion`,
`forcedColors`, `contrast`, `viewport`, `deviceScaleFactor`,
`hasTouch`, `isMobile`, `javaScriptEnabled`, `geolocation` +
`permissions`, `extraHTTPHeaders`, `offline`, `recordVideo` folded
in.

CDP fixes that rode along: `Browser.grantPermissions` now ships
`browserContextId` (without it permissions silently scoped to
default context only); `Emulation.setTouchEmulationEnabled` passes
`maxTouchPoints: 5`.

### Commit 2 — `3ec1dc9` refactor: single apply_context_options pathway

Deleted 13 Java-bean-style page setters (`set_user_agent`,
`set_locale`, `set_timezone`, `set_geolocation`, `set_network_state`,
`set_javascript_enabled`, `set_bypass_csp`,
`set_ignore_certificate_errors`, `set_download_behavior`,
`set_http_credentials`, `set_service_workers_blocked`,
`set_focus_emulation_enabled`, `set_storage_state`, `set_viewport`).

One method on each layer — `Page::apply_context_options(opts)` →
`AnyPage::apply_context_options(opts)` → backend's single
`apply_context_options` which inlines every protocol command via
`tokio::join!` + `OptionFuture` per field, aggregating failures
into one labelled error.

ContextRef public setters (`setGeolocation`, `setOffline`,
`setExtraHTTPHeaders`, `grantPermissions`, `clearPermissions`) now
go through a single `mutate_options` helper that mutates the stored
bag on `BrowserState::context_options` and re-applies to every open
page. Matches Playwright's "the bag is the source of truth".

Worker/BDD/NAPI-test/QuickJS callers migrated to build options bags
and call apply once.

Remaining `set_*` methods anywhere in the codebase are direct
Playwright JS API names (`setContent`, `setViewportSize`,
`setExtraHTTPHeaders`, `setDefaultTimeout`,
`setDefaultNavigationTimeout`, `setInputFiles`, `setChecked`,
`setGeolocation`, `setOffline`) or wire-protocol command wrappers
(`set_cookie` → `Network.setCookie`).

### Commit 3 — `e0b3d51` feat(context): second wave (bypassCSP / serviceWorkers / screen / baseURL / storageState / proxy)

Six more fields, Rule-9 tested on every supported backend:

- **`bypassCSP`** — CDP `Page.setBypassCSP`. Test serves a
  `Content-Security-Policy: script-src 'none'` HTML and asserts
  `addInitScript` executes only with bypass. BiDi skipped.
- **`serviceWorkers: 'block'`** — cross-backend init-script
  injection (Playwright's exact pattern at
  `browserContext.ts:168`).
- **`screen`** — CDP `setDeviceMetricsOverride` with `screenWidth`/
  `screenHeight`. Test asserts `window.screen.{width,height}`.
  CDP only.
- **`baseURL`** — new `options::construct_url_with_base`, applied
  in `Page::goto` before dispatch. Mirrors Playwright's
  `constructURLBasedOnBaseURL` at
  `/tmp/playwright/packages/isomorphic/urlMatch.ts:253`.
  Works on every backend.
- **`storageState: string | { cookies, origins }`** — cookies +
  localStorage hydration on the first page of a context, tracked
  per-composite-key via
  `BrowserState::claim_storage_state_hydration`. `Path(PathBuf)`
  reads JSON from disk; `Inline(Value)` consumes directly. Works
  on every backend (WebKit cookie assertion is CDP-only due to
  stricter `secure`+loopback validation).
- **`proxy: { server, bypass? }`** — per-context via
  `Target.createBrowserContext({ proxyServer, proxyBypassList })`
  on CDP and `browser.createUserContext({ proxy })` on BiDi with
  WebDriver capability decomposition (`proxyType: 'manual',
  httpProxy, sslProxy, socksProxy, socksVersion, noProxy`). Test
  uses `bypass: '<-loopback>'` so 127.0.0.1 routes through the
  proxy (matches Playwright's `chromium.ts::proxyBypassRules`).
  BiDi skipped (Firefox 137+ required).

### Baseline after this session (must stay green)

```
cargo clippy --workspace --all-targets -- -D warnings            # clean
cargo test -p ferridriver --lib                                   # 125 pass
cargo test -p ferridriver-script --lib                            # 13 pass
cargo test -p ferridriver-mcp --lib                               # 38 pass
cd crates/ferridriver-node && bun run build:debug
cd <repo root> && bun test                                        # 859 pass (was 871; 12 non-Playwright-public setter tests deleted in refactor)
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1
# cdp-pipe 159, cdp-raw 159, bidi 154, webkit 155  (+19 per backend vs pre-§4.1)
```

## §4.1 coverage matrix (now)

18 of 28 Playwright `BrowserContextOptions` fields have
apply_context_options plumbing + Rule-9 tests:

Applied: `acceptDownloads`, `baseURL`, `bypassCSP`, `colorScheme`,
`contrast`, `deviceScaleFactor`, `extraHTTPHeaders`, `forcedColors`,
`geolocation`, `hasTouch`, `httpCredentials` (origin scoping),
`ignoreHTTPSErrors`, `isMobile`, `javaScriptEnabled`, `locale`,
`offline`, `permissions`, `proxy`, `recordVideo`, `reducedMotion`,
`screen`, `serviceWorkers`, `storageState` (cookies+localStorage),
`timezoneId`, `userAgent`, `viewport`.

Deferred (documented in PLAYWRIGHT_COMPAT.md §4.1 + "Section B"):

- **`recordHar`** — blocks on **§2.6** (HAR writer).
- **`clientCertificates`** — needs TLS-intercepting proxy (major).
- **`httpCredentials.send` policy** — needs APIRequestContext
  preemptive-header wiring.
- **`strictSelectors`** — needs strict-mode counting threaded
  through every backend selector path.

## Next priorities

See `docs/NEXT_SESSION.md` for the full next-session prompt. Top pick
is **§2.15 BrowserType class** — extracts `Browser::launch` /
`Browser::connect` off `Browser`, introduces `BrowserType` with
`launch` / `connect` / `connectOverCDP` / `launchPersistentContext`.
Matches Playwright's public JS entry point (`chromium.launch()`
rather than `Browser.launch({ browser: 'chromium' })`).

Alternative picks:

- Close the four §4.1 deferred fields (one per dedicated session).
- §2.6 HAR recording (unblocks §4.1 `recordHar`).
- §2.3 Tracing (unblocks §4.5 `context.tracing`).
- §3.17 Auto-waiting deadline parity.

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

## Key source locations (§4.1)

| area | path |
|---|---|
| `BrowserContextOptions` struct | `crates/ferridriver/src/options.rs::BrowserContextOptions` |
| `construct_url_with_base` | `crates/ferridriver/src/options.rs::construct_url_with_base` |
| `context_options` registry | `crates/ferridriver/src/state.rs::{context_options, set_context_options, get_context_options}` |
| `storage_state_hydrated` | `crates/ferridriver/src/state.rs::claim_storage_state_hydration` |
| `Browser::new_context(opts)` | `crates/ferridriver/src/browser.rs` |
| `apply_context_options` (context side) | `crates/ferridriver/src/context.rs::apply_context_options` |
| `ContextRef::mutate_options` | `crates/ferridriver/src/context.rs::mutate_options` |
| `AnyPage::apply_context_options` | `crates/ferridriver/src/backend/mod.rs` |
| CDP impl | `crates/ferridriver/src/backend/cdp/mod.rs::apply_context_options` |
| BiDi impl | `crates/ferridriver/src/backend/bidi/page.rs::apply_context_options` |
| WebKit impl | `crates/ferridriver/src/backend/webkit/mod.rs::apply_context_options` |
| BiDi proxy decomposition | `crates/ferridriver/src/backend/bidi/browser.rs::parse_bidi_proxy` |
| NAPI `Browser.newContext(options)` | `crates/ferridriver-node/src/browser.rs::new_context` |
| NAPI options struct | `crates/ferridriver-node/src/context.rs::NapiBrowserContextOptions` |
| QuickJS `browser` global | `crates/ferridriver-script/src/bindings/browser.rs::BrowserJs` |
| QuickJS install | `crates/ferridriver-script/src/bindings/mod.rs::install_browser` |
| MCP run_script wiring | `crates/ferridriver-mcp/src/tools/script.rs` |
| Rust integration tests | `crates/ferridriver-cli/tests/backends_support/browser_context_options.rs` (17 tests) |
| NAPI tests | `crates/ferridriver-node/test/browser-context-options.test.ts` (9 tests × 2 backends) |
| Rules + lessons | `CLAUDE.md` |
