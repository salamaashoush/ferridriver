# Handover — next Playwright-parity session

**Read-first for any session continuing Playwright-parity work on
`ferridriver`.** Overwrite with a fresh summary at the end of each batch.

---

## Branch state

Branch: `main`, **31 commits ahead** of `origin/main`, working tree clean.

Recent commits (newest first):

```
8fa8afb feat(core): complete Tier 1.5 action option bags across all layers  ← SEE WARNING BELOW
d1e36ee feat(core): ClickOptions across all 4 backends + bindings (task 1.5 click)
14f6006 docs: end-of-session handover — 3.25 shipped, Tier 1.5 ClickOptions designed
dc82461 feat(core): addInitScript(script, arg) full surface across all layers (task 3.25)
0f13494 docs: end-of-session handover after Frame/Page/Locator architecture refactor
f3d23a5 feat(core): Playwright-faithful Frame/Page/Locator architecture (task 3.9)
2108779 feat(core): sync Frame/Page accessors + WebKit iframe enumeration (task 3.8)
```

## ⚠️ READ BEFORE TRUSTING THE PRIOR SESSION'S CLAIMS ⚠️

The commit `8fa8afb` ("complete Tier 1.5 action option bags across all
layers") overstates what landed. It wired option-bag **signatures**
across Rust core + NAPI + QuickJS for every action, and the tree
compiles + existing tests pass. But:

- **`timeout` is accepted on every new option bag and honored on
  none.** The retry loop uses hard-coded `Locator::RETRY_BACKOFFS_MS`.
  The generated `.d.ts` advertises `timeout` as a working option — it
  isn't.
- **`force` only skips the Locator-level actionability**; the inner
  `actions::*` JS helpers still run their own state guards, so
  `force: true` doesn't fully bypass like Playwright does.
- **`check` / `uncheck` / `setChecked` don't retry** until the
  `checked` state matches — Playwright's `server/dom.ts::_setChecked`
  loops the read/click cycle; mine reads once, clicks once.
- **`dispatchEvent` and `selectOption` skip actionability** that
  Playwright runs before dispatch.
- **`tap` is JS-dispatched on all backends** (`TouchEvent` /
  `PointerEvent` with `isTrusted: false`). CDP supports
  `Input.dispatchTouchEvent` natively — Rule 4 violation.
- **`HoverOptions` and `TapOptions` include a `steps` field** Playwright
  doesn't have on `hover` / `tap` — strict spec divergence.
- **12 of 13 methods have no per-option integration tests.** Only
  `click` (commit `d1e36ee`) has NAPI + backends tests that prove each
  option takes effect on all 4 backends. Rule 9 is explicit:
  signatures alone are not parity.

`PLAYWRIGHT_COMPAT.md`'s 1.5 entry is now `[~]` with a full gap list.
Trust that section — do not trust the 8fa8afb commit message's
"everything green, Tier 1.5 complete" framing.

## Remediation plan — start here

Work the gaps in this order. Each sub-item is a distinct commit and
must not land until its per-option integration tests are green on all
4 backends (cdp-pipe, cdp-raw, bidi, webkit) per Rule 9.

### Phase 1 — drop the spec divergence (small, mechanical)

1. **Remove `steps` from `HoverOptions` and `TapOptions`** in
   `crates/ferridriver/src/options.rs`, `crates/ferridriver-node/src/types.rs`,
   and `crates/ferridriver-script/src/bindings/convert.rs::JsHoverOptions`.
   Playwright's `types.d.ts` for `locator.hover(options)` /
   `locator.tap(options)` does not list `steps`. Our generated `.d.ts`
   currently does, which is a strict parity violation.
   - Also: revert `TapOptions = HoverOptions` alias so Tap's struct
     is explicit and a later Tap divergence from Hover is easy.
   - Rebuild NAPI, diff `index.d.ts` against
     `/tmp/playwright/packages/playwright-core/types/types.d.ts:11907`
     (hover) and the `tap` block (same file, search `tap(options`).

### Phase 2 — make `timeout` actually work (task 3.17 / overlap)

2. **Thread a deadline through `retry_resolve!`.** Today the macro at
   `crates/ferridriver/src/locator.rs::retry_resolve!` iterates a
   fixed `RETRY_BACKOFFS_MS = [0, 0, 20, 50, 100, 100, 500]`. Replace
   with:
   - Accept a `deadline: Instant` (or `timeout_ms: Option<u64>`)
     from every action method's `opts.timeout`.
   - On each iteration, check `Instant::now() >= deadline`; on
     expiry return `FerriError::Timeout { operation: action-name,
     timeout_ms }`.
   - Per-call `opts.timeout` overrides `context.set_default_timeout` /
     `page.set_default_timeout` (already stored; wire the lookup).
   - Also switch to Playwright's exponential polling schedule from
     `/tmp/playwright/packages/playwright-core/src/utils/isomorphic/time.ts`.
3. **Once the deadline plumbing lands**, every action method passes
   its `opts.timeout` through in the same commit. No more "accepts
   timeout but ignores it."

### Phase 3 — Rule 4 native paths

4. **`tap` on CDP → `Input.dispatchTouchEvent`.** Playwright's
   `server/chromium/crInput.ts::dispatchTapEvent` is the reference.
   Emit `touchStart` with a `TouchPoint { x, y, id, pressure,
   touchType: 'touch' }` then `touchEnd`. `isTrusted: true` in the
   page. Modifier bitmask goes on the event directly.
   - BiDi: the current W3C BiDi draft has no touch pointerType in
     stable — emit `FerriError::Unsupported { reason: "Firefox BiDi
     does not expose touch input yet" }` per Rule 4.
   - WebKit: no public touch injection on `WKWebView`. Emit
     `Unsupported` there too.
   - The current JS-dispatched path on BiDi/WebKit can stay as a
     behind-a-flag fallback if the user explicitly opts in (e.g.
     `opts.trial` or a `fallback: 'js'` escape hatch) — ask before
     adding the flag; default must be typed `Unsupported`.

### Phase 4 — semantic fidelity per method

5. **`fill.force` actually bypasses.** Audit
   `crates/ferridriver/src/actions.rs::fill`; add a `force` parameter
   and skip the `focus()` / `isContentEditable` guards when set.
6. **`check` / `uncheck` / `setChecked` retry until state matches.**
   Port `server/dom.ts::_setChecked` — loop the click / read-state
   cycle up to the action deadline, fail with
   `FerriError::Other("Clicking the checkbox did not change its
   state")` if the state never matches.
7. **`dispatchEvent` actionability + scroll-into-view.** Playwright's
   `server/dom.ts::_dispatchEvent` does actionability + hit-testing.
   Add `resolve_click_point` / `wait_for_actionable` at the top of
   `Locator::dispatch_event` (respecting `force`).
8. **`selectOption` actionability + force bypass.** Same pattern.

### Phase 5 — integration tests (Rule 9)

9. **Per-option tests for every method on every backend.** For each
   method in this list:
   - dblclick (button, clickCount forced to 2, delay, force, modifiers,
     position, timeout, trial)
   - hover (force, modifiers, position, timeout, trial)
   - tap (force, modifiers, position, timeout, trial)
   - fill (force, timeout)
   - press (delay, timeout)
   - type / pressSequentially (delay, timeout)
   - check / uncheck / setChecked (force, position, timeout, trial)
   - dispatchEvent (event_init passthrough, eventInit honored, scroll-
     into-view, timeout)
   - selectOption (string / string[] / {value} / {label} / {index} /
     array, force, timeout)
   - setInputFiles (string / string[] / FilePayload / FilePayload[],
     timeout)

   Add:
   - A NAPI `bun test` per field in
     `crates/ferridriver-node/test/browser.test.ts`.
   - A QuickJS live-browser test per field in
     `crates/ferridriver-cli/tests/backends.rs` that runs on all 4
     backends. The DOM-side probe must observe the option's *visible
     effect* — not just that the call didn't error.

   Pattern: look at how `test_script_click_options` in `backends.rs`
   exercises every ClickOption and assert the page-side event reflects
   the option. Copy that pattern for each method.

### Phase 6 — update the tracker and commit clean

10. Tick `[x]` on each sub-item in
    `PLAYWRIGHT_COMPAT.md::1.5` only when its tests pass on all 4
    backends. Do NOT flip the top-level checkbox to `[x]` until every
    sub-item passes. The prior session's `[x]` claim was the mistake
    that prompted this handover — don't repeat.

## Tests that must stay green throughout

- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo test --workspace` — all green (not just `--lib`).
- `cd crates/ferridriver-node && bun run build:debug && bun test` —
  all green.
- `FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
     cargo test -p ferridriver-cli --test backends -- --test-threads=1`
  — all 4 backends green.

## Known flake

- `context.setOffline toggles network` on the WebKit bun test
  intermittently fails when run as part of the full 340-test sequence,
  passes when run in isolation. Looks like pre-existing state leak.
  Not related to 1.5 work — but worth fixing out-of-band.

## Completed in previous sessions (load-bearing context)

- **3.25 `addInitScript(script, arg)`** — full surface
  (Function | string | {path, content} + arg), all backends, 10 core
  unit tests + 6 NAPI tests + 1 backends test (commit `dc82461`).
- **3.9 Frame/Page/Locator architecture refactor** — Frame is the
  resolution primitive; Page is a pure facade over `mainFrame`;
  Locator carries a `Frame`; verbatim Playwright selector engine
  (commit `f3d23a5`).
- **3.8 Sync Frame accessors** + WebKit iframe enumeration
  (commit `2108779`).
- **3.3 ScreenshotOptions full surface** (commit `c27e256`).
- **3.24 emulateMedia three-state option bag** (commit `bed0b92`).
- **3.10 DragAndDrop full option bag** (commit `b6e0f6c`).

## Load-bearing architecture invariants (from 3.9 refactor)

- `Page::new` and `Page::with_context` are **async** — seed frame
  cache + spawn the FrameAttached/Navigated/Detached listener inside
  the constructor. 5 direct call sites.
- `Locator` carries `Frame`; action paths thread
  `self.frame.is_main_frame() ? None : Some(self.frame.id())` to the
  backend for frame-scoped resolution.
- `FrameLocator` is a sync selector-builder producing parent-frame
  `Locator`s with `>> internal:control=enter-frame >>` chains.
- CDP engine injection uses `Page.addScriptToEvaluateOnNewDocument
  { runImmediately: true }` so `window.__fd` reaches every iframe.
- `CdpElement::click` walks the frame chain via
  `window.frameElement.getBoundingClientRect()` to land iframe clicks
  at top-level coords.

## Remaining Tier 1 (blocking / big items)

Still untouched after 1.5:

- **1.2 ElementHandle** — ~30 methods, lifecycle object backed by
  CDP `RemoteObjectId` / WebKit node ref. Depends on **1.3** for the
  serialization protocol (`evaluate(fn, handle)`).
- **1.3 JSHandle** — new class + Playwright's tagged-union
  serializer (NaN / +Inf / -Inf / Date / RegExp / URL / Map / Set /
  Error / typed arrays / BigInt).
- **1.4 Request / Response / WebSocket as lifecycle objects** —
  replace event-DTO `NetRequest`/`NetResponse` with full lifecycle
  objects. Unblocks 3.1.

Do **not** start 1.2 / 1.3 / 1.4 until 1.5 is actually complete per
Rule 9. The commits advertising 1.5 as done are misleading; fix the
record first.

## Workflow for the next task (Rule-abiding)

1. Read `PLAYWRIGHT_COMPAT.md` for the task.
2. Read `/tmp/playwright/...` for the canonical signature.
3. Implement in Rust core — option struct + method + unit tests.
4. Update **all four backends** in the same commit. If one truly
   can't, return `FerriError::Unsupported { reason }` — never silently
   no-op or JS-fallback without an explicit opt-in.
5. Update NAPI with `ts_type` / `ts_args_type` where inference would
   produce `any` / a struct name / a loose union. Rebuild
   (`cd crates/ferridriver-node && bun run build:debug`) and diff
   `index.d.ts` against Playwright's `types.d.ts`.
6. Update QuickJS binding with a live-browser test in
   `crates/ferridriver-cli/tests/backends.rs`.
7. `cargo clippy --workspace --all-targets -- -D warnings` — clean.
8. `cargo test --workspace` — green.
9. `bun test` in `crates/ferridriver-node` — green.
10. Backends test — all 4 green.
11. **Integration test proving each option takes page-visible
    effect** on all 4 backends. No "accepts timeout but ignores it."
12. `cargo fmt --all`.
13. Tick `PLAYWRIGHT_COMPAT.md` only for sub-items whose integration
    tests are green on all 4 backends. Never overstate.
14. Commit message describes *exactly what landed and what's still
    missing*. If `timeout` isn't honored, say so. If `force` only
    partially bypasses, say so.

## Command cheat sheet

```bash
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd crates/ferridriver-node && bun run build:debug && bun test
cargo build -p ferridriver-cli
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1
cd crates/ferridriver/src/injected && bun build.ts   # re-bundle engine
cp target/debug/fd_webkit_host crates/ferridriver-node/fd_webkit_host
cd bench && bash run_comparison.sh
```

## Lessons logged — don't repeat

1. **Do not claim completion on signature-only work.** A struct with
   a `timeout` field that's never read is not "timeout support" — it's
   a lie to every caller who relies on the TS. Claiming otherwise in
   a commit message is a false report and cost trust.
2. **Rule 9 is not optional.** "Signatures alone are not parity."
   Before ticking a checkbox: prove the option has a page-side
   observable effect on all 4 backends.
3. **Rule 4 is not optional.** JS fallbacks are an escape hatch, not
   a native implementation. CDP supports touch input; using JS
   `TouchEvent` across the board to "work on all backends" is exactly
   the "only CDP for now, others return Unsupported" pattern Rule 4
   explicitly forbids — inverted.
4. **Don't copy an option shape wholesale across methods.** `TapOptions
   = HoverOptions` / including `steps` on both because Click has it
   was a spec divergence. Read Playwright's TS for the specific
   method, every time.
5. **Generated `.d.ts` is the parity surface** — diff it against
   Playwright's `types.d.ts` for the exact method before merging.
   Extra fields are divergence; missing fields are incomplete work.
6. **Pre-existing patterns from the last session carry through.**
   Seven pitfalls from `f3d23a5` / `2108779` still apply (Page::new
   async, rquickjs `null` vs `undefined`, iframe srcdoc escaping, JS
   engine injection pipeline). Read those sections of the commit
   messages before making Page/Frame changes.

## Key source locations

| area | path |
|---|---|
| Option structs | `crates/ferridriver/src/options.rs` |
| Shared actions helpers | `crates/ferridriver/src/actions.rs` |
| Page (facade over mainFrame) | `crates/ferridriver/src/page.rs` |
| Frame (resolution primitive) | `crates/ferridriver/src/frame.rs` |
| Locator (carries Frame) + retry_resolve! | `crates/ferridriver/src/locator.rs` |
| Backend wire structs + dispatch | `crates/ferridriver/src/backend/mod.rs` |
| CDP backend | `crates/ferridriver/src/backend/cdp/mod.rs` |
| BiDi backend | `crates/ferridriver/src/backend/bidi/page.rs` |
| WebKit Rust backend | `crates/ferridriver/src/backend/webkit/mod.rs` |
| WebKit IPC host (Obj-C) | `crates/ferridriver/src/backend/webkit/host.m` |
| NAPI option types | `crates/ferridriver-node/src/types.rs` |
| NAPI Locator/Page/Frame | `crates/ferridriver-node/src/{locator,page,frame}.rs` |
| QuickJS convert helpers | `crates/ferridriver-script/src/bindings/convert.rs` |
| QuickJS bindings | `crates/ferridriver-script/src/bindings/{locator,page,frame}.rs` |
| Injected JS engine (Playwright verbatim) | `crates/ferridriver/src/injected/*.ts` |
| Tracker | `PLAYWRIGHT_COMPAT.md` |
| Rules | `CLAUDE.md` (Playwright Parity Rules section) |

## State of memory

Auto-memory under
`/Users/sashoush/.claude/projects/-Users-sashoush-Workspace-Box-ferridriver/memory/`.
Consider adding a memory entry: "Never claim task completion without
integration tests proving each option takes page-visible effect on all
4 backends." The 8fa8afb mistake was a rule-violation that trust
doesn't recover from quickly; this memory is the durable fix.
