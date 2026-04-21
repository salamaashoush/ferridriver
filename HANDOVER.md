# Handover — next Playwright-parity session

Read-first for any session continuing work. Overwrite this file with a
fresh summary at the end of each block.

## Cross-device setup

1. `CLAUDE.md` — rules + lessons.
2. `PLAYWRIGHT_COMPAT.md` — gap tracker. Tier 1 done. §3.1, §3.12,
   §2.9, §2.11, and now §2.10 landed in recent sessions.
3. This file — block summary below.
4. `docs/NEXT_SESSION.md` — next-block brief.

`git clone https://github.com/microsoft/playwright /tmp/playwright` if missing.

## Landed this session

### §2.10 — Download as first-class handle (this commit)

Live `Download` handle with `url()` / `suggestedFilename()` / `page()`
sync accessors + async `path()` / `saveAs(path)` / `cancel()` /
`delete()` / `failure()`. Dispatch follows the same synchronous-claim
pattern §2.9 / §2.11 shipped: backend listener builds the live
`Download` from the protocol's download-begin event and synchronously
calls `DownloadManager::did_open(&download)` — no broadcast race, no
grace-window polling.

- **`crates/ferridriver/src/download.rs`** — new module:
  - `Download { url, suggested_filename, page (weak), guid,
    downloads_dir, local_path, canceler, state_tx, deleted }` behind
    `Arc`. Terminal state on `tokio::sync::watch::Sender<DownloadStatus>`
    (`Pending` / `Finished { path }` / `Failed { error }`); `path()` /
    `failure()` subscribe and await the transition. `saveAs` = await
    path + `tokio::fs::copy` (matches Playwright's `_localPathAfterFinished`
    + copy flow). `delete` = await path + unlink, idempotent.
  - **Critical correctness note**: `report_finished` uses
    `watch::Sender::send_replace`, **not** `send`. `send` silently
    discards the value when `receiver_count() == 0`, which causes any
    later `path()` / `failure()` caller to hang forever on
    `changed().await` — a real race because the backend's terminal
    progress event can arrive before user code subscribes. Documented
    inline so the fix isn't regressed.
  - `DownloadManager` per-page registry: `add_handler(Fn(&Download) ->
    bool) -> DownloadHandlerId`, `remove_handler(id)`,
    `did_open(&Download)`, `take_for_guid(guid)` / `peek_for_guid(guid)`
    for the backend listener's terminal-event lookup. **Unclaimed
    downloads are not auto-cancelled** — matches Playwright's server,
    which emits `Page.Events.Download` and leaves the bytes in
    `downloadsPath`. The per-page `Arc<TempDir>` drop cleans up
    orphans.
  - `DownloadManager::register_emitter_bridge(events)` — default
    handler installed once per page so
    `page.events().on("download", cb)` keeps delivering live handles.
  - Removed the old `Page::wait_for_download(url_pattern, timeout)` +
    `Page::expect_download` + `DownloadInfo` wire struct. Both were
    Rule-3 violations (exposed `{guid, url, suggested_filename}` as a
    user-facing type). Callers now use `page.waitForEvent('download')`.

- **Backends** — all three wire the same pattern:
  - CDP (`backend/cdp/mod.rs::spawn_download_listener`): subscribes to
    the transport event stream **first**, then sends
    `Browser.setDownloadBehavior({ behavior: 'allowAndName',
    downloadPath: <tempdir>, eventsEnabled: true })` — same ordering
    rationale as §2.11 (fast click can beat the enable reply).
    `allowAndName` writes each download as `<downloadPath>/<guid>` so
    parallel downloads don't collide on filename. On
    `Browser.downloadWillBegin`: reads `guid` + `url` +
    `suggestedFilename`, upgrades the page backref, builds `Download`
    with a canceler that issues `Browser.cancelDownload`, synchronously
    calls `did_open`. On `Browser.downloadProgress state: 'completed'
    | 'canceled'`: `take_for_guid` + `report_finished`.
  - BiDi (`backend/bidi/page.rs`): same for
    `browsingContext.downloadWillBegin` + `browsingContext.downloadEnd`.
    `downloadEnd.filepath` overrides the default
    `<downloads_dir>/<guid>` path with the real absolute path Firefox
    wrote to. BiDi cancel is typed `Unsupported` because Firefox's
    BiDi has no cancel primitive — Playwright's own BiDi backend
    leaves `cancelDownload` as a no-op (`bidiBrowser.ts:527`). Rule-4
    honest.
  - WebKit: stock `WKWebView` routes downloads through
    `WKDownloadDelegate` in the host's Obj-C subprocess and those
    events don't currently flow through our IPC. The manager is wired
    for API parity (bridge registered so `page.on('download', cb)`
    doesn't error), but no event ever dispatches and
    `Page::wait_for_download` times out honestly. Scoped as a future
    phase — wiring requires a new `WKDownload` delegate class on the
    host side + ~3 new IPC ops. Documented in the `download_manager`
    field's doc comment.

- **Page API** — new `Page::wait_for_download(timeout_ms) ->
  Result<Download>`: registers a one-shot handler with the
  `DownloadManager`, awaits a `tokio::sync::oneshot`, removes the
  handler on resolve / timeout. Typed `Timeout` / `TargetClosed`
  errors. NAPI + QuickJS route `page.waitForEvent('download')` through
  it so the claim is synchronous with the browser event.

- **NAPI** (`crates/ferridriver-node/src/download.rs`): new `#[napi]
  class Download`. `page.waitForEvent('download')` returns it via a
  new `Either7<Request, Response, WebSocket, Dialog, FileChooser,
  Download, Value>` — generated `.d.ts` matches Playwright's
  `Promise<Request | Response | WebSocket | Dialog | FileChooser |
  Download | Record<string, any>>`. `page()` returns `Page` non-null.
  `createReadStream` deferred to future NAPI parity pass — callers use
  `fs.createReadStream(await download.path())`.

- **QuickJS** (`crates/ferridriver-script/src/bindings/download.rs`):
  new `DownloadJs`. `page.waitForEvent('download')` instantiates it.
  Same sync/async surface minus `page()` (symmetric with
  `FileChooserJs`).

### Rule-9 tests

- `tests/backends_support/download.rs`: webkit-unsupported timeout +
  save-as byte-for-byte round-trip + path() contents + cdp cancel-then-failure
  + bidi cancel-typed-Unsupported. Stand up a local attachment server
  (HTTP stub serving `Content-Disposition: attachment`) — downloads
  trigger via `<a href="/file.bin">` click. All four backends green.
- `crates/ferridriver-node/test/download.test.ts`: saveAs + path +
  cancel-failure + waitForEvent timeout. 4 tests × 2 CDP backends = 8
  assertions (`node:http` attachment server per test, no shared
  fixture state).

### Baseline after this commit (must stay green)

```
cargo clippy --workspace --all-targets -- -D warnings         # clean
cargo test -p ferridriver --lib                                # 125 core
cargo test -p ferridriver-script --lib                         # 22 script
cargo test -p ferridriver-mcp --lib                            # 38 MCP
cd crates/ferridriver-node && bun run build:debug
cd <repo root> && bun test                                     # 825 (was 817)
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1
# cdp-pipe 132, cdp-raw 132, bidi 127, webkit 128
```

## Next priorities

1. **§2.12 ConsoleMessage rich** — replace `ConsoleMsg { type, text }`
   with the full Playwright `ConsoleMessage { args: Vec<JSHandle>,
   location, page, text, type, timestamp }`. Blocks on JSHandle
   parity (§1.3) which is mostly in place.
2. **§4.1 BrowserContextOptions** — 28-field option bag at context
   creation (viewport, userAgent, locale, timezone, geolocation,
   permissions, acceptDownloads, ...). Probably 2–3 sessions.
3. **§3.17 Auto-waiting deadline parity** — replace fixed backoff
   with Playwright's exponential polling + deadline propagation.
4. **WebKit Download bridge** — add `WKDownloadDelegate` to `host.m`
   + IPC op routing to unblock downloads on WebKit. Closes the
   documented §2.10 WebKit gap.

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
  (§2.9: `Dialog.accept/dismiss` returns typed `Unsupported`). **File
  chooser** cannot be intercepted — no event flows through our IPC
  (§2.11: `Page::wait_for_file_chooser` times out, documented).
  **Download** events route through `WKDownloadDelegate` in the host
  subprocess and don't currently flow through our IPC (§2.10:
  `Page::wait_for_download` times out). `page.evaluate` runs in
  utility context isolated from the user-script's fetch wrap.

## Known flakes

- `context.setOffline toggles network` on WebKit bun occasionally
  fails under the full suite but passes in isolation. Pre-existing.

## Key source locations

| area | path |
|---|---|
| Download handle + manager | `crates/ferridriver/src/download.rs` |
| `send_replace` correctness note | `Download::report_finished` docstring |
| CDP download listener | `crates/ferridriver/src/backend/cdp/mod.rs::spawn_download_listener` |
| BiDi download listener | `crates/ferridriver/src/backend/bidi/page.rs` (`browsingContext.downloadWillBegin` / `downloadEnd` arms) |
| WebKit download (no-op bridge) | `crates/ferridriver/src/backend/webkit/mod.rs::attach_listeners` |
| Page::wait_for_download | `crates/ferridriver/src/page.rs` |
| NAPI Download class | `crates/ferridriver-node/src/download.rs` |
| QuickJS DownloadJs class | `crates/ferridriver-script/src/bindings/download.rs` |
| Rust integration tests | `crates/ferridriver-cli/tests/backends_support/download.rs` |
| NAPI download tests | `crates/ferridriver-node/test/download.test.ts` |
| FileChooser handle + manager (§2.11) | `crates/ferridriver/src/file_chooser.rs` |
| PageBackref helper | `crates/ferridriver/src/backend/mod.rs::PageBackref` |
| Dialog handle + manager (§2.9) | `crates/ferridriver/src/dialog.rs` |
| Navigation `NavRequestSlot` (§3.1) | `crates/ferridriver/src/network.rs` |
| `StringOrRegex` + escapes (§3.12) | `crates/ferridriver/src/options.rs`, `locator.rs` |
| Rules + lessons | `CLAUDE.md` |
