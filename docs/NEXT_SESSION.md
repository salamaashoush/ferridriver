# Next session — §2.14 Video

Tier 1 done. §3.1, §3.12, §2.9, §2.11, §2.10, §2.12, §2.13 landed.
Next pick: **§2.14 Video** — live handle for
`page.video()` carrying `path() -> Future<String>`, `save_as(path)`,
`delete()`. Playwright-equivalent of the `Video` client class wired to
the `record_video` context option.

## Read-first

1. `CLAUDE.md` — rules + lessons.
2. `PLAYWRIGHT_COMPAT.md` — §2.14 is next; §2.13 just landed.
3. `HANDOVER.md` — §2.13 WebError summary.
4. `/tmp/playwright/packages/playwright-core/src/client/video.ts`
   + `/tmp/playwright/packages/playwright-core/src/server/page.ts`
   (`PageVideo` section) for the server-side recording → artifact
   flow.

## §2.14 canonical surface

```ts
class Video {
  path(): Promise<string>;
  saveAs(path: string): Promise<void>;
  delete(): Promise<void>;
}

// access: page.video() — null when record_video wasn't enabled
page.video(): Video | null
```

The video file becomes available only after `page.close()` — `path()`
resolves once the backend finishes the recording and the artifact is
on disk. Playwright's Chrome backend uses `Target.attachToTarget` with
`Page.startScreencast` under the hood for BiDi/Chromium flows;
ferridriver already has a `VideoRecordingHandle` in
`crates/ferridriver/src/video.rs` that can be wrapped.

## Implementation sketch

1. **Rust core — wrap existing `crates/ferridriver/src/video.rs`**:
   - Promote `VideoRecordingHandle` to a public `Video` API with
     `path()`, `save_as(path)`, `delete()`.
   - `page.video() -> Option<Arc<Video>>`. `None` when the page's
     context wasn't created with `record_video`.
   - Ensure `save_as` and `delete` wait for the recording file to be
     finalised before operating (the recording finalises on
     `page.close()` — gate `path()` on a `tokio::sync::watch` signal).

2. **Context option plumbing**:
   - `record_video: Option<RecordVideoOptions { dir, size? }>` on the
     context creation path. Today the option exists as a
     `ViewportConfig` shape — verify it wires end-to-end.

3. **Per-backend recording**:
   - **CDP**: `Page.startScreencast` + manual frame-assembly via
     ffmpeg (already present in `crates/ferridriver/src/ffmpeg.rs`).
   - **BiDi**: Firefox BiDi has no first-class screencast primitive;
     Playwright itself drops video on Firefox except via its own
     extension. Typed `FerriError::Unsupported` is correct for now —
     document under Section B.
   - **WebKit**: `WKWebView` has no public screencast API. Typed
     `Unsupported`. Document Section B.

4. **NAPI / QuickJS**:
   - `#[napi] class Video` with `path()` / `saveAs(path)` /
     `delete()`. `page.video()` returns `Video | null`.
   - `VideoJs` script binding same surface.

5. **Rule-9 integration tests** (all four backends where supported):
   - Create context with `record_video: { dir: '/tmp/...' }`.
   - Navigate, interact, close.
   - Assert `video.path()` resolves to an existing file.
   - `video.save_as('/tmp/x.webm')` copies it; `video.delete()`
     removes it.
   - BiDi + WebKit: assert `Unsupported` error is returned, document
     under Section B.

## Ground rules (from CLAUDE.md)

- Rule 1/2/3: core is source of truth; three layers update in the
  same commit; no wire shapes leak.
- Rule 4: every backend real (or typed `Unsupported` for genuine
  protocol limits).
- Rule 6: read `/tmp/playwright/...` first.
- Rule 7: rebuild NAPI + diff `.d.ts` against Playwright's types.
- Rule 9: per-backend integration test before flipping `[x]`.
- Rule 10: no escape hatches.

## Baseline (must stay green)

```
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p ferridriver --lib                                 # 125 core
cargo test -p ferridriver-script --lib                          # 13 script
cargo test -p ferridriver-mcp --lib                             # 38 MCP
cd crates/ferridriver-node && bun run build:debug
cd <repo root> && bun test                                      # 845
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1
# cdp-pipe 138, cdp-raw 138, bidi 133, webkit 134
```

## Commit shape

Single commit: `feat(page): Video as first-class handle (§2.14)`.
Update `PLAYWRIGHT_COMPAT.md` §2.14 to `[x]` and rewrite `HANDOVER.md`
+ `docs/NEXT_SESSION.md` in the same commit.

## Notes from §2.13 that generalise

- **Shared emitter registry pattern** — when a handle type (Video,
  future context events) needs to share identity across `ContextRef`
  clones with the same key, use `BrowserState::context_events`'s
  pattern: a sync `std::sync::Mutex<HashMap<...>>` looked up via
  `get_or_create_context_events` at `ContextRef::new`. Avoids the
  tokio-RwLock-in-sync-construction problem.
- **Per-page → per-context event bridge** — installed exactly once in
  `BrowserState::register_opened_page` (not `Page::with_context`), so
  it works regardless of whether the page is wrapped via `Page::new`
  (MCP server) or `Page::with_context` (direct ContextRef usage).
- **Polling for matching events vs. asserting the first** — Rule-9
  tests that exercise event routes should poll for a specific
  identifier embedded in the event payload. Firefox BiDi emits a
  spurious cross-origin "Permission denied" error at page init that
  would land first for any naive `waitForEvent('pageerror')` test.
- **Separating `!Send` `Function<'_>` from async futures** — when
  writing NAPI `async fn` handlers that take a listener function,
  lower the `Function<'_>` to a pure-`Send` `Callback` in a separate
  sync helper (`build_context_event_callback` pattern). The async
  macro otherwise captures the raw JS value pointer across await
  points and rejects the generated future.
- **`write!` into `String`** — clippy flags
  `s.push_str(&format!(...))`. Use `use std::fmt::Write as _; let _ =
  write!(s, ...);` to avoid the intermediate allocation.
- **`Runtime.exceptionThrown` vs. `Runtime.consoleAPICalled`** — the
  two listeners must NOT share code paths. Exception events have
  `exceptionDetails.exception.description` (full stack + message),
  console events have `args` + `stackTrace`. `cdp_exception_to_error_details`
  and `cdp_remote_object_to_backing` (§2.12) are the canonical
  helpers for each.
