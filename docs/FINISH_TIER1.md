# Prompt — finish Tier 1

Paste this entire file into a fresh Claude Code session on any device.
No local-device state is needed.

---

Your job: close out Tier 1 of `PLAYWRIGHT_COMPAT.md`. Two items
remain:

1. **§1.5 per-option tests** (4 methods owed): dblclick, press, type,
   setInputFiles. Landing these flips the top-level `[~]` → `[x]`.
2. **§1.4 Request / Response / WebSocket** as first-class lifecycle
   objects — the largest remaining Tier-1 piece.

Ship §1.5 first (small, unblocks the checkbox), then §1.4.

## Read first

1. `CLAUDE.md` — Playwright-parity rules. Rule 1, 4, 6, 7, 9, 10 are
   the binding constraints. Rule 9 specifically: no flipping `[x]`
   without per-option integration tests on all 4 backends.
2. `HANDOVER.md` — block-level summary of the consolidation that
   preceded this session.
3. `PLAYWRIGHT_COMPAT.md` §1.4 and §1.5 — the acceptance criteria.
4. `/tmp/playwright/packages/playwright-core/src/client/network.ts` —
   canonical `Request` / `Response` / `WebSocket` shapes (949 lines).
   Don't reconstruct from memory. If `/tmp/playwright` is empty,
   `git clone https://github.com/microsoft/playwright /tmp/playwright`
   first.
5. `/tmp/playwright/packages/playwright-core/src/server/network.ts` —
   server-side lifecycle the backends deliver.

---

## Part A — §1.5: 4 per-option tests owed

Each test must pass on all four backends (cdp-pipe, cdp-raw, bidi,
webkit) via `crates/ferridriver-cli/tests/backends.rs`, AND as a
NAPI test in `crates/ferridriver-node/test/browser.test.ts`. Rule 9:
prove the option took page-visible effect, not just that the call
didn't error.

### A.1 Dblclick

- `DblClickOptions` lowers to `ClickOptions { click_count: Some(2) }`.
- Page-visible probe: install a `ondblclick` handler that sets
  `el.dataset.d = '1'`. After `locator.dblclick(opts)`, read back the
  attribute.
- Cover: `delay: 50`, `button: 'right'`, `modifiers: ['Shift']`,
  `position: {x, y}`, `trial: true` (no dispatch — data-attr stays
  unset), `timeout: 200` (missing selector → TimeoutError).

### A.2 Press

- `PressOptions { delay, no_wait_after, timeout }`.
- Page-visible probe: listen on `keydown` / `keyup`, record
  `performance.now()` on each into `el.dataset`. `press('A', { delay: 120 })`
  should leave a `keyup - keydown` gap of **≥ 80ms** (conservative
  floor — actual is ~120ms minus jitter).
- Cover: `delay: 120` measurable gap; `timeout: 200` → TimeoutError;
  `no_wait_after: true` returns without blocking.

### A.3 Type / pressSequentially

- `TypeOptions { delay, no_wait_after, timeout }`.
- Page-visible probe: `<input>` with `oninput` recording
  `performance.now()` per keystroke into a `data-marks` JSON array.
  `type('abc', { delay: 50 })` should have inter-keystroke gaps
  averaging ≥ 35ms across the 3 strokes.
- Cover: `delay: 50` per-char gap; `timeout: 200` → TimeoutError.

### A.4 SetInputFiles

- Polymorphic `string | string[] | FilePayload | FilePayload[]`.
- Cover all four forms on every backend:
  - single path string → `input.files[0].name`.
  - array of path strings → 2 files.
  - single `FilePayload { name, mimeType, buffer }` → `input.files[0].type`.
  - array of payloads → mixed forms survive.
- Use `tempfile` crate (already a dep) for scratch paths; ensure the
  test cleans up.
- Assert on `input.files.length`, `input.files[i].name`,
  `input.files[i].type`.

### Where the tests go

- Rust integration: add a new group function
  (`test_script_action_options`) in
  `crates/ferridriver-cli/tests/backends_support/` — extend
  `backends.rs` only through a new helper file, NOT inline. (See the
  handover note at the top of `backends.rs`.)
- NAPI: append to `test/browser.test.ts` under a new `describe` block
  `action options – Playwright parity`.

### Flipping §1.5

Once the 4 tests pass on all 4 backends + NAPI, update
`PLAYWRIGHT_COMPAT.md`:

- Change `- [~]` to `- [x]` at line ~73.
- Move dblclick/press/type/setInputFiles from "Partial" to "Shipped"
  with the commit SHA + test file reference.

**Do not flip `[x]` without the tests — Rule 9 is non-negotiable.**
Prior sessions got burned on signature-only claims; don't repeat it.

---

## Part B — §1.4: Request / Response / WebSocket lifecycle objects

Biggest remaining Tier-1 piece. Replaces the event-DTO `NetRequest` /
`NetResponse` (currently at `context.rs:29` / `events.rs:59`) with
stateful objects that match Playwright's client-side `Request`,
`Response`, `WebSocket` classes.

### Read first

- `/tmp/playwright/packages/playwright-core/src/client/network.ts` —
  the client-side surface (Request class at ~line 60, Response at
  ~line 370, WebSocket at ~line 770).
- `/tmp/playwright/packages/playwright/types/types.d.ts` — TS types
  for each accessor; use these to drive NAPI `ts_return_type` where
  inference diverges.
- `crates/ferridriver/src/route.rs` — existing route handler;
  `Request` shares lifecycle with an in-flight intercept.
- `crates/ferridriver-node/src/route.rs` — NAPI route wrapper.
- Per-backend network events:
  `crates/ferridriver/src/backend/cdp/mod.rs` (`Network.*` handlers),
  `crates/ferridriver/src/backend/bidi/page.rs`
  (`network.beforeRequestSent`, `network.responseStarted`,
  `network.responseCompleted`),
  `crates/ferridriver/src/backend/webkit/ipc.rs` +
  `crates/ferridriver/src/backend/webkit/host.m` (subset of fields).

### Request surface

Playwright's `Request` class at `client/network.ts:60`. Every method
must work on every backend:

```
url                  → string
resourceType         → string
method               → string
postData             → string | null
postDataBuffer       → Buffer | null
postDataJSON         → any
headers              → Record<string, string>
headersArray         → { name, value }[]
allHeaders           → Promise<Record<string, string>>    // redirected
headerValue          → Promise<string | null>             // redirected
frame                → Frame
isNavigationRequest  → boolean
serviceWorker        → Worker | null        // 2.x subsystem; stub Unsupported
redirectedFrom       → Request | null
redirectedTo         → Request | null
response             → Promise<Response | null>           // awaitable
sizes                → Promise<RequestSizes>              // CDP Network.getRequestPostData + dataReceived
timing               → RequestTiming                      // CDP Network.requestWillBeSent
failure              → { errorText: string } | null
```

Surface the redirect chain via `redirectedFrom` / `redirectedTo`.
Match CDP's `requestWillBeSent.redirectResponse` for the link.

### Response surface

Playwright's `Response` class at `client/network.ts:370`:

```
url                 → string
status              → number
statusText          → string
ok                  → boolean
fromServiceWorker   → boolean
headers             → Record<string, string>
headersArray        → { name, value }[]
allHeaders          → Promise<Record<string, string>>     // raw + extra
headerValue         → Promise<string | null>
headerValues        → Promise<string[]>
body                → Promise<Buffer>                     // CDP Network.getResponseBody
text                → Promise<string>
json                → Promise<any>
request             → Request                              // back-ref
frame               → Frame
securityDetails     → Promise<SecurityDetails | null>
serverAddr          → Promise<ServerAddr | null>
finished            → Promise<Error | null>                // awaitable; null on success
```

`response.body()` must work for already-received responses on every
backend (CDP: `Network.getResponseBody` post-`loadingFinished`; BiDi:
`network.fetchBodyBytes`; WebKit: IPC op).

### WebSocket surface

Playwright's `WebSocket` class at `client/network.ts:770`. Events:
`framesent` (payload), `framereceived` (payload), `socketerror`,
`close`. Methods: `url`, `isClosed`. CDP:
`Network.webSocketFrameSent` / `webSocketFrameReceived`.

### Lifecycle events on Page

Playwright emits:

- `request`  — when request started
- `requestfinished` — on `loadingFinished`
- `requestfailed` — on `loadingFailed`
- `response` — on first byte
- `websocket` — on ws open (fires the `WebSocket` object)

All emit **live object references**, not snapshots. A listener holds
a `Request` and can call `.response()` later once the response
arrives — the promise resolves against the current state.

### Architecture

1. Add `crates/ferridriver/src/network.rs` with `Request`, `Response`,
   `WebSocket`, `RequestTiming`, `RequestSizes`, `SecurityDetails`,
   `ServerAddr` types. Use `Arc<NetworkState>` shared between the
   page event loop and every live object — state mutations
   (e.g. response arriving) flip flags and wake up waiters.
2. Delete `NetRequest` / `NetResponse` from `events.rs` / `context.rs`.
   Update `PageEvent::{Request,Response}` to carry the new objects.
3. `Page::on_request` / `on_response` / `on_request_finished` /
   `on_request_failed` / `on_websocket` emit the typed objects.
4. Per-backend plumbing:
   - **CDP**: `Network.enable` (already on); wire the existing
     `Network.requestWillBeSent` / `requestWillBeSentExtraInfo` /
     `responseReceived` / `responseReceivedExtraInfo` /
     `loadingFinished` / `loadingFailed` / `webSocketFrameSent` /
     `webSocketFrameReceived` into the new `NetworkState`.
   - **BiDi**: `network.beforeRequestSent` + `responseStarted` +
     `responseCompleted` + `fetchError`. WebSocket events have
     different shape — check `/tmp/playwright/packages/playwright-core/src/server/bidi/bidiNetworkManager.ts`.
   - **WebKit**: add `Op::GetResponseBody` / `Op::GetPostData` IPC
     ops if not present; host.m uses `WKURLSchemeTask` /
     `_WKNetworkingSession` for body access. WebKit WebSocket
     support is partial in Playwright's own backend — mirror what
     they do.
5. NAPI bindings in `crates/ferridriver-node/src/network.rs`:
   `#[napi] class Request` / `Response` / `WebSocket` with getters
   for sync fields and async methods for the promise-returning ones.
   Rule 2: diff generated `index.d.ts` against Playwright's
   `test.d.ts` after each rebuild.
6. QuickJS bindings in `crates/ferridriver-script/src/bindings/`:
   matching `RequestJs` / `ResponseJs` / `WebSocketJs`.

### Tests (Rule 9, all 4 backends)

Minimum per-backend integration tests in
`crates/ferridriver-cli/tests/backends_support/network.rs`:

1. **Redirect chain**: navigate to a 302 → 200 page; assert
   `response.request().redirectedFrom().url()` is the 302 URL;
   `redirectedFrom.response().status()` is 302.
2. **Request failure**: navigate to `about:blank`, fetch a blocked
   URL via `route.abort()`, assert `requestfailed` fires with
   `failure.errorText`.
3. **Response body**: fetch a JSON endpoint via `page.evaluate(...)`
   (after a `page.on('response')` listener stores the Response);
   assert `response.json()` returns the parsed shape.
4. **Post data**: POST a JSON body; assert `request.postDataJSON()`
   round-trips.
5. **Headers**: assert `request.headers()` includes `User-Agent`;
   `response.headersArray()` includes duplicates like multi-`Set-Cookie`.
6. **WebSocket**: open a ws echo; assert `framereceived` event fires
   with the echoed payload.

NAPI tests in `crates/ferridriver-node/test/network.test.ts`
exercising the same surface from JS.

### Acceptance for §1.4

- All six integration test buckets pass on cdp-pipe, cdp-raw, bidi,
  webkit. (WebSocket bucket may skip on WebKit with a typed
  `Unsupported` if Playwright's own WebKit backend skips it.)
- NAPI tests pass.
- Generated `index.d.ts` matches Playwright's `test.d.ts` for every
  method (diff + commit the diff in the PR description).
- `PLAYWRIGHT_COMPAT.md` §1.4 checkbox flipped `[ ]` → `[x]` with
  commit SHA.

---

## Ground rules (non-negotiable, from CLAUDE.md)

- Rule 1: Rust core is source of truth. Bindings are thin delegators.
- Rule 2: NAPI + QuickJS + core all mirror Playwright's TS
  signatures in the same commit that changes them.
- Rule 4: every backend real — no stubs, no hardcoded placeholders.
  Typed `FerriError::Unsupported { reason }` only where the protocol
  genuinely can't (e.g. WebSocket on WebKit if Playwright itself
  skips).
- Rule 6: read `/tmp/playwright/...` before coding each signature.
- Rule 7: rebuild NAPI (`bun run build:debug`) after every binding
  change and diff `crates/ferridriver-node/index.d.ts` against
  Playwright's `test.d.ts`. Use `ts_args_type` / `ts_return_type`
  where napi-rs inference diverges.
- Rule 9: per-option integration test on every backend before
  flipping `[x]`. No signature-only completion claims.
- Rule 10: no `#[allow(clippy::*)]` escape hatches. No `--no-verify`
  on commits.
- No task / phase / rule-number annotations in source comments or
  filenames.
- No emojis. No AI attribution in commit messages.

## Commit shape

Two commits:

1. `feat(tests): add per-option tests for dblclick/press/type/setInputFiles`
   — body lists each test + backend matrix.
2. `feat(network): full Request/Response/WebSocket lifecycle objects`
   — body lists the surface, per-backend plumbing, test coverage,
   and the `PLAYWRIGHT_COMPAT.md` §1.4 flip.

Update `PLAYWRIGHT_COMPAT.md` in the same commits that ship the tests
(no floating checkbox flips).

## Tests that must stay green after each commit

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --lib
cd crates/ferridriver-node && bun run build:debug && bun test
cargo build -p ferridriver-cli
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1
```

Current baseline (after the evaluate-API consolidation): 119 core,
730 NAPI bun, 4/4 backend integration, workspace-wide clippy clean.
All must stay green.

## Close-out

After both parts land:

- Overwrite `HANDOVER.md` to pivot to Tier 2 §2.1 (CDPSession) or the
  next-highest-priority Tier-2 item.
- Replace this prompt file (`docs/FINISH_TIER1.md`) with a short
  "Tier 1 done" pointer note.
- `PLAYWRIGHT_COMPAT.md` §1.4 and §1.5 both `[x]`.

No checkbox flips without Rule-9 test coverage.
