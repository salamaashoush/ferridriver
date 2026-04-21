# Handover — next Playwright-parity session

Read-first for any session continuing work. Overwrite this file with a
fresh summary at the end of each block.

## Cross-device setup

1. `CLAUDE.md` — rules + lessons.
2. `PLAYWRIGHT_COMPAT.md` — gap tracker. Tier 1 done. §3.1, §3.12,
   §2.9, and §2.11 landed in recent sessions.
3. This file — block summary below.
4. `docs/NEXT_SESSION.md` — next-block brief.

`git clone https://github.com/microsoft/playwright /tmp/playwright` if missing.

## Landed this session

### §2.11 — FileChooser as first-class handle (this commit)

Live `FileChooser` handle with `element()` / `isMultiple()` / `page()`
/ async `setFiles(files, options?)`. Dispatch follows the same
synchronous-claim pattern §2.9 Dialog shipped: backend listener
resolves the triggering `<input>` into an `ElementHandle` in its
async task, then synchronously calls `FileChooserManager::did_open(&chooser)`
— no broadcast race, no grace-window polling.

- **`crates/ferridriver/src/file_chooser.rs`** — new module:
  - `FileChooser { element, is_multiple }` behind `Arc`. `page()` derives
    from `element.page()` so the chooser always reports the page its
    element lives on. `set_files` delegates to
    `ElementHandle::set_input_files` and reuses the §1.5 path /
    payload plumbing verbatim.
  - `FileChooserManager` per-page registry. `add_handler(Fn(&FileChooser) -> bool) ->
    FileChooserHandlerId`, `remove_handler(id)`, `did_open(&FileChooser)`.
    `did_open` iterates handlers synchronously; each returns `true`
    to claim. If nobody claims, a detached task disposes the
    underlying `ElementHandle` (matches Playwright's
    `server/page.ts::_onFileChooserOpened` no-listener branch).
  - `FileChooserManager::register_emitter_bridge(events)` — default
    handler installed once per page in each backend's
    `attach_listeners`. When a `filechooser` broadcast listener is
    registered, the bridge emits `PageEvent::FileChooser` on the
    broadcast and returns `true` synchronously.

- **`crate::backend::PageBackref`** — new shared helper (`Mutex<Weak<Page>>`):
  every backend page struct carries one. Populated by `Page::new` /
  `Page::with_context` on every construction — MCP tool handlers wrap
  the same backend page fresh on every call, so a one-shot
  `OnceLock<Weak<Page>>` would lock in a stale weak whose target
  dies as soon as the first call returns. The file-chooser listener
  calls `page_backref.upgrade()` per event.

- **Backends** — all three wire the same pattern:
  - CDP (`backend/cdp/mod.rs::spawn_file_chooser_listener`):
    subscribes first, **then** sends `Page.setInterceptFileChooserDialog({ enabled: true })`
    (reverse order misses events triggered between the enable-reply
    and subscribe). On `Page.fileChooserOpened`, reads `backendNodeId`
    + `mode`, upgrades the backref, spawns a per-event task that
    resolves the element via `AnyPage::resolve_backend_node`, builds
    the `ElementHandle`, and dispatches via `manager.did_open(&chooser)`.
  - BiDi (`backend/bidi/page.rs`): same for `input.fileDialogOpened`
    (Firefox's native BiDi event). Resolves via the sharedId payload
    directly — no extra DOM round-trip.
  - WebKit: stock `WKWebView` has no public API for intercepting
    the open-panel; the host's `WKUIDelegate` runs the native panel
    before any event could reach Rust. The manager is still wired
    for API parity, but no event is ever dispatched. Rule-4 honest.

- **Page API** — new `Page::wait_for_file_chooser(timeout_ms) -> Result<FileChooser>`:
  registers a one-shot handler with `FileChooserManager`, awaits a
  `tokio::sync::oneshot`, removes the handler on resolve / timeout.
  Typed `Timeout` / `TargetClosed` errors. NAPI + QuickJS route
  `page.waitForEvent('filechooser')` through it so the claim is
  synchronous with the browser event — no broadcast round-trip.

- **NAPI** (`crates/ferridriver-node/src/file_chooser.rs`): new
  `#[napi] class FileChooser`. `page.waitForEvent('filechooser')`
  returns it via a new `Either6<Request, Response, WebSocket, Dialog,
  FileChooser, Value>` — generated `.d.ts` matches Playwright's
  `Promise<Request | Response | WebSocket | Dialog | FileChooser | Record<string, any>>`.
  `setFiles` accepts the full `string | string[] | FilePayload | FilePayload[]`
  union via `ts_args_type`.

- **QuickJS** (`crates/ferridriver-script/src/bindings/file_chooser.rs`):
  new `FileChooserJs`. `page.waitForEvent('filechooser')` instantiates it.
  `setFiles` reuses `parse_input_files` / `parse_set_input_files_options`
  from §1.5.

### Rule-9 tests

- `tests/backends_support/file_chooser.rs`: single path + multiple
  array + FilePayload + unclaimed-disposes + WebKit-unsupported. All
  four backends green.
- `crates/ferridriver-node/test/filechooser.test.ts`: single path +
  multiple array + FilePayload with `text=hello` round-trip (also
  proves payload bytes reached the DOM's view of the file) + waitForEvent
  timeout. 4 tests × 2 CDP backends = 8 assertions.

### Baseline after this commit (must stay green)

```
cargo clippy --workspace --all-targets -- -D warnings   # clean
cargo test --workspace --lib                            # 122 core + 29 script + 38 MCP
cd crates/ferridriver-node && bun run build:debug
cd <repo root> && bun test                              # 817 (was 809)
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1   # 4/4 backends
```

## Next priorities

1. **§2.10 Download as handle** — promote `Download` to a first-class
   handle (`cancel`, `create_read_stream`, `delete`, `failure`, `page`,
   `path`, `save_as`, `suggested_filename`, `url`). Follows the same
   live-event pattern as §2.9 / §2.11.
2. **§4.1 BrowserContextOptions** — 28-field option bag at context
   creation (viewport, userAgent, locale, timezone, geolocation,
   permissions, …). Probably 2–3 sessions.
3. **§3.17 Auto-waiting deadline parity** — replace fixed backoff with
   Playwright's exponential polling + deadline propagation.
4. **§2.12 ConsoleMessage rich** — replace `ConsoleMsg { type, text }`
   with the full Playwright `ConsoleMessage { args, location, page,
   text, type, timestamp }`.

## Carried-forward backend gaps (real protocol limits)

- **BiDi**: response body unavailable for non-intercepted responses
  (Firefox discards bytes; Playwright's own BiDi backend has the
  same limit). Multi-`Set-Cookie` collapses. `request.postData()`
  null for fetch-with-body.
- **WebKit**: stock `WKWebView` exposes no public API for main-doc
  Response observability (§3.1: returns `null`, documented),
  redirect chain, response body bytes, browser-set request headers,
  `Set-Cookie`, WebSocket frame events. Dialog accept/dismiss is
  decided by the host `WKUIDelegate` before the event reaches Rust
  (§2.9: `Dialog.accept/dismiss` returns typed `Unsupported`). **File
  chooser** cannot be intercepted — no event flows through our IPC
  (§2.11: `Page::wait_for_file_chooser` times out, documented).
  `page.evaluate` runs in utility context isolated from the
  user-script's fetch wrap.

## Known flakes

- `context.setOffline toggles network` on WebKit bun occasionally
  fails under the full suite but passes in isolation. Pre-existing.

## Key source locations

| area | path |
|---|---|
| FileChooser handle + manager | `crates/ferridriver/src/file_chooser.rs` |
| PageBackref helper | `crates/ferridriver/src/backend/mod.rs::PageBackref` |
| CDP fileChooser listener | `crates/ferridriver/src/backend/cdp/mod.rs::spawn_file_chooser_listener` |
| BiDi fileChooser listener | `crates/ferridriver/src/backend/bidi/page.rs` (`input.fileDialogOpened` arm) |
| WebKit fileChooser (no-op bridge) | `crates/ferridriver/src/backend/webkit/mod.rs::attach_listeners` |
| Page::wait_for_file_chooser | `crates/ferridriver/src/page.rs` |
| NAPI FileChooser class | `crates/ferridriver-node/src/file_chooser.rs` |
| QuickJS FileChooserJs class | `crates/ferridriver-script/src/bindings/file_chooser.rs` |
| Rust integration tests | `crates/ferridriver-cli/tests/backends_support/file_chooser.rs` |
| NAPI filechooser tests | `crates/ferridriver-node/test/filechooser.test.ts` |
| Dialog handle + manager (§2.9) | `crates/ferridriver/src/dialog.rs` |
| Navigation `NavRequestSlot` (§3.1) | `crates/ferridriver/src/network.rs` |
| `StringOrRegex` + escapes (§3.12) | `crates/ferridriver/src/options.rs`, `locator.rs` |
| Rules + lessons | `CLAUDE.md` |
