# Handover — next Playwright-parity session

Read-first for any session continuing work. Overwrite this file with a
fresh summary at the end of each block.

## Cross-device setup

Everything needed is committed. Next session should read, in order:

1. `CLAUDE.md` — the Playwright-parity rules and consolidated
   lessons. Authoritative cross-device source.
2. `PLAYWRIGHT_COMPAT.md` — gap tracker. §1.4 is the next Tier-1
   item.
3. This file — block-level commit summary below.

Set up the cloned Playwright at `/tmp/playwright` if it isn't there:

```bash
git clone https://github.com/microsoft/playwright /tmp/playwright
```

## What just landed

The evaluate-API consolidation is complete. One commit, no
`PLAYWRIGHT_COMPAT.md` checkboxes flipped.

Highlights:

- **Surface now matches Playwright's public API exactly**:
  `page.evaluate(fn, arg?)`, `page.evaluateHandle(fn, arg?)`,
  `frame.evaluate` / `frame.evaluateHandle`,
  `locator.evaluate` / `evaluateHandle` / `evaluateAll`,
  `jsHandle.evaluate` / `evaluateHandle` / `jsonValue`,
  `elementHandle.$eval` / `$$eval`. No more `WithArg` suffixes.
- **`pageFunction` accepts `string | Function` everywhere** (NAPI +
  QuickJS). Matches Playwright's `String(pageFunction)` +
  `typeof fn === 'function'` check.
- **Rehydration to native JS** in NAPI + QuickJS returns: `Date`,
  `RegExp`, `BigInt`, `URL`, `Error`, typed arrays, `ArrayBuffer`,
  `NaN`, `±Infinity`, `undefined`, `-0`. Mirrors Playwright's
  `parseSerializedValue`. All `*Wire` escape-hatch methods deleted.
- **Utility-eval wrapper is now `async` + `await`s** on every backend
  (CDP, WebKit, BiDi) so Promise-returning expressions settle before
  the wrapper JSON.stringify's the result.
- **Frame gets a native `evaluate` primitive**; Page delegates to
  main frame as per Playwright.
- **Locator uses `retry_resolve!` + delegate-to-ElementHandle** so
  auto-wait is on the evaluate path automatically.

Baseline, all green after consolidation:

```
cargo clippy --workspace --all-targets -- -D warnings  # clean
cargo test --workspace --lib                           # 119 core
cd crates/ferridriver-node && bun run build:debug && bun test   # 730/730
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1   # 4/4
```

## Next session: Tier-1 §1.4

`PLAYWRIGHT_COMPAT.md` §1.4 — Request / Response / WebSocket
lifecycle objects — is the next Tier-1 item and the largest
remaining one in Tier 1.

Canonical Playwright sources to read before starting:

- `/tmp/playwright/packages/playwright-core/src/client/network.ts`
  — `Request`, `Response`, `Route`, `WebSocket`.
- `/tmp/playwright/packages/playwright-core/src/server/network.ts`
  — server-side shape each backend delivers.
- Existing ferridriver spots: `crates/ferridriver/src/route.rs`,
  `crates/ferridriver-mcp/src/tools/network.rs`, per-backend
  network events in `backend/{cdp,bidi,webkit}/*`.

The MCP and QuickJS bindings don't yet expose `Request`/`Response`
as first-class handle-like objects with fluent accessors (`.url()`,
`.method()`, `.headers()`, `.postData()`, `.response()` /
`.request()`, `.body()`, `.text()`, `.ok()`, `.finished()`).
ferridriver has `ResponseEvent`/`RequestEvent` structs (snapshot-
shaped) on the events bus but not the object lifecycle.

Playwright parity requires:

1. Core types `Request`, `Response`, `WebSocket` with the full
   accessor surface.
2. Events delivered with live object references (not just POJO
   snapshots) so `.response()` resolves against the current state.
3. NAPI + QuickJS bindings exposing each accessor (and Rule-2
   `ts_args_type` / `ts_return_type` where napi-rs inference
   diverges from Playwright's `types.d.ts`).
4. Rule-9 integration tests on all 4 backends (cdp-pipe, cdp-raw,
   bidi, webkit).

Consider a short design pass before coding — `Request`/`Response`
are stateful and interact with the route-handling pipeline already
shipped in `route.rs`. Split the work by sub-surface
(Request → Response → WebSocket) rather than by backend.

## Ground rules (unchanged)

- Rule 1: core is source of truth; NAPI / QuickJS bindings mirror.
- Rule 2: all three layers update in the same commit; diff NAPI's
  generated `index.d.ts` against Playwright's `test.d.ts`.
- Rule 4: every backend real — no stubs; typed
  `FerriError::Unsupported { reason }` only where the protocol
  genuinely can't.
- Rule 6: read `/tmp/playwright/...` before coding the signature.
- Rule 9: per-option integration test on every backend before
  flipping `[x]`.
- Rule 10: no escape hatches. No `#[allow(clippy::...)]` without
  explicit `reason = "…"`; never on `dead_code` except the
  phase-scaffolding exception in `CLAUDE.md`.
- No task / phase / rule-number annotations in source comments or
  filenames — see
  `memory/feedback_no_task_phase_annotations_in_source.md`.
- No emojis. No AI attribution in commit messages.

## Tests that must stay green

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd crates/ferridriver-node && bun run build:debug && bun test
cargo build -p ferridriver-cli
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1
```

Current baseline after the consolidation: 119 core tests,
730 NAPI bun tests, 4/4 backend integration tests, workspace-wide
clippy clean.

## Known flakes

- `context.setOffline toggles network` on WebKit bun occasionally
  fails under the full suite but passes in isolation. Pre-existing
  state leak, unrelated to the consolidation.

## Key source locations

| area | path |
|---|---|
| Handle types + dual backing | `crates/ferridriver/src/{js_handle,element_handle}.rs` |
| Backend primitive | `crates/ferridriver/src/backend/mod.rs::AnyPage::call_utility_evaluate`, `backend/cdp/mod.rs::UTILITY_EVAL_WRAPPER` |
| Per-backend eval + release + element-from-remote | `crates/ferridriver/src/backend/{cdp,bidi,webkit}/*` |
| Wire serializer (isomorphic) | `crates/ferridriver/src/protocol/serializers.rs` |
| Injected utility script | `crates/ferridriver/src/injected/{utilityScript,isomorphic/utilityScriptSerializers}.ts` |
| Page / Frame / Locator evaluate | `crates/ferridriver/src/{page,frame,locator}.rs` |
| NAPI rehydration + function-source extraction | `crates/ferridriver-node/src/serialize_out.rs` |
| QuickJS rehydration + `extract_page_function` | `crates/ferridriver-script/src/bindings/convert.rs` |
| NAPI bindings | `crates/ferridriver-node/src/{page,frame,locator,js_handle,element_handle}.rs` |
| QuickJS bindings | `crates/ferridriver-script/src/bindings/{page,frame,locator,js_handle,element_handle}.rs` |
| Integration test client + helpers | `crates/ferridriver-cli/tests/backends_support/client.rs` |
| Handle-surface tests | `crates/ferridriver-cli/tests/backends_support/handle_surface.rs` |
| NAPI tests | `crates/ferridriver-node/test/handles.test.ts` |
| Rules + lessons | `CLAUDE.md` |
