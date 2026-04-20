# Next session — Tier 2 §2.1 CDPSession

Tier 1 is fully [x] in `PLAYWRIGHT_COMPAT.md` (commit `1c84045`).
The natural next item is `§2.1 CDPSession` — exposing Chrome's raw
CDP session surface so users can drive the protocol directly. This
mirrors Playwright's `chromiumBrowserContext.newCDPSession(page)`.

## Read-first

1. `CLAUDE.md` — Playwright-parity rules (Rules 1–10) and the
   consolidated lessons. Authoritative cross-device source.
2. `PLAYWRIGHT_COMPAT.md` — gap tracker. §2.1 is the next item.
3. `HANDOVER.md` — full block-level summary of what landed in §1.4
   plus the documented gap matrix carried forward.
4. `/tmp/playwright/packages/playwright-core/src/client/cdpSession.ts`
   — canonical client surface (clone with
   `git clone https://github.com/microsoft/playwright /tmp/playwright`
   if missing).
5. `/tmp/playwright/packages/playwright-core/src/server/chromium/crBrowser.ts`
   (the `_newCDPSession` server-side path) for protocol shape.

## §2.1 scope (canonical Playwright signature)

```ts
class CDPSession {
  send<T extends keyof Protocol.CommandParameters>(
    method: T,
    params?: Protocol.CommandParameters[T]
  ): Promise<Protocol.CommandReturnValues[T]>;
  on<T extends keyof Protocol.Events>(
    event: T,
    listener: (params: Protocol.Events[T]) => void
  ): this;
  off / once / removeAllListeners: standard EventEmitter shape;
  detach(): Promise<void>;
}

// Entry point on a Chromium-flavored context:
chromiumBrowserContext.newCDPSession(page: Page): Promise<CDPSession>
```

## Architecture sketch

1. **Rust core** — new `crates/ferridriver/src/cdp_session.rs` with a
   `CDPSession` struct holding `Arc<dyn CdpTransport>` + the
   `session_id` (or `None` for browser-target sessions). Methods:
   - `send(method: &str, params: serde_json::Value) -> Result<serde_json::Value>`
   - `on(event: &str, callback) -> ListenerId` (uses the same
     `EventEmitter` pattern as `PageEvent`, but generic over event name
     rather than typed enum)
   - `off(id)` / `once` / `remove_all_listeners`
   - `detach()` — calls `Target.detachFromTarget` (or
     `Target.disposeBrowserContext` for context sessions)

   Live wiring: piggy-back on the existing `CdpTransport::subscribe_events`
   machinery. The CDP backend already broadcasts every event — `CDPSession::on`
   filters by method name and (optionally) session id.

2. **CdpPipe / CdpRaw** — `CdpBrowser::new_cdp_session(page) -> Result<CDPSession>`
   issues `Target.attachToTarget { targetId, flatten: true }` (already
   set as default per `init`), then constructs a `CDPSession` keyed off
   the returned `sessionId`. For browser-level sessions, pass `None`.

3. **BiDi / WebKit** — return typed `FerriError::Unsupported`. Per Rule 4:
   - BiDi has its own protocol (no CDP at all). Playwright's BiDi backend
     refuses CDPSession.
   - WebKit doesn't expose CDP. Same.

4. **NAPI / QuickJS bindings** — `#[napi] class CDPSession` and
   `CDPSessionJs` with the four methods. Mirror the Playwright TS shape.

5. **Per-backend Rule-9 integration tests** in
   `crates/ferridriver-cli/tests/backends_support/cdp_session.rs`:
   - cdp-pipe / cdp-raw: send `Browser.getVersion`, assert non-empty
     `product`. Subscribe to `Page.frameNavigated`, navigate, assert
     callback fires.
   - bidi / webkit: assert `chromiumBrowserContext.newCDPSession` rejects
     with typed `Unsupported`.
6. NAPI parity test in `crates/ferridriver-node/test/cdp-session.test.ts`.

## Ground rules (from CLAUDE.md — non-negotiable)

- Rule 1: core is source of truth; bindings are thin delegators.
- Rule 2: all three layers (Rust core / NAPI / QuickJS) update in the same commit.
- Rule 4: every backend real — `FerriError::Unsupported` only where
  the protocol genuinely can't.
- Rule 6: read `/tmp/playwright/...` before coding each signature.
- Rule 7: rebuild NAPI + diff generated `index.d.ts` against
  `/tmp/playwright/packages/playwright/types/test.d.ts` after every
  binding change.
- Rule 9: per-backend integration test on every backend before
  flipping `[x]`. No silent `if backend == ...` skips — typed
  `Unsupported` is the documented Rule-4 path.
- Rule 10: no `#[allow(clippy::*)]` escape hatches. The single-site
  `unused_self` allow on `Request::service_worker` is the only
  precedent — only justified because of Rule 2 (Playwright signature
  parity) vs Rule 10 collision; document `Why:` if you need another.
- No emojis, no AI attribution in commit messages, no task / phase
  / rule-number annotations in source comments or filenames.

## Baseline (must stay green through §2.1)

```
cargo clippy --workspace --all-targets -- -D warnings   # clean
cargo test --workspace --lib                            # 122 core
cd crates/ferridriver-node && bun run build:debug && bun test   # 754 bun
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1   # 4/4 backends
```

## Commit shape

Single commit:
- `feat(cdp): CDPSession lifecycle on chromium contexts (§2.1)` —
  body lists the surface, per-backend implementation (real on CDP,
  typed Unsupported on BiDi/WebKit), test coverage, and the
  `PLAYWRIGHT_COMPAT.md` §2.1 flip.

Update `PLAYWRIGHT_COMPAT.md` + `HANDOVER.md` in the same commit.

## Carried-forward backend gaps (don't relitigate; track in PLAYWRIGHT_COMPAT.md)

These are real protocol limits documented under §1.4 — not in scope
for §2.1, but worth re-reading so you don't accidentally re-introduce
shortcuts:

- **BiDi**: response body unavailable for non-intercepted responses
  (Firefox discards bytes; Playwright's BiDi backend hits the same).
  Multi-`Set-Cookie` collapses. `request.postData()` null for
  fetch-with-body.
- **WebKit**: stock `WKWebView` exposes no public API for: main-doc
  Response observability, redirect chain, response body bytes,
  browser-set request headers (`User-Agent`), `Set-Cookie` (Fetch
  spec hides it from `Headers.forEach`), or WebSocket frame events.
  Also: `page.evaluate` runs in utility context isolated from the
  user-script's fetch wrap, so `page.route` cannot intercept fetches
  initiated through `page.evaluate("fetch(...)")` — only main-world
  fetches initiated from user-controlled JS.

## Useful key locations

| area | path |
|---|---|
| Live network types | `crates/ferridriver/src/network.rs` |
| CDP backend dispatch | `crates/ferridriver/src/backend/cdp/mod.rs` |
| BiDi backend | `crates/ferridriver/src/backend/bidi/page.rs` |
| WebKit backend | `crates/ferridriver/src/backend/webkit/mod.rs` |
| WebKit IPC + host | `crates/ferridriver/src/backend/webkit/ipc.rs`, `host.m` |
| NAPI bindings entry | `crates/ferridriver-node/src/lib.rs` |
| QuickJS bindings entry | `crates/ferridriver-script/src/bindings/mod.rs` |
| Per-backend integration tests | `crates/ferridriver-cli/tests/backends_support/` |
| MCP server | `crates/ferridriver-mcp/src/` |
| Rules + lessons | `CLAUDE.md` |
