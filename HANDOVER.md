# Handover — next Playwright-parity session

Read-first for any session continuing work. Overwrite this file with a
fresh summary at the end of each block.

## Cross-device setup

Everything needed is committed. Next session should read, in order:

1. `CLAUDE.md` — the Playwright-parity rules and consolidated
   lessons. Authoritative cross-device source.
2. `PLAYWRIGHT_COMPAT.md` — gap tracker. §1.4 is now the last
   remaining Tier-1 item.
3. This file — block-level commit summary below.

Set up the cloned Playwright at `/tmp/playwright` if it isn't there:

```bash
git clone https://github.com/microsoft/playwright /tmp/playwright
```

## What just landed (2026-04-19)

§1.5 closed out. Commit `28f7a04` shipped the four per-option
Rule-9 tests (dblclick, press, type, setInputFiles) that were owed
before the top-level §1.5 checkbox could flip. Three real bugs
surfaced during testing got fixed in the same commit.

### Per-option coverage landed

- **dblclick** — modifier / position / delay / trial / button
  probes on all 4 backends + NAPI. `button:'right'` emits
  trusted `contextmenu` events with `event.button === 2`.
- **press** — `delay:120` keydown→keyup gap ≥ 80ms; `delay:0`
  <80ms; `noWaitAfter` returns within bounds.
- **type** — `delay:50` per-char gap ≥ 35ms across 3 strokes;
  `delay:0` completes three strokes in <1s.
- **setInputFiles** — all four polymorphic forms
  (`string | string[] | FilePayload | FilePayload[]`) on every
  backend.

### Bugs fixed (uncovered by the new tests)

- `locator::set_input_files` for `FilePayload`s used to prefix the
  on-disk filename with `{i}-` and delete temp files right after
  `upload_file` returned. The page saw `"0-payload.txt"` with
  `size: 0` because CDP reads bytes lazily when the page accesses
  `file.size` — after our cleanup ran. Fix: each payload gets its
  own subdir (`<tmpdir>/<upload-id>-<idx>/<name>`), and the cleanup
  is dropped. Temp files share the process-scoped root so they get
  reaped at shutdown.
- WebKit `Op::SetFileInput`: the per-path handler overwrote the
  `DataTransfer` on every call, so `<input type=file multiple>`
  only ever saw the last file. Fixed in `host.m` to append into a
  live `DataTransfer` that preserves `el.files`. Rust-side now
  resets `el.value = ''` before the first append so the whole
  `setInputFiles` call still *replaces* the prior selection
  (matches Playwright).
- NAPI `setInputFiles` bindings now declare
  `ts_args_type = "files: string | string[] | FilePayload | FilePayload[], options?: SetInputFilesOptions"`
  on all four sites (Locator / ElementHandle / Frame / Page). The
  generated `.d.ts` no longer leaks the internal `NapiInputFiles`
  type (Rule 3).

Tests that must stay green and are green after this commit:

```
cargo clippy --workspace --all-targets -- -D warnings   # clean
cargo test --workspace --lib                            # 119 core
cd crates/ferridriver-node && bun run build:debug && bun test   # 742 bun
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1   # 4/4
```

Per `PLAYWRIGHT_COMPAT.md`: §1.5 is now [x]. Four per-option
Rule-9 tests landed with a new shared file
`crates/ferridriver-cli/tests/backends_support/action_options.rs`
(don't extend `backends.rs` directly — add new test groups under
`backends_support/`).

## Next session: §1.4 — Request / Response / WebSocket lifecycle

`PLAYWRIGHT_COMPAT.md` §1.4 is the last Tier-1 item and the
largest. It was deferred from this session because landing it
half-done (e.g. CDP-only, or signatures without Rule-9 tests)
would violate Rule 4 and Rule 9 — worse than the gap.

Canonical Playwright sources (read first):

- `/tmp/playwright/packages/playwright-core/src/client/network.ts`
  — `Request` (~line 60), `Response` (~line 370), `WebSocket`
  (~line 770).
- `/tmp/playwright/packages/playwright-core/src/server/network.ts`
  — server-side lifecycle each backend delivers.
- `/tmp/playwright/packages/playwright-core/src/server/bidi/bidiNetworkManager.ts`
  — BiDi's `network.*` events + WebSocket-in-BiDi caveats.

### Ferridriver state today

- `NetRequest` / `NetResponse` DTOs at
  `crates/ferridriver/src/context.rs:29` and
  `crates/ferridriver/src/events.rs:59`.
- CDP plumbing in `crates/ferridriver/src/backend/cdp/mod.rs`
  handles `Network.requestWillBeSent` +
  `Network.responseReceived` + `loadingFinished` / `loadingFailed`
  today; those update `NetRequest` records in `network_log` and
  emit `PageEvent::Request` / `PageEvent::Response` DTOs.
- No BiDi or WebKit network plumbing yet — context's
  `network_requests()` reads only what CDP emits.
- `MCP tools/network.rs` exposes the DTO shape as tool output.
- No NAPI or QuickJS binding surfaces for network objects beyond
  `page.on('request', ...)` / `page.on('response', ...)` snapshots.

### What §1.4 requires

1. **Core types** in a new `crates/ferridriver/src/network.rs`:
   `Request`, `Response`, `WebSocket`, `RequestTiming`,
   `RequestSizes`, `SecurityDetails`, `ServerAddr`. Use an
   `Arc<NetworkState>` shared between the page event loop and
   every live object so the promise-returning accessors
   (`request.response()`, `response.finished()`,
   `response.body()`) resolve against the current state.
2. **Replace the DTOs.** Delete `NetRequest` / `NetResponse` from
   `context.rs` / `events.rs`; update `PageEvent::Request` /
   `PageEvent::Response` to carry the new objects. Update
   `Page::on_request` / `on_response` / `on_request_finished` /
   `on_request_failed` / `on_websocket` to emit typed objects —
   live references, not snapshots.
3. **Per-backend plumbing**:
   - CDP: wire the existing `requestWillBeSent` /
     `requestWillBeSentExtraInfo` / `responseReceived` /
     `responseReceivedExtraInfo` / `loadingFinished` /
     `loadingFailed` / `webSocketFrameSent` /
     `webSocketFrameReceived` into the new `NetworkState`.
   - BiDi: `network.beforeRequestSent` + `responseStarted` +
     `responseCompleted` + `fetchError`. WebSocket events have a
     different shape — mirror `bidiNetworkManager.ts`.
   - WebKit: add `Op::GetResponseBody` / `Op::GetPostData` IPC
     ops; `host.m` can use `WKURLSchemeTask` /
     `_WKNetworkingSession` for body access. Follow Playwright's
     own WebKit backend for what's skippable.
4. **NAPI + QuickJS bindings** for each class with getters for
   sync fields and async methods for the promise-returning ones.
   Diff generated `index.d.ts` against Playwright's `test.d.ts`
   after each rebuild (Rule 7).
5. **Rule-9 integration tests on all 4 backends**, minimum six
   buckets from the `docs/FINISH_TIER1.md` plan (kept verbatim —
   see the plan file in git history): redirect chain, request
   failure, response body, post data, headers, WebSocket.
6. **Flip `§1.4 [ ] → [x]`** in `PLAYWRIGHT_COMPAT.md` with the
   commit SHA and test-file references.

### Ground rules reminder

- Rule 1: core is source of truth; bindings are thin delegators.
- Rule 2: all three layers update in the same commit.
- Rule 4: every backend real — `FerriError::Unsupported` only
  where the protocol genuinely can't.
- Rule 6: read `/tmp/playwright/...` before coding each signature.
- Rule 7: rebuild NAPI + diff generated `index.d.ts` after every
  binding change.
- Rule 9: per-option integration test on every backend before
  flipping `[x]`.
- Rule 10: no `#[allow(clippy::*)]` escape hatches.
- No task / phase / rule-number annotations in source comments or
  filenames.
- No emojis. No AI attribution in commit messages.

### Current baseline (everything green before starting §1.4)

```
cargo clippy --workspace --all-targets -- -D warnings   # clean
cargo test --workspace --lib                            # 119 core
cd crates/ferridriver-node && bun run build:debug && bun test   # 742 bun
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1   # 4/4 incl. bidi
```

## Known flakes

- `context.setOffline toggles network` on WebKit bun occasionally
  fails under the full suite but passes in isolation. Pre-existing
  state leak, unrelated to recent work.

## Key source locations

| area | path |
|---|---|
| Shared integration-test support | `crates/ferridriver-cli/tests/backends_support/` |
| Action-options Rule-9 tests | `crates/ferridriver-cli/tests/backends_support/action_options.rs` |
| Existing network DTOs (to delete for §1.4) | `crates/ferridriver/src/context.rs::NetRequest`, `crates/ferridriver/src/events.rs::NetResponse` |
| CDP network event handlers | `crates/ferridriver/src/backend/cdp/mod.rs` (~line 2562) |
| MCP network tool surface | `crates/ferridriver-mcp/src/tools/network.rs` |
| Existing route interception | `crates/ferridriver/src/route.rs`, `crates/ferridriver-node/src/route.rs` |
| Rules + lessons | `CLAUDE.md` |
