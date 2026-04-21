# Handover — next Playwright-parity session

Read-first for any session continuing work. Overwrite this file with a
fresh summary at the end of each block.

## Cross-device setup

1. `CLAUDE.md` — rules + lessons.
2. `PLAYWRIGHT_COMPAT.md` — gap tracker. Tier 1 done. §3.1, §3.12,
   §2.9, §2.11, §2.10, and now §2.12 landed in recent sessions.
3. This file — block summary below.
4. `docs/NEXT_SESSION.md` — next-block brief.

`git clone https://github.com/microsoft/playwright /tmp/playwright` if missing.

## Landed this session

### §2.12 — ConsoleMessage rich as first-class handle (this commit)

Live `ConsoleMessage` with sync `type()` / `text()` / `args()` /
`location()` / `page()` / `timestamp()`. Replaces the old
wire-shaped `ConsoleMsg { type, text }` that rode through
`PageEvent::Console` — Rule-3 violation. Dispatch mirrors §2.9 /
§2.10 / §2.11: backend listener builds the live handle from the
protocol's console event and synchronously emits `PageEvent::Console`.

- **`crates/ferridriver/src/console_message.rs`** — new module:
  - `ConsoleMessage` struct behind `Arc`. `text()` lazily joins
    `args.map(preview)` when no explicit text was reported (matches
    Playwright's `server/console.ts` lazy getter).
  - `ConsoleMessageLocation { url, lineNumber, columnNumber }`
    mirrors Playwright's `server/types.ts:169` struct.
  - `new_detached(...)` constructor for cases where the owning page
    weak-backref isn't populated yet — keeps the WebKit drain path
    race-safe.

- **Backends** — all three wire args as live `JSHandle`s and
  location from the protocol's stack trace:
  - CDP (`backend/cdp/mod.rs::spawn_console_listener`): new helpers
    `cdp_remote_object_to_backing` (maps `Runtime.RemoteObject` ->
    `JSHandleBacking::Remote | Value`) and `cdp_stack_trace_to_location`
    (maps `Runtime.StackTrace.callFrames[0]` -> `ConsoleMessageLocation`
    byte-for-byte against Playwright's `crProtocolHelper.ts::toConsoleMessageLocation`).
    Listener now takes a `PageBackref`; events that arrive before the
    outer `Arc<Page>` is addressable are dropped silently (matches
    Playwright's `createHandle(context, arg)` guard).
  - BiDi (`backend/bidi/page.rs`): new helpers
    `bidi_remote_value_to_backing` (handles DOM node `sharedId`, object
    `handle`, and primitive inline value shapes) and
    `bidi_stack_trace_to_location`. `method: 'warn'` remapped to
    `type: 'warning'` for Playwright parity.
  - WebKit (`backend/webkit/mod.rs`): host interceptor only surfaces
    `(level, text)` — args and stack-trace location require a new IPC
    op. Section B gap documented; emitted `ConsoleMessage` has
    `args = []` + default location, but `type` / `text` / `timestamp`
    are real. Same `'warn'` -> `'warning'` remap as BiDi so
    `msg.type()` is consistent across backends.

- **Removed `context::ConsoleMsg`**: the wire-shaped struct is gone.
  `state.rs::ConsoleMessage` re-exports the new core type. MCP
  `diagnostics` tool projects `ConsoleMessage::type_str()` / `text()`
  to its compact JSON output (no live handles needed at that tool
  boundary).

- **`PageEvent::Console(ConsoleMessage)`** — variant now carries the
  live handle. NAPI `waitForEvent` union extended `Either7 -> Either8`
  adding `ConsoleMessage`; generated `.d.ts` shows
  `Promise<Request | Response | WebSocket | Dialog | FileChooser |
  Download | ConsoleMessage | Record<string, any>>`. QuickJS
  `PageJs::waitForEvent` instantiates `ConsoleMessageJs` via
  `Class::instance`. `page.on('console', cb)` and
  `page.events().on(...)` continue to deliver a compact JSON
  projection (`{ type, text, location, timestamp, argsCount }`) —
  same pattern as dialog / filechooser / download.

- **NAPI** (`crates/ferridriver-node/src/console_message.rs`): new
  `#[napi] class ConsoleMessage`. `args()` returns `Vec<JSHandle>`
  wrapping each core handle. `page()` returns `Page | null`.
  `ConsoleMessageLocation` as `#[napi(object)]`.

- **QuickJS** (`crates/ferridriver-script/src/bindings/console_message.rs`):
  new `ConsoleMessageJs` with matching surface (no `page()`, symmetric
  with `DownloadJs` / `FileChooserJs`). `args()` instantiates
  `JSHandleJs` via `Class::instance`.

### Rule-9 tests

- `tests/backends_support/console_message.rs`: type/text/args,
  `console.warn` -> `'warning'` remap, `console.error` type, location
  shape. 4 tests × 4 backends = 16 assertions. All four backends
  green.
- `crates/ferridriver-node/test/console-message.test.ts`: 5 tests ×
  2 CDP backends = 10 assertions covering type/text/args round-trip,
  warn -> warning remap, error type, location shape (trigger via
  inline `<script>` for stack-trace attribution), numeric timestamp.

### Baseline after this commit (must stay green)

```
cargo clippy --workspace --all-targets -- -D warnings           # clean
cargo test -p ferridriver --lib                                  # 125 core
cargo test -p ferridriver-script --lib                           # 13 script (or 22 with integration)
cargo test -p ferridriver-mcp --lib                              # 38 MCP
cd crates/ferridriver-node && bun run build:debug
cd <repo root> && bun test                                       # 835 (was 825)
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1
# cdp-pipe 136, cdp-raw 136, bidi 131, webkit 132
```

## Next priorities

1. **§2.13 WebError** — add `WebError { error, page }` for
   `context.on('weberror')`. Same live-handle dispatch as console.
2. **§4.1 BrowserContextOptions** — 28-field option bag at context
   creation (viewport, userAgent, locale, timezone, geolocation,
   permissions, acceptDownloads, ...). Probably 2–3 sessions.
3. **§3.17 Auto-waiting deadline parity** — replace fixed backoff
   with Playwright's exponential polling + deadline propagation.
4. **WebKit console args + location bridge** — add an IPC op that
   captures per-arg serialization + stack-trace frames. Closes the
   §2.12 WebKit Section B gap.
5. **WebKit Download bridge** — add `WKDownloadDelegate` to `host.m`
   + IPC op routing. Closes the §2.10 WebKit gap.

## Carried-forward backend gaps (real protocol limits)

- **BiDi**: response body unavailable for non-intercepted responses
  (Firefox discards bytes; Playwright's own BiDi backend has the
  same limit). Multi-`Set-Cookie` collapses. `request.postData()`
  null for fetch-with-body. `Download.cancel` typed `Unsupported`
  (Firefox BiDi has no cancel primitive).
- **WebKit**: stock `WKWebView` exposes no public API for main-doc
  Response observability (§3.1: returns `null`, documented),
  redirect chain, response body bytes, browser-set request headers,
  `Set-Cookie`, WebSocket frame events. Dialog accept/dismiss is
  decided by the host `WKUIDelegate` before the event reaches Rust
  (§2.9: `Dialog.accept/dismiss` returns typed `Unsupported`). File
  chooser cannot be intercepted (§2.11: times out). Download events
  don't flow through our IPC (§2.10: times out). **Console args +
  location** are not carried by our current `(level, text)` IPC
  payload (§2.12: `args = []`, default location). `page.evaluate`
  runs in utility context isolated from the user-script's fetch wrap.

## Known flakes

- `context.setOffline toggles network` on WebKit bun occasionally
  fails under the full suite but passes in isolation. Pre-existing.

## Key source locations

| area | path |
|---|---|
| ConsoleMessage handle + location | `crates/ferridriver/src/console_message.rs` |
| CDP console listener | `crates/ferridriver/src/backend/cdp/mod.rs::spawn_console_listener` |
| CDP RemoteObject -> backing | `crates/ferridriver/src/backend/cdp/mod.rs::cdp_remote_object_to_backing` |
| CDP stack trace -> location | `crates/ferridriver/src/backend/cdp/mod.rs::cdp_stack_trace_to_location` |
| BiDi console handler | `crates/ferridriver/src/backend/bidi/page.rs` (`log.entryAdded` arm) |
| BiDi RemoteValue -> backing | `crates/ferridriver/src/backend/bidi/page.rs::bidi_remote_value_to_backing` |
| WebKit console drain | `crates/ferridriver/src/backend/webkit/mod.rs::attach_listeners` |
| NAPI ConsoleMessage class | `crates/ferridriver-node/src/console_message.rs` |
| QuickJS ConsoleMessageJs class | `crates/ferridriver-script/src/bindings/console_message.rs` |
| Rust integration tests | `crates/ferridriver-cli/tests/backends_support/console_message.rs` |
| NAPI tests | `crates/ferridriver-node/test/console-message.test.ts` |
| Download handle + manager (§2.10) | `crates/ferridriver/src/download.rs` |
| FileChooser handle + manager (§2.11) | `crates/ferridriver/src/file_chooser.rs` |
| PageBackref helper | `crates/ferridriver/src/backend/mod.rs::PageBackref` |
| Dialog handle + manager (§2.9) | `crates/ferridriver/src/dialog.rs` |
| Navigation `NavRequestSlot` (§3.1) | `crates/ferridriver/src/network.rs` |
| `StringOrRegex` + escapes (§3.12) | `crates/ferridriver/src/options.rs`, `locator.rs` |
| Rules + lessons | `CLAUDE.md` |
