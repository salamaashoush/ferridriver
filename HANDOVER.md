# Handover — next Playwright-parity session

Read-first for any session continuing work. Overwrite this file with a
fresh summary at the end of each block.

## Cross-device setup

1. `CLAUDE.md` — rules + lessons.
2. `PLAYWRIGHT_COMPAT.md` — gap tracker. Tier 1 done. §3.1, §3.12,
   §2.9, §2.11, §2.10, §2.12, and now §2.13 landed in recent sessions.
3. This file — block summary below.
4. `docs/NEXT_SESSION.md` — next-block brief.

`git clone https://github.com/microsoft/playwright /tmp/playwright` if missing.

## Landed this session

### §2.13 — WebError / pageerror / weberror with Playwright API shape

Playwright-parity surface verified against
`/tmp/playwright/packages/playwright-core/types/types.d.ts`:

| Surface | Delivers | Playwright ref |
|---|---|---|
| `page.on('pageerror', cb)` | native JS `Error` | `types.d.ts:1101` |
| `page.waitForEvent('pageerror')` | `Promise<Error>` | `types.d.ts:4897` |
| `context.on('weberror', cb)` | live `WebError` class | `types.d.ts:8365` |
| `context.waitForEvent('weberror')` | `Promise<WebError>` | `types.d.ts:9629` |
| `WebError.page()` | `null | Page` | `types.d.ts:21667` |
| `WebError.error()` | native JS `Error` | `types.d.ts:21662` |

All tests assert `err instanceof Error` (page surface) and
`webErr instanceof WebError` + `webErr.error() instanceof Error`
(context surface). Replaces the old flat
`PageEvent::PageError(String)` — Rule-3 violation. Dispatched via a
new `ContextEvent::WebError` channel to `context.on('weberror', cb)` /
`context.waitForEvent('weberror')` (context-scoped).

- **`crates/ferridriver/src/web_error.rs`** — new module:
  - `WebError` struct behind `Arc<WebErrorState>`. `error()` returns a
    shared `&ErrorDetails { name, message, stack }`, `page()` upgrades
    a weak back-reference.
  - `new(...)` / `new_detached(...)` constructors — the detached form
    is used where the backend listener spawns before the outer
    `Arc<Page>` is populated (CDP/BiDi race window, WebKit drain).

- **Context event system** (`crates/ferridriver/src/events.rs`):
  - `ContextEvent::WebError(WebError)` enum + `ContextEventEmitter`
    (broadcast-based, parallel to the per-page `EventEmitter`). Only
    `'weberror'` today — extensible to `'page'`, `'request'`,
    `'response'`, etc. under §6.14 without further refactor.
  - Emitter registry lives on `BrowserState::context_events`
    (`Arc<std::sync::Mutex<HashMap<composite-key, ContextEventEmitter>>>`).
    `ContextRef::new` looks up the emitter by composite key — two
    `browser.defaultContext()` calls now hand out ContextRefs with the
    SAME emitter, so events dispatched via one listener are observed
    by another.

- **Page → Context fan-out bridge**
  (`crates/ferridriver/src/state.rs::register_opened_page`):
  Spawns a forwarding task exactly once per registered page. Fetches
  the context emitter via `get_or_create_context_events` BEFORE taking
  the mutable instance borrow. Forwards `PageEvent::PageError` →
  `ContextEvent::WebError`. Runs regardless of whether the page is
  later wrapped via `Page::new` or `Page::with_context`, so MCP-server
  `run_script` callers get the same fan-out NAPI callers do.

- **Backends** — all three emit real exception data:
  - **CDP** (`backend/cdp/mod.rs::spawn_web_error_listener` +
    `cdp_exception_to_error_details` / `cdp_get_exception_message` /
    `split_error_message`): new listener on `Runtime.exceptionThrown`.
    Helpers mirror Playwright's `crProtocolHelper.ts::{getExceptionMessage,
    exceptionToError}` + `stackTrace.ts::splitErrorMessage` byte-for-byte.
    Custom `Error` subclasses (`TypeError`, `RangeError`, …) via the
    `exception.preview.properties.name` override branch.
  - **BiDi** (`backend/bidi/page.rs`): `log.entryAdded` with
    `type: 'javascript'` + `level: 'error'` routes to
    `PageEvent::PageError(WebError)` (was silently dropped).
    `split_error_text` + `build_bidi_stack` helpers synthesise the
    `error.stack` string from BiDi's `stackTrace.callFrames` (with
    `+1` to adjust BiDi's 0-based line/column to the 1-based JS
    convention) — mirrors `bidiPage.ts:280-283` verbatim.
  - **WebKit** (`backend/webkit/mod.rs` + `backend/webkit/host.m`):
    new `errorJS` userScript installs
    `window.addEventListener('error', …)` +
    `'unhandledrejection'` and forwards through the existing
    `fdConsole` IPC channel with `level: 'pageerror'` and
    `text: '<name>: <message>\n<stack>'`. The Rust-side drain
    (`drain_console_events` — extracted to its own fn to get under
    the 100-line clippy ceiling) routes `'pageerror'` levels to
    `PageEvent::PageError(WebError)` instead of
    `PageEvent::Console(ConsoleMessage)` and recovers the structured
    `{name, message, stack}` via `parse_webkit_pageerror_payload`.
    Reuses the `(level, text)` IPC — same ceiling as §2.12 console.

- **`PageEvent::PageError(WebError)`** — variant upgraded from
  `String`. All consumers updated in the same commit: NAPI `on` /
  `once` / `waitForEvent` / page-event-JSON projection, QuickJS
  `match_event_name` / `page_event_json` / `waitForEvent`.

- **NAPI** (`crates/ferridriver-node/src/web_error.rs`):
  `#[napi] class WebError` with `page()` / `error(): Error` (native).
  `JsErrorValue` is the cross-thread Rust wrapper whose
  `ToNapiValue::to_napi_value` runs inside the JS thread: it fetches
  `globalThis.Error`, calls `new Error(message)`, and overrides
  `name` / `stack` — the returned value satisfies `instanceof Error`.
  `PageListenerArg` enum (`Snapshot(Value)` | `PageError(JsErrorValue)`)
  is the threadsafe-function arg for `page.on/once`; non-pageerror
  events keep the existing JSON snapshot path.
  `WebErrorArg(CoreWebError)` is the threadsafe-function arg for
  `context.on/once` — its `ToNapiValue` wraps the core handle in the
  NAPI `WebError` class.
  `page.waitForEvent` union: `Either9` slot H changed from `WebError`
  to `JsErrorValue` (and `ts_return_type` updated from
  `… | WebError | …` to `… | Error | …`) — the `WebError` class is
  only reachable via the context surface, matching Playwright.
  `build_context_event_callback` helper isolates the `!Send`
  `Function<'_>` lowering from the async generator.

- **QuickJS** (`crates/ferridriver-script/src/bindings/web_error.rs`):
  `WebErrorJs` mirrors NAPI minus `page()` (symmetric with
  `ConsoleMessageJs` / `DownloadJs` / `FileChooserJs`).
  `build_native_error(ctx, details)` is the shared helper: `ctx.eval`
  defines a tiny `(n, m, s) => { const e = new Error(m); e.name = n;
  if (s) e.stack = s; return e; }` factory and invokes it. Used by
  `WebErrorJs::error()` AND `PageJs::waitForEvent('pageerror')`.
  rquickjs's `Function` lacks a `call-as-new` primitive and
  `Constructor`'s inner field is `pub(crate)`, blocking the obvious
  `Constructor(fun)` wrap — the eval factory is the cleanest portable
  path and keeps both surfaces consistent.
  `BrowserContextJs::waitForEvent('weberror')` returns `WebErrorJs`.

### Rule-9 tests

- `tests/backends_support/web_error.rs` — 2 tests × 4 backends =
  **8 assertions**. Playwright-shape: `test_page_error_is_native_error`
  asserts `err instanceof Error` on the raw `page.waitForEvent('pageerror')`
  return value; `test_context_weberror_is_webbed_error_class` asserts
  `typeof webErr.error === 'function'` + `webErr.error() instanceof Error`
  on the raw `context.waitForEvent('weberror')` return value. Both poll
  for the specific error identifier — Firefox BiDi emits a spurious
  `"Permission denied to access property 'length'"` cross-origin error
  at page init that would otherwise land first.
- `crates/ferridriver-node/test/web-error.test.ts`: 6 tests ×
  2 CDP backends = **12 assertions** covering `page.waitForEvent` →
  `instanceof Error`, `TypeError` name preservation,
  `page.on('pageerror', cb)` → `instanceof Error`,
  `context.waitForEvent` → `instanceof WebError` + `.error() instanceof Error`,
  `context.on('weberror', cb)` → `instanceof WebError`, and
  `WebError.page()` back-reference.

### Baseline after this commit (must stay green)

```
cargo clippy --workspace --all-targets -- -D warnings           # clean
cargo test -p ferridriver --lib                                  # 125 core
cargo test -p ferridriver-script --lib                           # 13 script
cargo test -p ferridriver-mcp --lib                              # 38 MCP
cd crates/ferridriver-node && bun run build:debug
cd <repo root> && bun test                                       # 847 (was 845 / 835)
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1
# cdp-pipe 138, cdp-raw 138, bidi 133, webkit 134
```

## Next priorities

1. **§2.14 Video** — expose `Video { path, save_as, delete }` via
   `page.video()`; wire `record_video` context option.
2. **§4.1 BrowserContextOptions** — 28-field option bag at context
   creation (viewport, userAgent, locale, timezone, geolocation,
   permissions, acceptDownloads, ...). Probably 2–3 sessions.
3. **§3.17 Auto-waiting deadline parity** — replace fixed backoff
   with Playwright's exponential polling + deadline propagation.
4. **WebKit console args + location + web-error stack frames
   bridge** — ONE new IPC op (e.g. `Op::RichConsoleEvent` carrying
   `args + stack frames + isError: bool`) closes both the §2.12
   WebKit args/location gap AND the §2.13 WebKit stack-richness
   ceiling together.
5. **WebKit Download bridge** — add `WKDownloadDelegate` to `host.m`
   + IPC op routing. Closes the §2.10 WebKit gap.

## Carried-forward backend gaps (real protocol limits)

- **BiDi**: response body unavailable for non-intercepted responses
  (Firefox discards bytes; Playwright's own BiDi backend has the
  same limit). Multi-`Set-Cookie` collapses. `request.postData()`
  null for fetch-with-body. `Download.cancel` typed `Unsupported`
  (Firefox BiDi has no cancel primitive). Page-init emits a spurious
  `"Permission denied to access property 'length'"` cross-origin
  error observed by `pageerror` listeners — real Firefox behaviour,
  not a ferridriver bug; tests poll past it.
- **WebKit**: stock `WKWebView` exposes no public API for main-doc
  Response observability (§3.1: returns `null`, documented),
  redirect chain, response body bytes, browser-set request headers,
  `Set-Cookie`, WebSocket frame events. Dialog accept/dismiss is
  decided by the host `WKUIDelegate` before the event reaches Rust
  (§2.9: `Dialog.accept/dismiss` returns typed `Unsupported`). File
  chooser cannot be intercepted (§2.11: times out). Download events
  don't flow through our IPC (§2.10: times out). **Console args +
  location** are not carried by our current `(level, text)` IPC
  payload (§2.12: `args = []`, default location). **WebError stack
  richness** is bounded by the same IPC — `stack` is whatever JSC's
  `error.stack` string reports, no structured frame array (§2.13).
  `page.evaluate` runs in utility context isolated from the
  user-script's fetch wrap.

## Known flakes

- `context.setOffline toggles network` on WebKit bun occasionally
  fails under the full suite but passes in isolation. Pre-existing.

## Key source locations

| area | path |
|---|---|
| WebError handle + ErrorDetails | `crates/ferridriver/src/web_error.rs` |
| ContextEvent + ContextEventEmitter | `crates/ferridriver/src/events.rs` |
| Per-page → context bridge | `crates/ferridriver/src/state.rs::register_opened_page` |
| Shared ContextEventEmitter registry | `crates/ferridriver/src/state.rs::{context_events, get_or_create_context_events}` |
| CDP exception listener | `crates/ferridriver/src/backend/cdp/mod.rs::spawn_web_error_listener` |
| CDP exception → ErrorDetails | `crates/ferridriver/src/backend/cdp/mod.rs::cdp_exception_to_error_details` |
| BiDi JS error routing | `crates/ferridriver/src/backend/bidi/page.rs` (`log.entryAdded` JS-error arm) |
| BiDi stack synthesis | `crates/ferridriver/src/backend/bidi/page.rs::build_bidi_stack` |
| WebKit host error userScript | `crates/ferridriver/src/backend/webkit/host.m::errorJS` |
| WebKit pageerror drain | `crates/ferridriver/src/backend/webkit/mod.rs::drain_console_events` |
| WebKit payload parser | `crates/ferridriver/src/backend/webkit/mod.rs::parse_webkit_pageerror_payload` |
| NAPI WebError class | `crates/ferridriver-node/src/web_error.rs` |
| NAPI context `weberror` wiring | `crates/ferridriver-node/src/context.rs` |
| QuickJS WebErrorJs class | `crates/ferridriver-script/src/bindings/web_error.rs` |
| QuickJS context `waitForEvent('weberror')` | `crates/ferridriver-script/src/bindings/context.rs` |
| Rust integration tests | `crates/ferridriver-cli/tests/backends_support/web_error.rs` |
| NAPI tests | `crates/ferridriver-node/test/web-error.test.ts` |
| ConsoleMessage handle (§2.12) | `crates/ferridriver/src/console_message.rs` |
| Download handle (§2.10) | `crates/ferridriver/src/download.rs` |
| FileChooser handle (§2.11) | `crates/ferridriver/src/file_chooser.rs` |
| Dialog handle (§2.9) | `crates/ferridriver/src/dialog.rs` |
| Navigation `NavRequestSlot` (§3.1) | `crates/ferridriver/src/network.rs` |
| `StringOrRegex` + escapes (§3.12) | `crates/ferridriver/src/options.rs`, `locator.rs` |
| Rules + lessons | `CLAUDE.md` |
