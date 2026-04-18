# Evaluate-API consolidation — done

Landed in a single commit. The evaluate surface now matches Playwright's
public API exactly across all three layers.

## What landed

- Core renames in `crates/ferridriver/src/`:
  - `Page::evaluate_with_arg` → `Page::evaluate`
  - `Page::evaluate_handle_with_arg` → `Page::evaluate_handle`
  - `JSHandle::evaluate_with_arg` → `JSHandle::evaluate`
  - `JSHandle::evaluate_handle_with_arg` → `JSHandle::evaluate_handle`
- Legacy expression-only methods deleted:
  - `Page::evaluate(&str)`, `Page::evaluate_str(&str)`
  - `Frame::evaluate(&str)`, `Frame::evaluate_str(&str)`
  - `Locator::evaluate(&str)`, `Locator::evaluate_all(&str)`
- `Frame::evaluate(fn, arg, is_function)` and
  `Frame::evaluate_handle(fn, arg, is_function)` added as the
  frame-scoped primitive. Main-frame calls pass `frame_id=None`;
  child-frame calls thread `self.id()`.
- `Page::evaluate` / `Page::evaluate_handle` delegate to
  `main_frame()`, mirroring Playwright's
  `client/page.ts:515`.
- `Locator::evaluate(fn, arg, is_function, options)` /
  `Locator::evaluate_handle(...)` added via
  `retry_resolve!` + delegate to `ElementHandle::evaluate`
  (matching Playwright's `_withElement(h => h.evaluate(fn, arg))`).
- `Locator::evaluate_all(fn, arg, is_function)` added: resolves every
  matching element into an array handle (via `window.__fd.selAll`)
  and calls the user function with that array as the first arg.
- NAPI and QuickJS bindings renamed in lockstep:
  `evaluate`, `evaluateHandle`, `evaluateAll`, `$eval`, `$$eval`.
  No more `WithArg` suffixes.
- Bindings accept `string | Function` everywhere (NAPI via a
  `NapiPageFunction` adapter that calls `coerce_to_string` on the
  Unknown; QuickJS via `extract_page_function` which reads
  `is_function()` + invokes global `String(v)` for functions),
  matching Playwright's `String(pageFunction)` +
  `typeof fn === 'function'` at `client/frame.ts:196`.
- NAPI and QuickJS rehydrate the wire shape to native JS values in
  `evaluate`/`jsonValue`/`$eval`/`$$eval` returns: `Date`,
  `RegExp`, `BigInt`, `URL`, `Error`, typed arrays, `ArrayBuffer`,
  `NaN`, `±Infinity`, `undefined`, `-0`. Matches Playwright's
  `parseSerializedValue` at
  `/tmp/playwright/packages/playwright-core/src/protocol/serializers.ts:19`.
- All 3 `evaluateWire` / `evaluateWithArgWire` / `jsonValueWire`
  escape-hatch methods are deleted; `evaluate` now gives native JS
  directly, same as Playwright.
- Utility-eval wrapper (CDP + WebKit + BiDi) is now an `async function`
  that `await`s `us.evaluate(...)` before JSON.stringify-ing the
  isomorphic wire shape — fixes Promise-returning expressions that
  the old sync wrapper silently stringified as `{}`.
- ~100 call sites migrated across the workspace (tests, BDD steps,
  MCP tool, component-test mount/unmount, expect/locator, worker).

## Baseline

- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo test --workspace --lib` — 119 core tests pass.
- `cd crates/ferridriver-node && bun test` — 730/730 pass.
- Backend integration (`cargo test -p ferridriver-cli --test backends`)
  — 4/4 backends green (cdp-pipe, cdp-raw, bidi, webkit).

## Tracker

`PLAYWRIGHT_COMPAT.md` unchanged — this was a code-shape refactor
plus rehydration/Function-acceptance fix, not a new Tier-1 tick.

Next in the queue: Tier-1 §1.4 (Request / Response / WebSocket
lifecycle objects). See `HANDOVER.md`.
