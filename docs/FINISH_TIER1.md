# Prompt — finish Tier 1

**Status (2026-04-19):** §1.5 closed out in commit `28f7a04`
(dblclick / press / type / setInputFiles per-option Rule-9 tests
across all 4 backends + NAPI). §1.4 — Request / Response /
WebSocket lifecycle — is still open; it was too big to land in
the same session without violating Rule 4 or Rule 9.

The canonical plan for §1.4 (core types, per-backend plumbing,
bindings, test buckets) is preserved verbatim below — follow it
verbatim for the next session. Hand-over context and current
baseline tests live in `HANDOVER.md`.

---

## §1.4 — Request / Response / WebSocket lifecycle objects

Biggest remaining Tier-1 piece. Replaces the event-DTO
`NetRequest` / `NetResponse` (currently at `context.rs:29` /
`events.rs:59`) with stateful objects that match Playwright's
client-side `Request`, `Response`, `WebSocket` classes.

### Read first

- `/tmp/playwright/packages/playwright-core/src/client/network.ts`
  — the client-side surface (Request class at ~line 60, Response
  at ~line 370, WebSocket at ~line 770).
- `/tmp/playwright/packages/playwright/types/types.d.ts` — TS
  types for each accessor; use these to drive NAPI
  `ts_return_type` where inference diverges.
- `crates/ferridriver/src/route.rs` — existing route handler;
  `Request` shares lifecycle with an in-flight intercept.
- `crates/ferridriver-node/src/route.rs` — NAPI route wrapper.
- Per-backend network events:
  `crates/ferridriver/src/backend/cdp/mod.rs` (`Network.*`),
  `crates/ferridriver/src/backend/bidi/page.rs`
  (`network.beforeRequestSent` / `responseStarted` /
  `responseCompleted`), `crates/ferridriver/src/backend/webkit/`
  (subset of fields).

### Request surface

Playwright's `Request` class at `client/network.ts:60`. Every
method must work on every backend:

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

`response.body()` must work for already-received responses on
every backend (CDP: `Network.getResponseBody` post
`loadingFinished`; BiDi: `network.fetchBodyBytes`; WebKit: IPC
op).

### WebSocket surface

Playwright's `WebSocket` class at `client/network.ts:770`.
Events: `framesent` (payload), `framereceived` (payload),
`socketerror`, `close`. Methods: `url`, `isClosed`. CDP:
`Network.webSocketFrameSent` / `webSocketFrameReceived`.

### Lifecycle events on Page

Playwright emits:

- `request`  — when request started
- `requestfinished` — on `loadingFinished`
- `requestfailed` — on `loadingFailed`
- `response` — on first byte
- `websocket` — on ws open (fires the `WebSocket` object)

All emit **live object references**, not snapshots. A listener
holds a `Request` and can call `.response()` later once the
response arrives — the promise resolves against the current
state.

### Architecture

1. Add `crates/ferridriver/src/network.rs` with `Request`,
   `Response`, `WebSocket`, `RequestTiming`, `RequestSizes`,
   `SecurityDetails`, `ServerAddr` types. Use
   `Arc<NetworkState>` shared between the page event loop and
   every live object — state mutations (e.g. response arriving)
   flip flags and wake up waiters.
2. Delete `NetRequest` / `NetResponse` from `events.rs` /
   `context.rs`. Update `PageEvent::{Request,Response}` to carry
   the new objects.
3. `Page::on_request` / `on_response` / `on_request_finished` /
   `on_request_failed` / `on_websocket` emit the typed objects.
4. Per-backend plumbing:
   - **CDP**: `Network.enable` (already on); wire the existing
     `Network.requestWillBeSent` / `requestWillBeSentExtraInfo` /
     `responseReceived` / `responseReceivedExtraInfo` /
     `loadingFinished` / `loadingFailed` /
     `webSocketFrameSent` / `webSocketFrameReceived` into the
     new `NetworkState`.
   - **BiDi**: `network.beforeRequestSent` + `responseStarted` +
     `responseCompleted` + `fetchError`. WebSocket events have a
     different shape — check
     `/tmp/playwright/packages/playwright-core/src/server/bidi/bidiNetworkManager.ts`.
   - **WebKit**: add `Op::GetResponseBody` / `Op::GetPostData`
     IPC ops if not present; `host.m` uses `WKURLSchemeTask` /
     `_WKNetworkingSession` for body access. WebKit WebSocket
     support is partial in Playwright's own backend — mirror
     what they do.
5. NAPI bindings in `crates/ferridriver-node/src/network.rs`:
   `#[napi] class Request` / `Response` / `WebSocket` with
   getters for sync fields and async methods for the
   promise-returning ones. Rule 2: diff generated `index.d.ts`
   against Playwright's `test.d.ts` after each rebuild.
6. QuickJS bindings in `crates/ferridriver-script/src/bindings/`:
   matching `RequestJs` / `ResponseJs` / `WebSocketJs`.

### Tests (Rule 9, all 4 backends)

Minimum per-backend integration tests in a new
`crates/ferridriver-cli/tests/backends_support/network.rs`:

1. **Redirect chain**: navigate to a 302 → 200 page; assert
   `response.request().redirectedFrom().url()` is the 302 URL;
   `redirectedFrom.response().status()` is 302.
2. **Request failure**: navigate to `about:blank`, fetch a
   blocked URL via `route.abort()`, assert `requestfailed` fires
   with `failure.errorText`.
3. **Response body**: fetch a JSON endpoint via
   `page.evaluate(...)` (after a `page.on('response')` listener
   stores the Response); assert `response.json()` returns the
   parsed shape.
4. **Post data**: POST a JSON body; assert
   `request.postDataJSON()` round-trips.
5. **Headers**: assert `request.headers()` includes `User-Agent`;
   `response.headersArray()` includes duplicates like
   multi-`Set-Cookie`.
6. **WebSocket**: open a ws echo; assert `framereceived` event
   fires with the echoed payload.

NAPI tests in `crates/ferridriver-node/test/network.test.ts`
exercising the same surface from JS.

### Acceptance for §1.4

- All six integration test buckets pass on cdp-pipe, cdp-raw,
  bidi, webkit. (WebSocket bucket may skip on WebKit with a typed
  `Unsupported` if Playwright's own WebKit backend skips it.)
- NAPI tests pass.
- Generated `index.d.ts` matches Playwright's `test.d.ts` for
  every method (diff + commit the diff in the PR description).
- `PLAYWRIGHT_COMPAT.md` §1.4 checkbox flipped `[ ]` → `[x]`
  with commit SHA.

---

## Ground rules (non-negotiable, from CLAUDE.md)

- Rule 1: Rust core is source of truth. Bindings are thin
  delegators.
- Rule 2: NAPI + QuickJS + core all mirror Playwright's TS
  signatures in the same commit that changes them.
- Rule 4: every backend real — no stubs, no hardcoded
  placeholders. Typed `FerriError::Unsupported { reason }` only
  where the protocol genuinely can't.
- Rule 6: read `/tmp/playwright/...` before coding each
  signature.
- Rule 7: rebuild NAPI (`bun run build:debug`) after every
  binding change and diff `crates/ferridriver-node/index.d.ts`
  against Playwright's `test.d.ts`.
- Rule 9: per-option integration test on every backend before
  flipping `[x]`.
- Rule 10: no `#[allow(clippy::*)]` escape hatches.
- No task / phase / rule-number annotations in source comments
  or filenames.
- No emojis. No AI attribution in commit messages.

## Commit shape

Single commit:

- `feat(network): full Request/Response/WebSocket lifecycle objects`
  — body lists the surface, per-backend plumbing, test
  coverage, and the `PLAYWRIGHT_COMPAT.md` §1.4 flip.

Update `PLAYWRIGHT_COMPAT.md` in the same commit (no floating
checkbox flips).

## Tests that must stay green after the commit

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --lib
cd crates/ferridriver-node && bun run build:debug && bun test
cargo build -p ferridriver-cli
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1
```

Baseline after §1.5 close-out (must stay green through §1.4):
119 core, 742 NAPI bun, 4/4 backend integration, workspace-wide
clippy clean.

## Close-out

After §1.4 lands:

- Overwrite `HANDOVER.md` to pivot to Tier 2 §2.1 (CDPSession)
  or the next-highest-priority Tier-2 item.
- Replace this prompt file with a short "Tier 1 done" pointer.
- `PLAYWRIGHT_COMPAT.md` §1.4 flipped `[x]`.

No checkbox flips without Rule-9 test coverage.
