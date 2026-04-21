# Handover — next Playwright-parity session

Read-first for any session continuing work. Overwrite this file with a
fresh summary at the end of each block.

## Cross-device setup

Everything needed is committed. Next session should read, in order:

1. `CLAUDE.md` — the Playwright-parity rules and consolidated
   lessons. Authoritative cross-device source.
2. `PLAYWRIGHT_COMPAT.md` — gap tracker. Tier 1 is fully closed. §3.1,
   §3.12 just landed this session. §2.9 Dialog / §2.11 FileChooser
   are the next large pair (deferred together — see below).
3. This file — block-level commit summary below.
4. `docs/NEXT_SESSION.md` — specific next-block brief (currently
   pointed at §2.9 Dialog + §2.11 FileChooser, bundled because both
   require a NAPI event-dispatch refactor).

Set up the cloned Playwright at `/tmp/playwright` if it isn't there:

```bash
git clone https://github.com/microsoft/playwright /tmp/playwright
```

## What just landed this session (2026-04-21)

Two blocks committed back-to-back:

### §3.1 — Navigation returns Response (commit `3c26547`)

`page.goto` / `reload` / `goBack` / `goForward` (and `frame.goto`)
return `Promise<Response | null>` matching Playwright verbatim. New
`NavRequestSlot` helper (cheap `Arc<Mutex<Option<Request>>>`) shared
between each page and its backend network listener. CDP + BiDi
observe the response via `is_navigation_request`; WebKit returns
`null` (documented §1.4 gap — stock `WKWebView` has no public API for
main-doc response observability). NAPI `ts_return_type = "Promise<Response | null>"`
on every method; QuickJS returns `Option<ResponseJs>`. Rule-9 coverage:
5 Rust integration tests × 4 backends + 6 NAPI tests × 2 backends.

### §3.12 — Regex on `getBy*` + `RoleOptions.name` (this commit)

Every `getBy*` matcher accepts `string | RegExp` — Playwright parity
for `page.getByRole('button', { name: /submit/i })`,
`page.getByText(/hello \d+/)`, etc.

New `options::StringOrRegex` enum. `RoleOptions.name` changes from
`Option<String>` to `Option<StringOrRegex>`. Selector builders are
rewritten to emit Playwright-native `internal:text=` /
`internal:label=` / `internal:attr=[name=<escaped>]` /
`internal:testid=[data-testid=<escaped>]` /
`internal:role=<role>[...]` forms with ports of Playwright's
`escapeForTextSelector` / `escapeForAttributeSelector` /
`escapeRegexForSelector` (from
`/tmp/playwright/packages/isomorphic/stringUtils.ts`). Literal
strings encode as `"quoted"i`/`"quoted"s`; regexes encode as
`/source/flags` with `>>` escaped.

The selector parser (`selectors.rs`) gained `InternalText`,
`InternalLabel`, `InternalAttr`, `InternalTestId`, `InternalRole`
engine variants. The injected-JS adapter (`injected/index.ts::executeSelector`)
passes `internal:*` bodies through unchanged so Playwright's bundled
selector engine (verbatim from §3.9) does the regex matching natively.
Bundle rebuilt — 164.3 KB.

NAPI: `ts_args_type = "text: string | RegExp"` / `"testId: string | RegExp"`
on every `getBy*`; `RoleOptions.name` typed `Option<Either<String,
JsRegExpLike>>` with `ts_type = "string | RegExp"`. Reuses the
`JsRegExpLike` prototype-chain trick — no `{ regexSource, regexFlags }`
wire shape ever exposed.

QuickJS: new `string_or_regex_from_js` helper reads real JS RegExp
instances via `source`/`flags` prototype getters. `getByRole`
gained its options bag (was missing — shipped alongside).

Rule-9 coverage:
- Rust integration: 4 tests × 4 backends (`tests/backends_support/getby_regex.rs`).
- NAPI: 9 tests × 2 CDP backends = 18 assertions (`test/getby-regex.test.ts`).

### Baseline after this commit (must stay green)

```
cargo clippy --workspace --all-targets -- -D warnings   # clean
cargo test --workspace --lib                            # 122 core
cd crates/ferridriver-node && bun run build:debug
cd <repo root> && bun test                              # 799 bun (was 781)
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1   # 4/4 backends
```

## Next session priority queue

1. **`§2.9 Dialog as first-class handle` + `§2.11 FileChooser`** — bundle
   them together. Both require the NAPI event-dispatch to pass live
   class instances (not JSON snapshots) to `page.on('dialog', ...)` /
   `page.on('filechooser', ...)`. That refactor is load-bearing for every
   future event-handle API (Dialog, FileChooser, Download-as-handle
   §2.10, Page.on('popup') §3.22) — best done once, correctly. Plan:
   - Extend NAPI `page.on` / `page.once` / `page.waitForEvent` to build
     threadsafe functions that pass `NapiDialog` / `NapiFileChooser`
     instances directly. Likely uses `Function<'_, Unknown, ()>` + a
     per-event builder callback that constructs the right class.
   - Delete `page.set_dialog_handler` callback API; replace with
     `PageEvent::Dialog(Dialog)` carrying a live handle.
   - CDP / BiDi: route `Page.javascriptDialogOpening` /
     `browsingContext.userPromptOpened` through the new Dialog handle.
     CDP: add `Page.fileChooserOpened` listener + `Page.setInterceptFileChooserDialog`.
   - WebKit: already has an IPC dialog path — thread through Dialog.
     FileChooser on WebKit is a follow-up (WKWebView's
     `runOpenPanelWithParameters:` hook needs a new IPC op).
   - Per-event-name listener counting on `EventEmitter` (auto-dismiss
     when no listener; `beforeunload` auto-accepts per Playwright).
2. `§4.1 BrowserContextOptions` — 28-field option object at context
   creation (viewport, userAgent, locale, timezone, geolocation,
   permissions, etc.). Probably 2–3 sessions.
3. `§3.17 Auto-waiting deadline parity` — replace fixed backoff with
   Playwright's exponential polling + deadline propagation.
4. `§2.10 Download as handle` — once the event-handle refactor from
   #1 is in place, this is mechanical.

See `docs/NEXT_SESSION.md` for the §2.9 + §2.11 block brief.

## Ground rules reminder

- Rule 1: core is source of truth; bindings are thin delegators.
- Rule 2: all three layers update in the same commit.
- Rule 4: every backend real — `FerriError::Unsupported` / honest
  `None` only where the protocol genuinely can't.
- Rule 6: read `/tmp/playwright/...` before coding each signature.
- Rule 7: rebuild NAPI + diff generated `index.d.ts` against
  `/tmp/playwright/packages/playwright/types/test.d.ts` after every
  binding change.
- Rule 9: per-backend integration test on every backend before
  flipping `[x]`.
- Rule 10: no `#[allow(clippy::*)]` escape hatches.
- No task / phase / rule-number annotations in source comments or
  filenames.
- No emojis. No AI attribution in commit messages.

## Carried-forward backend gaps (don't relitigate)

From §1.4 / §3.1, still real protocol limits:

- **BiDi**: response body unavailable for non-intercepted responses
  (Firefox discards bytes; Playwright's BiDi backend hits the same).
  Multi-`Set-Cookie` collapses. `request.postData()` null for
  fetch-with-body.
- **WebKit**: stock `WKWebView` exposes no public API for main-doc
  Response observability (§3.1: `goto`/`reload`/`goBack`/`goForward`
  all return `null` — documented, honest, not a shortcut), redirect
  chain, response body bytes, browser-set request headers
  (`User-Agent`), `Set-Cookie`, or WebSocket frame events. Also:
  `page.evaluate` runs in utility context isolated from the
  user-script's fetch wrap, so `page.route` cannot intercept fetches
  initiated through `page.evaluate("fetch(...)")` — only main-world
  fetches initiated from user-controlled JS.

## Known flakes

- `context.setOffline toggles network` on WebKit bun occasionally
  fails under the full suite but passes in isolation. Pre-existing
  state leak, unrelated to recent work.
- `[cdp-raw] Navigation returns Response > 404 case` occasionally
  fails in the full bun suite (observed once during §3.12) — passes
  in isolation. Likely a test-ordering artifact; if it recurs, stage
  a `beforeEach` browser reset.

## Key source locations

| area | path |
|---|---|
| Navigation methods + `NavRequestSlot` | `crates/ferridriver/src/page.rs`, `frame.rs`, `network.rs` |
| `StringOrRegex` + escape helpers | `crates/ferridriver/src/options.rs`, `locator.rs` (`build_text_like_selector`, `escape_*_for_selector`) |
| Selector engine + `internal:*` parser | `crates/ferridriver/src/selectors.rs` |
| Injected JS adapter | `crates/ferridriver/src/injected/index.ts::executeSelector` |
| NAPI getBy + RoleOptions | `crates/ferridriver-node/src/{page,frame,locator}.rs`, `types.rs::{JsRegExpLike, RoleOptions, getby_input_to_rust}` |
| QuickJS getBy helpers | `crates/ferridriver-script/src/bindings/page.rs::{string_or_regex_from_js, parse_text_options, parse_role_options}` |
| §3.1 Rust integration | `crates/ferridriver-cli/tests/backends_support/navigation_response.rs` |
| §3.12 Rust integration | `crates/ferridriver-cli/tests/backends_support/getby_regex.rs` |
| §3.1 NAPI tests | `crates/ferridriver-node/test/navigation-response.test.ts` |
| §3.12 NAPI tests | `crates/ferridriver-node/test/getby-regex.test.ts` |
| Rules + lessons | `CLAUDE.md` |
