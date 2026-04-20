# Handover — next Playwright-parity session

Read-first for any session continuing work. Overwrite this file with a
fresh summary at the end of each block.

## Cross-device setup

Everything needed is committed. Next session should read, in order:

1. `CLAUDE.md` — the Playwright-parity rules and consolidated
   lessons. Authoritative cross-device source.
2. `PLAYWRIGHT_COMPAT.md` — gap tracker. Tier 1 is now fully closed;
   the largest remaining unit is Tier 2.
3. This file — block-level commit summary below.

Set up the cloned Playwright at `/tmp/playwright` if it isn't there:

```bash
git clone https://github.com/microsoft/playwright /tmp/playwright
```

## What just landed (2026-04-21) — Tier 1 §1.4 closed (with gap reduction pass)

`§1.4 — Request / Response / WebSocket lifecycle objects` shipped.
Tier 1 is now entirely [x] in `PLAYWRIGHT_COMPAT.md`. Several
shortcuts taken in the initial §1.4 commit were eliminated in a
follow-up gap-reduction pass:

- `request.timing()`, `request.redirectedTo()` are now sync (Playwright-shape) via `ArcSwap`.
- `request.frame()` returns the live `Frame` via the owning page's frame cache (NAPI + QuickJS hold optional `Arc<Page>` references on wrappers).
- `request.serviceWorker()` exists on all three layers (returns `null` until §2.7 ships; signature is stable).
- WebKit's JS-fetch interceptor was extended (`host.m`) to emit `kind:'response'` and `kind:'failure'` postMessages with status/headers/errorText. Two new IPC reps (`REP_NET_RESPONSE_EVENT`, `REP_NET_FAILURE_EVENT`) carry them. The WebKit listener (`drain_network_events`) emits matching `Response` / `RequestFailed` events. Result: `page.on('response')` / `request.failure()` work on WebKit within the JS interceptor's reach.
- QuickJS `page.waitForRequest` / `waitForResponse` accept `string | RegExp` (RegExp via `source`/`flags` getters → `UrlMatcher::regex_from_source`).
- QuickJS `page.route(matcher, handler)` + `page.unroute(matcher)` + `RouteJs` class (`fulfill` / `continue` / `abort` / `url` / `method` / `resourceType` / `postData` / `headers`) — full Playwright surface. Cross-task JS callback dispatch: `install_page` captures the script's `AsyncContext`; route handler stashes the JS function in a per-page `globalThis.__fdRoutes` `Map` keyed by ID; backend route handler spawns a `tokio` task that `async_with`s back into the script's `AsyncContext` and invokes the JS callback by ID. Failure test now uses `route.abort('blockedbyclient')` (canonical Playwright path) on cdp-pipe / cdp-raw / bidi.
- Per-backend integration tests in `crates/ferridriver-cli/tests/backends_support/network.rs` retightened: every conditional branch is an explicit per-backend assertion of a documented protocol limit (Rule 4 typed gap), not a silent skip.

### Surfaces shipped

- New `crates/ferridriver/src/network.rs` carries `Request`,
  `Response`, `WebSocket`, `RequestTiming`, `RequestSizes`,
  `SecurityDetails`, `RemoteAddr` — all `Arc`-shared so backend
  listeners and JS callers see the same live state. Promise-returning
  accessors (`request.response()`, `response.finished()`,
  `response.body()`) wake on backend state transitions via
  `tokio::sync::Notify`.
- `events.rs::PageEvent` gained `Request`, `Response`,
  `RequestFinished`, `RequestFailed`, `WebSocket` variants carrying the
  live objects. `NetRequest` / `NetResponse` snapshot DTOs deleted.
- Per-backend live wiring:
  - **CDP** (`backend/cdp/mod.rs::NetworkTracker`) — full
    `Network.requestWillBeSent` + `requestWillBeSentExtraInfo` +
    `responseReceived` + `responseReceivedExtraInfo` +
    `loadingFinished` + `loadingFailed` + `webSocketCreated` +
    `webSocketFrameSent` + `webSocketFrameReceived` +
    `webSocketFrameError` + `webSocketClosed`. Body via lazy
    `Network.getResponseBody`. Redirect chain via
    `redirectResponse` field.
  - **BiDi** (`backend/bidi/page.rs::BidiNetworkTracker`) —
    `network.beforeRequestSent` + `responseStarted` +
    `responseCompleted` + `fetchError`. Body via lazy
    `network.getData`. Maps Firefox's "no such network data" error
    to typed `FerriError::Unsupported` because Firefox discards
    body bytes for non-intercepted responses (Playwright's own BiDi
    has the same constraint).
  - **WebKit** (`backend/webkit/mod.rs`) — keeps the existing
    fetch/XHR JS interceptor for `Request` events; response body
    and Response events surface typed `Unsupported` / Timeout
    because stock `WKWebView` exposes no public API for response
    inspection (analogous to `printToPDF`).
- NAPI `crates/ferridriver-node/src/network.rs` adds
  `#[napi] class Request` / `Response` / `WebSocket` mirroring the
  Playwright `client/network.ts` surface. `Page.waitForEvent('websocket')`
  returns a real `WebSocket` instance via
  `napi::bindgen_prelude::Either4<Request, Response, WebSocket, Value>`
  with overloaded `ts_return_type`.
- QuickJS `crates/ferridriver-script/src/bindings/network.rs`:
  `RequestJs` / `ResponseJs` / `WebSocketJs` with the same surface.
  `page.waitForEvent('websocket')` dispatches through the QuickJS
  Class registry returning the live wrapper. `page.waitForRequest` /
  `waitForResponse` added (string-glob matchers only — RegExp
  variant tracked as a separate gap).
- MCP `tools/network.rs` and `server.rs` use a new
  `Request::to_diagnostic_json()` snapshot helper to keep the
  network-resource JSON output stable now that the underlying type
  is no longer `serde::Serialize`.

### Tests landed

- `crates/ferridriver-cli/tests/backends_support/network.rs` — six
  Rule-9 buckets (`test_network_redirect_chain`,
  `test_network_request_failure`, `test_network_response_body`,
  `test_network_post_data`, `test_network_headers`,
  `test_network_websocket`). Wired into `tests/backends.rs`'s
  `run_all_tests` so all four backends exercise them. Per-backend
  branches assert typed `Unsupported` / Timeout for genuine gaps;
  no silent skips. Uses `tokio-tungstenite` for the in-test
  WebSocket echo server.
- `crates/ferridriver-node/test/network.test.ts` — same six buckets
  for NAPI on `cdp-pipe` / `cdp-raw`. Uses the `ws` library for the
  echo server.

### Documented backend / binding gaps

The following are real protocol / API limits — tracked alongside the
§1.4 checkbox in `PLAYWRIGHT_COMPAT.md`. Each surfaces as a typed
`FerriError::Unsupported` (or explicit per-backend test assertion)
rather than a silent skip:

- **BiDi**: response body unavailable without `network.addIntercept`
  (Firefox discards bytes; Playwright BiDi mirrors). Multi-`Set-Cookie`
  collapses to a joined value. `request.postData()` null for fetch
  with body until BiDi exposes the request body field.
- **WebKit**: stock `WKWebView` exposes no public API for: main-doc
  Response events (the JS interceptor only sees user-script fetch/XHR,
  so `page.waitForResponse` for `page.goto` times out by design);
  redirect chain (handled internally); response body bytes; browser-set
  request headers like `User-Agent` (only user-overridden headers
  visible to the interceptor); `Set-Cookie` (Fetch spec hides it from
  `Headers.forEach`); WebSocket frame events.
- **WebKit `page.route` reachability via `page.evaluate`**: WKWebView's
  utility-context for QuickJS `page.evaluate` is isolated from the
  user-script's `fetch` wrap. The route registration system itself
  works (regex pushed to `__fd_routes`, handler dispatched via
  `RouteHandler` callback), but only fetches that go through the
  user-script-wrapped `fetch` (i.e. main-world fetches initiated by
  user-controlled JS) hit the route. WebKit failure test sidesteps
  by using a refused TCP port; the `requestfailed` lifecycle event
  fires identically through the JS-interceptor's `kind:'failure'`
  postMessage path. Documented Tier-2 follow-up (would require a
  separate world-injection mode for utility scripts).

### Baseline after this commit (must stay green)

```
cargo clippy --workspace --all-targets -- -D warnings   # clean
cargo test --workspace --lib                            # 122 core
cd crates/ferridriver-node && bun run build:debug && bun test   # 754 bun
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1   # 4/4 backends
```

## Next session: Tier 2

Tier 1 is done. Tier 2's largest unblocking item is `§2.1 CDPSession`
— exposing Chrome's raw CDP session surface so users can drive the
protocol directly (mirrors Playwright's `chromiumBrowserContext.newCDPSession`).

Other natural follow-ups (in rough priority order):

1. `§2.1 CDPSession` — CDP raw session API.
2. `§2.6 HAR recording + routing`.
3. `§2.2 Clock API`.
4. WebKit utility-context world-injection so `page.evaluate("fetch(...)")`
   hits the user-script's wrapped fetch (closes the WebKit-side
   `page.route` reachability gap).

### Ground rules reminder

- Rule 1: core is source of truth; bindings are thin delegators.
- Rule 2: all three layers update in the same commit.
- Rule 4: every backend real — `FerriError::Unsupported` only
  where the protocol genuinely can't.
- Rule 6: read `/tmp/playwright/...` before coding each signature.
- Rule 7: rebuild NAPI + diff generated `index.d.ts` after every
  binding change.
- Rule 9: per-backend integration test on every backend before
  flipping `[x]`.
- Rule 10: no `#[allow(clippy::*)]` escape hatches.
- No task / phase / rule-number annotations in source comments or
  filenames.
- No emojis. No AI attribution in commit messages.

## Known flakes

- `context.setOffline toggles network` on WebKit bun occasionally
  fails under the full suite but passes in isolation. Pre-existing
  state leak, unrelated to recent work.

## Key source locations

| area | path |
|---|---|
| Live network types | `crates/ferridriver/src/network.rs` |
| CDP network listener | `crates/ferridriver/src/backend/cdp/mod.rs::NetworkTracker` |
| BiDi network listener | `crates/ferridriver/src/backend/bidi/page.rs::BidiNetworkTracker` |
| WebKit network listener | `crates/ferridriver/src/backend/webkit/mod.rs` |
| NAPI network classes | `crates/ferridriver-node/src/network.rs` |
| QuickJS network classes | `crates/ferridriver-script/src/bindings/network.rs` |
| Per-backend integration tests | `crates/ferridriver-cli/tests/backends_support/network.rs` |
| NAPI integration tests | `crates/ferridriver-node/test/network.test.ts` |
| MCP network tool surface | `crates/ferridriver-mcp/src/tools/network.rs` |
| Rules + lessons | `CLAUDE.md` |
