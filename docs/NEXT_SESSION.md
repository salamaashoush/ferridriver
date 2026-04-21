# Next session — §2.11 FileChooser

Tier 1 done. §3.1, §3.12, §2.9 landed. Next pick: **§2.11 FileChooser**
— same live-event-handle pattern as §2.9 Dialog, so the plumbing is
now well-understood.

## Read-first

1. `CLAUDE.md` — rules + lessons.
2. `PLAYWRIGHT_COMPAT.md` — §2.11 is the next item.
3. `HANDOVER.md` — §2.9 Dialog summary (load-bearing for this block —
   same DialogManager-shaped pattern applies).
4. `/tmp/playwright/packages/playwright-core/src/client/fileChooser.ts`
   + `/tmp/playwright/packages/playwright-core/src/server/fileChooser.ts`.

## §2.11 canonical surface

```ts
class FileChooser {
  element(): ElementHandle;
  isMultiple(): boolean;
  page(): Page;
  setFiles(files: string | string[] | FilePayload | FilePayload[],
           options?: { timeout?: number, noWaitAfter?: boolean }): Promise<void>;
}

page.on('filechooser', (chooser: FileChooser) => { ... });
// or
const chooser = await page.waitForEvent('filechooser');
```

## Implementation sketch (follow §2.9)

1. **Rust core — new `crates/ferridriver/src/file_chooser.rs`**:
   - `FileChooser { inner: Arc<FileChooserState> }`.
   - State holds: `Arc<Page>`, `is_multiple: bool`, `element: ElementHandle`, `handled: AtomicBool`, optional manager back-ref.
   - `set_files(files, opts)` runs `element.set_input_files(files, opts)` through the existing §1.5 plumbing, then flips `handled`.
   - `FileChooserManager` mirrors `DialogManager`: `add_handler(Fn(&FileChooser) -> bool) -> id`, `remove_handler`, `did_open(chooser)`.
   - `register_emitter_bridge(events)` — bridge to `PageEvent::FileChooser` for `page.on('filechooser', cb)`.

2. **Per-backend listeners**:
   - **CDP**: subscribe to `Page.fileChooserOpened`. The event carries
     `backendNodeId` + `mode: 'selectSingle' | 'selectMultiple'`. Build
     an `ElementHandle` pinned to the node (via `DOM.resolveNode`) and
     a `FileChooser`, then `manager.did_open(chooser)`. Must call
     `Page.setInterceptFileChooserDialog({ enabled: true })` so the
     native picker doesn't open.
   - **BiDi**: Firefox's `input.fileDialogOpened` event (check if
     available — may need typed `Unsupported`).
   - **WebKit**: stock `WKWebView` has no public API for intercepting
     the file picker. Typed `Unsupported`.

3. **Page API**:
   - `page.wait_for_file_chooser(timeout) -> Result<FileChooser>` —
     one-shot, same pattern as `wait_for_dialog`.

4. **NAPI / QuickJS**:
   - `#[napi] class FileChooser` / `FileChooserJs`.
   - `waitForEvent('filechooser')` routes through `wait_for_file_chooser`.
   - `page.on('filechooser', cb)` flows through `PageEvent::FileChooser`
     via the emitter-bridge.

5. **Rule-9 integration tests**:
   - Trigger `<input type=file>` click via `page.locator('input').click()`.
   - `setFiles` with string path / string[] / `FilePayload`. Verify the
     form's submitted value.
   - `isMultiple` true / false verified against `<input type=file multiple>`.
   - Per-backend on all four backends — WebKit asserts typed `Unsupported`.

## Ground rules (from CLAUDE.md)

- Rule 1/2: core is source of truth; three layers update in the same commit.
- Rule 4: every backend real — typed `Unsupported` only where the
  protocol genuinely can't (WebKit file picker).
- Rule 6: read `/tmp/playwright/...` before each signature.
- Rule 7: rebuild NAPI + diff `.d.ts` against Playwright's `test.d.ts`.
- Rule 9: per-backend integration test on every backend before flipping `[x]`.
- Rule 10: no `#[allow(clippy::*)]` escape hatches.

## Baseline (must stay green)

```
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --lib                            # 122 core
cd crates/ferridriver-node && bun run build:debug
cd <repo root> && bun test                              # 809 bun
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1
```

## Commit shape

Single commit: `feat(page): FileChooser as first-class handle + page.waitForEvent('filechooser') (§2.11)`.
Update `PLAYWRIGHT_COMPAT.md` §2.11 to `[x]` and rewrite
`HANDOVER.md` + `docs/NEXT_SESSION.md` in the same commit.
