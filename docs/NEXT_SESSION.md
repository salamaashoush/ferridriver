# Next session — §2.10 Download as handle

Tier 1 done. §3.1, §3.12, §2.9, §2.11 landed. Next pick:
**§2.10 Download as handle** — same live-event pattern as the three
most recent blocks.

## Read-first

1. `CLAUDE.md` — rules + lessons.
2. `PLAYWRIGHT_COMPAT.md` — §2.10 is the next item; §2.11 just landed
   with full detail.
3. `HANDOVER.md` — §2.11 FileChooser summary (load-bearing — the
   `FileChooserManager` + `PageBackref` + per-event async resolver
   pattern generalises).
4. `/tmp/playwright/packages/playwright-core/src/client/download.ts`
   + `/tmp/playwright/packages/playwright-core/src/server/download.ts`.

## §2.10 canonical surface

```ts
class Download {
  page(): Page;
  url(): string;
  suggestedFilename(): string;
  path(): Promise<string | null>;
  createReadStream(): Promise<Readable>;
  saveAs(path: string): Promise<void>;
  cancel(): Promise<void>;
  delete(): Promise<void>;
  failure(): Promise<string | null>;
}

page.on('download', (download: Download) => { ... });
const download = await page.waitForEvent('download');
```

## Implementation sketch (follow §2.11)

1. **Rust core — new `crates/ferridriver/src/download.rs`**:
   - `Download { url, suggested_filename, page, guid, completion_state: Arc<DownloadState> }`
     where `DownloadState` tracks `path | failure | cancelled` with
     `tokio::sync::watch` or a `Notify`.
   - `save_as(path)` / `path()` / `failure()` / `cancel()` / `delete()` —
     most route through CDP `Browser.setDownloadBehavior` /
     `Page.downloadWillBegin` / `Page.downloadProgress` events plus
     direct filesystem ops on the cached path.
   - `DownloadManager` per-context registry mirroring
     `FileChooserManager` / `DialogManager`: `add_handler(Fn(&Download) -> bool)`,
     `remove_handler`, `did_open(&Download)`, `register_emitter_bridge`.

2. **Per-backend listeners**:
   - **CDP**: already have `Page.downloadWillBegin` surfacing on
     `NetworkTracker::on_download_will_begin`. Upgrade to emit a live
     `Download`, thread `Page.downloadProgress` through to the
     completion state. `Browser.setDownloadBehavior({ behavior: 'allow',
     downloadPath: <tempdir> })` at browser init or per context.
   - **BiDi**: `browsingContext.downloadWillBegin` (check exact name).
   - **WebKit**: the `WKDownload` delegate runs in the host subprocess;
     either add an IPC op that routes the decision + bytes back, or
     typed `Unsupported` per Rule 4.

3. **Page API**:
   - `page.wait_for_download(timeout) -> Result<Download>` — one-shot,
     same pattern as `wait_for_dialog` / `wait_for_file_chooser`.

4. **NAPI / QuickJS**:
   - `#[napi] class Download` / `DownloadJs`.
   - `waitForEvent('download')` routes through `wait_for_download`.
   - Extend the NAPI `Either6` return to include `Download` (or keep the
     snapshot path for download and provide a dedicated method — TBD).

5. **Rule-9 integration tests**:
   - Trigger a download via a `<a download>` click or a
     `Content-Disposition: attachment` response.
   - `saveAs(tempPath)` + read the file → assert contents match.
   - `failure()` after a cancel.
   - Per-backend on all four backends — WebKit asserts typed `Unsupported`
     (or an honest gap documented in compat).

## Ground rules (from CLAUDE.md)

- Rule 1/2: core is source of truth; three layers update in the same commit.
- Rule 4: every backend real — typed `Unsupported` only where the
  protocol genuinely can't.
- Rule 6: read `/tmp/playwright/...` before each signature.
- Rule 7: rebuild NAPI + diff `.d.ts` against Playwright's `test.d.ts`.
- Rule 9: per-backend integration test on every backend before flipping `[x]`.
- Rule 10: no `#[allow(clippy::*)]` escape hatches.

## Baseline (must stay green)

```
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --lib                            # 122 core + 29 script + 38 MCP
cd crates/ferridriver-node && bun run build:debug
cd <repo root> && bun test                              # 817
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1
```

## Commit shape

Single commit: `feat(page): Download as first-class handle + page.waitForEvent('download') (§2.10)`.
Update `PLAYWRIGHT_COMPAT.md` §2.10 to `[x]` and rewrite
`HANDOVER.md` + `docs/NEXT_SESSION.md` in the same commit.

## Notes from §2.11 that generalise

- **`PageBackref` is shared infra now** (`backend/mod.rs`). `Download`
  can reuse it as-is — no need for a separate helper.
- **QuickJS busy-wait Promise trick hangs forever on async
  `document.title` updates**. Page-side change handlers should set
  the title synchronously so the `run_script` observer doesn't need
  to sleep. (This bit §2.11's FilePayload test — we now split async
  vs sync forms between NAPI and QuickJS.)
- **`Page.setInterceptFileChooserDialog` must be sent AFTER
  subscribing to events**. Same principle likely applies to any
  future `Page.setDownloadBehavior`-adjacent command: subscribe first,
  enable second.
