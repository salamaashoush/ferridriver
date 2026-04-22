# Handover — next Playwright-parity session

Read-first for any session continuing work. Overwrite this file with a
fresh summary at the end of each block.

## Cross-device setup

1. `CLAUDE.md` — rules + lessons.
2. `PLAYWRIGHT_COMPAT.md` — gap tracker. Tier 1 done. §3.1, §3.12,
   §2.9, §2.11, §2.10, §2.12, §2.13, and now §2.14 landed in recent
   sessions.
3. This file — block summary below.
4. `docs/NEXT_SESSION.md` — next-block brief.

`git clone https://github.com/microsoft/playwright /tmp/playwright` if missing.

## Landed this session

### §2.14 — Video as first-class handle + context recordVideo option

Playwright-parity surface verified against
`/tmp/playwright/packages/playwright-core/types/types.d.ts:21621` (Video)
and `:4756` (page.video). Three layers, four backends green.

| Surface | Shape | Playwright ref |
|---|---|---|
| `page.video()` | `Video | null` | `types.d.ts:4756` |
| `Video.path()` | `Promise<string>` | `types.d.ts:21631` |
| `Video.saveAs(path)` | `Promise<void>` | `types.d.ts:21638` |
| `Video.delete()` | `Promise<void>` | `types.d.ts:21625` |
| `context.setRecordVideo({dir,size?})` | transitional until §4.1 | `types.d.ts:10150` |

- **Core `video.rs`**: wraps the existing `VideoRecordingHandle` in a
  public `Video` + paired `VideoSink`. The sink carries
  `watch::Sender<Option<FinalPath>>`; the Video blocks on
  `watch::Receiver::changed` inside every accessor until the sink
  announces the terminal state. `send_replace` on finalisation so
  callers that subscribe before the first publish still observe the
  value — mirrors §2.12's lazy-subscriber pattern.

- **Context option plumbing** (`options.rs::RecordVideoOptions`):
  `{ dir: PathBuf, size: Option<VideoSize> }`. Per-composite-key
  registry on `BrowserState::record_video` (sync `std::sync::Mutex`
  so `ContextRef::set_record_video` — a sync setter — doesn't need to
  own a tokio guard). Call `set_record_video` BEFORE opening a page
  — registry lookup happens in `ContextRef::new_page` after
  `Page::with_context` returns.

- **Recording runtime**
  (`context.rs::start_video_recording`): spawns a tokio task that
  runs `video::start_recording(page, output_path, w, h, quality=90)`
  → polls `page.is_closed()` at 50ms → `handle.stop(&page)` →
  `sink.finish_ok(path)` / `finish_err(reason)`. Filename format
  `<dir>/<millis>-<counter>.<ext>` (ext from `video_extension()`).

- **Backends**:
  - **CDP** (cdp-pipe/cdp-raw): real recording via the existing
    `Page.startScreencast` + ffmpeg encoder path. Default quality 90
    (Playwright parity).
  - **BiDi** (Firefox): polls at ~15fps via the backend's existing
    `start_screencast` polyfill (no native screencast primitive).
  - **WebKit**: `AnyPage::start_screencast` returns a typed
    `Unsupported`. The recording runtime funnels that into
    `VideoSink::finish_err(...)`, so `page.video()` STILL returns a
    non-null handle (Playwright parity: the class is always present
    when `recordVideo` is set) but its accessors reject with the
    backend reason. Section B gap documented.

- **NAPI** (`ferridriver-node/src/video.rs`): new `#[napi] class
  Video` with async `path()` / `saveAs(path)` / `delete()`.
  `page.video()` accessor on the `Page` class returns
  `Video | null`. `BrowserContext.setRecordVideo({ dir, size? })`
  as the transitional setter.

- **QuickJS** (`ferridriver-script/src/bindings/video.rs`): new
  `VideoJs` class. `PageJs::video()` accessor; new
  `BrowserContextJs::newPage()` (tests need it to open a recording
  page without closing the ambient `page` global that `run_script`
  binds); `BrowserContextJs::setRecordVideo(options)`.

### Rule-9 tests

- `tests/backends_support/video.rs` — 2 tests × 4 backends =
  **8 assertions**. `test_video_null_without_recording` asserts
  `page.video() === null` when no `recordVideo`.
  `test_video_recording_lifecycle` opens a fresh page via
  `context.newPage()` (so the ambient page isn't disrupted),
  navigates twice (two `goto`s give the screencast encoder a visible
  state transition without `setTimeout` — QuickJS doesn't have it),
  closes, awaits `video.path()`. CDP asserts file exists + non-empty
  size; BiDi asserts file exists; WebKit asserts `path()` rejects.
  Uses size 1280x720 to avoid ffmpeg "padded dimensions cannot be
  smaller than input" error on BiDi where Firefox renders at a
  larger viewport than the 800x450 default.
- `crates/ferridriver-node/test/video.test.ts`: 3 tests × 2 CDP
  backends = **6 assertions** covering null without recording,
  record-and-path-resolves, and saveAs/delete.

### Baseline after this commit (must stay green)

```
cargo clippy --workspace --all-targets -- -D warnings           # clean
cargo test -p ferridriver --lib                                  # 125 core
cargo test -p ferridriver-script --lib                           # 13 script
cargo test -p ferridriver-mcp --lib                              # 38 MCP
cd crates/ferridriver-node && bun run build:debug
cd <repo root> && bun test                                       # 853 (was 847)
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1
# cdp-pipe 140, cdp-raw 140, bidi 135, webkit 136
```

## Next priorities

1. **§4.1 BrowserContextOptions** — 28-field option bag at context
   creation (viewport, userAgent, locale, timezone, geolocation,
   permissions, acceptDownloads, recordVideo, recordHar, …). This
   folds today's transitional `set_record_video` setter into the full
   options struct. Probably 2–3 sessions.
2. **§2.15 BrowserType class** — remove ad-hoc `Browser::launch` /
   `Browser::connect` on `Browser`, introduce `BrowserType` with
   `launch`/`connect`/`connectOverCdp`/etc.
3. **§3.17 Auto-waiting deadline parity** — replace fixed backoff
   with Playwright's exponential polling + deadline propagation.
4. **WebKit rich IPC op** — one new `Op::RichConsoleEvent` carrying
   `args + stack frames + isError: bool` closes both §2.12 console-
   args/location AND §2.13 stack-richness gaps together.
5. **WebKit screencast bridge** — the §2.14 WebKit gap. Options: new
   Objective-C pipeline via `CGDisplayStreamCreate` + CoreVideo frame
   pump through IPC, or macOS 14+ `WKWebView._takeSnapshot` with
   frame scheduling. Closes the §2.14 WebKit gap.

## Carried-forward backend gaps (real protocol limits)

- **BiDi**: response body unavailable for non-intercepted responses
  (Firefox discards bytes; Playwright's own BiDi backend has the
  same limit). Multi-`Set-Cookie` collapses. `request.postData()`
  null for fetch-with-body. `Download.cancel` typed `Unsupported`
  (Firefox BiDi has no cancel primitive). Page-init emits a spurious
  `"Permission denied to access property 'length'"` cross-origin
  error observed by `pageerror` listeners — real Firefox behaviour,
  tests poll past it.
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
  **Screencast** has no public API — `page.video().path()` rejects
  with the typed `Unsupported` reason (§2.14). `page.evaluate` runs
  in utility context isolated from the user-script's fetch wrap.

## Known flakes

- `context.setOffline toggles network` on WebKit bun occasionally
  fails under the full suite but passes in isolation. Pre-existing.

## Key source locations

| area | path |
|---|---|
| Video handle + VideoSink | `crates/ferridriver/src/video.rs::{Video, VideoSink}` |
| RecordVideoOptions | `crates/ferridriver/src/options.rs::RecordVideoOptions` |
| Recording runtime | `crates/ferridriver/src/context.rs::start_video_recording` |
| record_video registry | `crates/ferridriver/src/state.rs::{record_video, set_record_video, get_record_video}` |
| `page.video()` accessor | `crates/ferridriver/src/page.rs::video` |
| NAPI Video class | `crates/ferridriver-node/src/video.rs` |
| NAPI context.setRecordVideo | `crates/ferridriver-node/src/context.rs` |
| QuickJS VideoJs | `crates/ferridriver-script/src/bindings/video.rs` |
| QuickJS context.newPage + setRecordVideo | `crates/ferridriver-script/src/bindings/context.rs` |
| Rust integration tests | `crates/ferridriver-cli/tests/backends_support/video.rs` |
| NAPI tests | `crates/ferridriver-node/test/video.test.ts` |
| WebError handle + ErrorDetails (§2.13) | `crates/ferridriver/src/web_error.rs` |
| ContextEvent + ContextEventEmitter (§2.13) | `crates/ferridriver/src/events.rs` |
| ConsoleMessage handle (§2.12) | `crates/ferridriver/src/console_message.rs` |
| Download handle (§2.10) | `crates/ferridriver/src/download.rs` |
| FileChooser handle (§2.11) | `crates/ferridriver/src/file_chooser.rs` |
| Dialog handle (§2.9) | `crates/ferridriver/src/dialog.rs` |
| Navigation `NavRequestSlot` (§3.1) | `crates/ferridriver/src/network.rs` |
| Rules + lessons | `CLAUDE.md` |
