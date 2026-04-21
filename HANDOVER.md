# Handover — next Playwright-parity session

Read-first for any session continuing work. Overwrite this file with a
fresh summary at the end of each block.

## Cross-device setup

Everything needed is committed. Next session should read, in order:

1. `CLAUDE.md` — the Playwright-parity rules and consolidated
   lessons. Authoritative cross-device source.
2. `PLAYWRIGHT_COMPAT.md` — gap tracker. Tier 1 is fully closed. Tier 3
   is now leading the next batch of "common usage" work (see below).
3. This file — block-level commit summary below.
4. `docs/NEXT_SESSION.md` — the specific next-block brief (currently
   pointed at §2.9 Dialog-as-handle).

Set up the cloned Playwright at `/tmp/playwright` if it isn't there:

```bash
git clone https://github.com/microsoft/playwright /tmp/playwright
```

## What just landed (2026-04-21) — §3.1 Navigation returns Response

`§3.1 — page.goto / reload / goBack / goForward return Response | null`
shipped end-to-end on Rust core + NAPI + QuickJS, Rule-9 tested on all
four backends. Matches Playwright's
`/tmp/playwright/packages/playwright-core/src/client/page.ts:378-489`
and `client/frame.ts:111-114` byte-for-byte.

### Surfaces shipped

- **`NavRequestSlot`** (`crates/ferridriver/src/network.rs`) — new
  cheap `Arc<Mutex<Option<Request>>>` helper. Backend network listeners
  set the slot whenever a request with `is_navigation_request == true`
  is observed; navigation methods clear the slot before issuing the
  command and read it after the lifecycle waiter resolves. Same-doc
  navigations leave the slot empty → `None` is returned naturally,
  matching Playwright's `null` contract for hash-only / `history.pushState`
  / SPA navigations.
- **CDP backend** (`backend/cdp/mod.rs`): `NetworkTracker` gained a
  `nav_request_slot` field; `on_request_will_be_sent` updates it when
  `loaderId == requestId`. Every redirect hop reuses the same CDP
  request id, so the slot naturally ends up pointing at the final
  request; `request.response().await` resolves immediately because the
  Response is already attached by the time lifecycle fires. `goto`,
  `reload`, `go_back`, `go_forward` now return `Result<Option<Response>,
  String>`; the sync fast-path for already-fired lifecycle also returns
  the resolved Response.
- **BiDi backend** (`backend/bidi/page.rs`): `BidiNetworkTracker`
  follows the same pattern — uses the `navigation` field on
  `network.beforeRequestSent` to detect nav requests. All four
  navigation methods updated.
- **WebKit backend** (`backend/webkit/mod.rs`): returns `Ok(None)` with
  a docstring naming the limit. Stock `WKWebView`'s
  `WKNavigationDelegate` callbacks don't round-trip `NSURLResponse`
  status/headers through our IPC, and the JS-fetch interceptor only
  observes user-script fetches. Returning `None` is the honest
  Playwright-parity outcome (Playwright itself returns `null` where it
  can't observe); placeholder Responses would violate Rule 4.
- **NAPI** (`crates/ferridriver-node/src/page.rs`, `frame.rs`):
  `#[napi(ts_return_type = "Promise<Response | null>")]` on every
  `goto`/`reload`/`goBack`/`goForward`. Generated `index.d.ts` matches
  Playwright's `types/test.d.ts` verbatim. `Response` carries the
  owning page reference so downstream code (e.g. `response.request()`)
  can walk back to the page.
- **QuickJS** (`crates/ferridriver-script/src/bindings/page.rs`):
  returns `Option<ResponseJs>`. Callers using `resp == null` see both
  `null` and `undefined` (rquickjs `Option::None` → JS `undefined`);
  strict `=== null` wouldn't match, so tests use the loose form.

### Tests landed

- **Rust integration** (`tests/backends_support/navigation_response.rs`):
  - `test_goto_returns_response` — status 200, ok true, url ends
    with `/landed` on CDP / BiDi; WebKit asserts `null` explicitly.
  - `test_goto_follows_redirects` — 302→200 resolves to the landed
    URL (not the 302) on CDP / BiDi; WebKit asserts `null`.
  - `test_goto_network_failure` — unreachable URL rejects with a
    typed error naming the network failure (ERR_CONNECTION /
    NS_ERROR / Navigation-failed). WebKit skipped here (the
    JS-interceptor + WKWebView error surface is tracked separately;
    `test_network_request_failure` covers the same lifecycle).
  - `test_reload_returns_response` — reload round-trip.
  - `test_history_traversal_returns_response` — goBack + goForward
    return the target entry's response.
  - Wired into `run_all_tests` in `tests/backends.rs`. All four
    backends green (cdp-pipe, cdp-raw, bidi, webkit).
- **NAPI** (`crates/ferridriver-node/test/navigation-response.test.ts`):
  6 tests × 2 CDP backends. Covers every method + the 404 case
  (`status() === 404`, `ok() === false`, does NOT throw) + the
  unreachable-URL rejection case.

### Baseline after this commit (must stay green)

```
cargo clippy --workspace --all-targets -- -D warnings   # clean
cargo test --workspace --lib                            # 122 core
cd crates/ferridriver-node && bun run build:debug && bun test   # 781 bun (was 754)
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1   # 4/4 backends
```

## Next session priority queue

Deprioritised `§2.1 CDPSession` (lower daily-usage); picked a batch of
high-frequency Playwright APIs instead. Order:

1. **`§2.9 Dialog as first-class handle + page.on('dialog')`** — next up.
   Promote `Dialog` from a callback-based handler to a first-class event
   handle. `page.on('dialog', d => d.accept())` is everyday Playwright.
   Delete `page.set_dialog_handler`. New `Dialog { accept, dismiss,
   default_value, message, page, type }` with one-shot resolution
   semantics (Playwright auto-dismisses `beforeunload` if no listener
   registered).
2. `§2.11 FileChooser class + page.on('filechooser')` — CDP
   `Page.fileChooserOpened` → `PageEvent::FileChooser(FileChooser {
   element, is_multiple, page, set_files(files, options) })`. Routes
   through the existing `setInputFiles` plumbing from §1.5.
3. `§3.12 Regex on getBy* + waitForURL` — `StringOrRegex` param on
   `get_by_role.name`, `get_by_text`, `get_by_label`, `get_by_placeholder`,
   `get_by_alt_text`, `get_by_title`, `get_by_test_id`, `wait_for_url`,
   `get_attribute` compare. NAPI accepts `string | RegExp` via the
   existing `JsRegExpLike` prototype-walker (no wire shapes); QuickJS
   accepts real `RegExp` instances too.
4. `§4.1 BrowserContextOptions` — 28-field option object at context
   creation (viewport, userAgent, locale, timezone, geolocation,
   permissions, etc.). Probably 2–3 sessions.

See `docs/NEXT_SESSION.md` for the §2.9 block brief.

### Ground rules reminder

- Rule 1: core is source of truth; bindings are thin delegators.
- Rule 2: all three layers update in the same commit.
- Rule 4: every backend real — `FerriError::Unsupported` / honest
  `None` only where the protocol genuinely can't.
- Rule 6: read `/tmp/playwright/...` before coding each signature.
- Rule 7: rebuild NAPI + diff generated `index.d.ts` after every
  binding change.
- Rule 9: per-backend integration test on every backend before
  flipping `[x]`.
- Rule 10: no `#[allow(clippy::*)]` escape hatches.
- No task / phase / rule-number annotations in source comments or
  filenames.
- No emojis. No AI attribution in commit messages.

## Carried-forward backend gaps (don't relitigate)

From §1.4, still real protocol limits:

- **BiDi**: response body unavailable for non-intercepted responses
  (Firefox discards bytes; Playwright's BiDi backend hits the same
  limit). Multi-`Set-Cookie` collapses. `request.postData()` null for
  fetch-with-body.
- **WebKit**: stock `WKWebView` exposes no public API for main-doc
  Response observability (extended to §3.1: `goto`/`reload`/`goBack`/
  `goForward` all return `null` — documented, honest, not a shortcut),
  redirect chain, response body bytes, browser-set request headers
  (`User-Agent`), `Set-Cookie`, or WebSocket frame events. Also:
  `page.evaluate` runs in utility context isolated from the
  user-script's fetch wrap, so `page.route` cannot intercept fetches
  initiated through `page.evaluate("fetch(...)")` — only main-world
  fetches initiated from user-controlled JS.

## Known flakes

- `context.setOffline toggles network` on WebKit bun occasionally
  fails under the full suite but passes in isolation. Pre-existing
  state leak, unrelated to recent work.

## Key source locations

| area | path |
|---|---|
| Navigation methods (public) | `crates/ferridriver/src/page.rs`, `frame.rs` |
| Nav-request slot helper | `crates/ferridriver/src/network.rs::NavRequestSlot` |
| CDP nav-response capture | `crates/ferridriver/src/backend/cdp/mod.rs::CdpPage::goto`, `await_nav_response` |
| BiDi nav-response capture | `crates/ferridriver/src/backend/bidi/page.rs::BidiPage::goto`, `await_nav_response` |
| WebKit nav (returns None) | `crates/ferridriver/src/backend/webkit/mod.rs::WebKitPage::goto` |
| NAPI nav bindings | `crates/ferridriver-node/src/page.rs`, `frame.rs` |
| QuickJS nav bindings | `crates/ferridriver-script/src/bindings/page.rs` |
| §3.1 Rust integration tests | `crates/ferridriver-cli/tests/backends_support/navigation_response.rs` |
| §3.1 NAPI integration tests | `crates/ferridriver-node/test/navigation-response.test.ts` |
| Rules + lessons | `CLAUDE.md` |
