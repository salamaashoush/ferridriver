# Handover — next Playwright-parity session

**Read-first for any session continuing Playwright-parity work on
`ferridriver`.** Overwrite with a fresh summary at the end of each batch.

---

## Branch state

Branch: `main`, **37 commits ahead** of `origin/main`, working tree clean.

Recent commits (newest first, all from the 2026-04-18 1.5 remediation session):

```
cb0e8b9 fix(core): dispatchEvent timeout + selectOption force/timeout (task 1.5 phase 4c + 4d)
6ffe86b fix(core): check/uncheck verify final state + reject radio uncheck (task 1.5 phase 4b)
ea3da35 fix(core): fill.force actually bypasses ['visible','enabled','editable'] (task 1.5 phase 4a)
170bc3d feat(core): CDP native tap via Input.dispatchTouchEvent (task 1.5 phase 3, Rule 4)
e2bdc85 fix(core): honor opts.timeout on every Locator action (task 1.5 phase 2)
b77b8c7 fix(core): drop `steps` from Hover/Tap options (task 1.5 phase 1)
4d14e94 docs: correct 1.5 completion claim, hand off gaps to next session  ← the previous handover
8fa8afb feat(core): complete Tier 1.5 action option bags across all layers  ← the overstated claim
d1e36ee feat(core): ClickOptions across all 4 backends + bindings (task 1.5 click)
```

## What the 1.5 remediation session fixed

- **Phase 1 (`b77b8c7`)** — Dropped the bogus `steps` field from
  `HoverOptions` + `TapOptions`. Broke the `TapOptions = HoverOptions`
  alias so Tap has its own struct. Generated NAPI `.d.ts` now matches
  Playwright's `types.d.ts` for locator.hover / locator.tap byte-for-byte.
- **Phase 2 (`e2bdc85`)** — `retry_resolve!` macro now takes
  `$timeout_ms: Option<u64>` and `$op: &str`. Effective deadline =
  `opts.timeout.or(page.default_timeout())`; `0` means infinite (Playwright
  parity). Polling schedule `[0,0,20,50,100,100,500]` clamps at the last
  value and checks the deadline every iteration. Returns
  `FerriError::Timeout { operation, timeout_ms }` on expiry. Every action
  call site (`click`, `dblclick`, `right_click`, `fill`, `clear`, `press`,
  `type`, `hover`, `focus`, `tap`, `set_checked`) threads
  `opts.timeout` + an operation name into the macro.
- **Phase 3 (`170bc3d`)** — Tap is now CDP-native via
  `Input.dispatchTouchEvent`. Before dispatch we flip
  `Emulation.setTouchEmulationEnabled { enabled: true, maxTouchPoints: 1 }`
  so Chromium's renderer routes the events to DOM listeners. BiDi and
  WebKit return typed `FerriError::Unsupported` per Rule 4 (no public
  touch-injection primitive on either). `FerriError::From<String>`
  upgrades backend strings with the `unsupported:` prefix to typed
  `FerriError::Unsupported`.
- **Phase 4a (`ea3da35`)** — `actions::fill(element, page, value, force)`.
  When `force: false`, call `fd.checkElementStates(['visible','enabled',
  'editable'])` and return `error:not<state>` as the Err; the retry
  loop's expanded retriable pattern (`error:not*` prefix) keeps polling
  until the deadline. With `force: true`, skip the pre-check and fill
  through a readonly input.
- **Phase 4b (`6ffe86b`)** — `Locator::set_checked` now reads state via
  `fd.getChecked(this)` (handles ARIA-checkable roles), rejects
  uncheck-of-checked-radio with the exact Playwright message, and
  verifies the post-click state; if it doesn't match the target,
  throws `"Clicking the checkbox did not change its state"`.
  `trial: true` short-circuits before the click and skips the verify.
- **Phase 4c (`cb0e8b9`)** — `Locator::dispatch_event` is now wrapped in
  `retry_resolve!` so `opts.timeout` actually fires. Playwright's own
  `dispatchEvent` does NOT run actionability (the previous handover was
  wrong about that); we match.
- **Phase 4d (`cb0e8b9`)** — `Locator::select_option` threads
  `opts.timeout` through the retry loop and honors `opts.force`.
  Without force, a `fd.checkElementStates(['visible','enabled'])` gate
  feeds the retry loop. With force, bypasses.

Every phase landed with per-backend integration tests in
`tests/backends.rs` + NAPI tests in `test/browser.test.ts`, covering
all 4 backends (cdp-pipe, cdp-raw, bidi, webkit) for the Rust side and
3 (cdp-pipe, cdp-raw, webkit) for the NAPI side.

## What's left in 1.5

The top-level 1.5 checkbox stays `[~]` until the per-option integration
coverage for the remaining methods lands. These only need **Phase 5
tests** — the semantics are correct:

1. **`dblclick`** — `DblClickOptions` lowers to `ClickOptions { click_count: Some(2) }`. Add a backends test + NAPI test that a real `dblclick` handler fires (not just that the call didn't error).
2. **`press`** — add a backends test that `press('A', { delay: 120 })` holds keyDown for ≥80ms (measure via `Date.now()` between keydown/keyup listener fires) on every backend.
3. **`type` / `pressSequentially`** — add a backends test that `type('hello', { delay: 50 })` produces a ≥200ms total gap across 5 chars (and a corresponding NAPI test).
4. **`setInputFiles`** — per-option tests for each polymorphic form: `string`, `string[]`, `FilePayload`, `FilePayload[]`. Probe `input.files[0].name` / `size` afterwards.

For each: copy the `test_script_click_options` pattern in
`crates/ferridriver-cli/tests/backends.rs` (a `c.nav(html-with-probes)`
+ `c.script_value(...)` + `assert_eq!(json probe)` per field) and mirror
with a `bun test` in `crates/ferridriver-node/test/browser.test.ts`.
Only flip each `[~]` to `[x]` in `PLAYWRIGHT_COMPAT.md` after the test
passes on all 4 backends. The top-level 1.5 checkbox flips once every
sub-item is proven.

## Ground rules (non-negotiable, unchanged)

- **Rule 9**: Signatures alone are not parity. Every option field gets
  a DOM-visible integration test on every backend the API claims to
  support.
- **Rule 4**: Every public API must work on every backend, or return
  typed `FerriError::Unsupported { reason }` where the protocol
  genuinely can't (tap on BiDi/WebKit is the precedent).
- **Rule 10**: No escape hatches. No `#[allow(clippy::...)]`, no
  `eslint-disable`, no `--no-verify`, no `git reset --hard` /
  `git checkout --` without user confirmation.
- Any new `opts.timeout` field that reaches a method MUST propagate to
  the retry loop deadline. No more accepting-but-ignoring.
- Never claim "complete" / "full surface" / "everything green" in a
  commit message unless Rule 9 tests are passing on all 4 backends.

## Tests that must stay green

- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo test --workspace` — all green.
- `cd crates/ferridriver-node && bun run build:debug && bun test` —
  all green (651 at last count).
- `FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver
     cargo test -p ferridriver-cli --test backends -- --test-threads=1`
  — all 4 backends green.

## Known flake (pre-existing, not related to 1.5)

- `context.setOffline toggles network` on the WebKit bun test
  intermittently fails when run as part of the full 340+ test sequence,
  passes when run in isolation. Pre-existing state leak, orthogonal to
  1.5 work.

## Remaining Tier 1 (blocking / big items)

Still untouched after 1.5 semantic remediation:

- **1.2 ElementHandle** — ~30 methods, lifecycle object backed by
  CDP `RemoteObjectId` / WebKit node ref. Depends on **1.3** for the
  serialization protocol (`evaluate(fn, handle)`).
- **1.3 JSHandle** — new class + Playwright's tagged-union
  serializer (NaN / +Inf / -Inf / Date / RegExp / URL / Map / Set /
  Error / typed arrays / BigInt).
- **1.4 Request / Response / WebSocket as lifecycle objects** —
  replace event-DTO `NetRequest`/`NetResponse` with full lifecycle
  objects. Unblocks 3.1.

Do **not** start 1.2 / 1.3 / 1.4 until 1.5's Phase 5 coverage lands
and the top-level 1.5 checkbox flips to `[x]`.

## Workflow for the next task (Rule-abiding)

1. Read `PLAYWRIGHT_COMPAT.md` for the task.
2. Read `/tmp/playwright/packages/playwright-core/types/types.d.ts` for
   the canonical signature.
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
    missing*.

## Lessons logged — don't repeat

1. **Do not claim completion on signature-only work.** The 8fa8afb
   commit that prompted this whole remediation session. Saved a
   memory entry to durably avoid.
2. **Rule 9 is not optional.** Before ticking a checkbox: prove the
   option has a page-side observable effect on all 4 backends.
3. **Rule 4 is not optional.** JS fallbacks are an escape hatch, not a
   native implementation.
4. **Read the Playwright source, don't reconstruct from memory.**
   The HANDOVER's claim that `dispatchEvent` needs actionability was
   wrong — Playwright's own `frames.ts::dispatchEvent` doesn't run
   actionability either. Checking `/tmp/playwright` first would've
   saved speculation.
5. **`innerHTML =` setContent does not execute scripts.** Tests that
   rely on script-injected listeners should use `goto(data:...)` so
   the HTML parser runs the script deterministically.
6. **CDP `Input.dispatchTouchEvent` needs `Emulation.setTouchEmulation
   Enabled { enabled: true }` first or DOM listeners never fire.**
   Not obvious from the protocol docs — found via test failure.

## Key source locations

| area | path |
|---|---|
| Option structs | `crates/ferridriver/src/options.rs` |
| Shared actions helpers (click/hover/tap/fill/select) | `crates/ferridriver/src/actions.rs` |
| Page (facade over mainFrame) | `crates/ferridriver/src/page.rs` |
| Frame (resolution primitive) | `crates/ferridriver/src/frame.rs` |
| Locator + `retry_resolve!` macro | `crates/ferridriver/src/locator.rs` |
| Backend wire structs + dispatch + `AnyPage::kind()` | `crates/ferridriver/src/backend/mod.rs` |
| CDP backend (+ `tap_at_with` native) | `crates/ferridriver/src/backend/cdp/mod.rs` |
| BiDi backend (tap returns Unsupported) | `crates/ferridriver/src/backend/bidi/page.rs` |
| WebKit Rust backend (tap returns Unsupported) | `crates/ferridriver/src/backend/webkit/mod.rs` |
| WebKit IPC host (Obj-C) | `crates/ferridriver/src/backend/webkit/host.m` |
| Error type (`FerriError::Unsupported`, From<String> upgrade) | `crates/ferridriver/src/error.rs` |
| NAPI option types | `crates/ferridriver-node/src/types.rs` |
| NAPI Locator/Page/Frame | `crates/ferridriver-node/src/{locator,page,frame}.rs` |
| QuickJS convert helpers | `crates/ferridriver-script/src/bindings/convert.rs` |
| QuickJS bindings | `crates/ferridriver-script/src/bindings/{locator,page,frame}.rs` |
| Injected JS engine (Playwright verbatim + our helpers) | `crates/ferridriver/src/injected/*.ts` |
| Tracker | `PLAYWRIGHT_COMPAT.md` |
| Rules | `CLAUDE.md` (Playwright Parity Rules section) |

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

## State of memory

Auto-memory under
`/Users/sashoush/.claude/projects/-Users-sashoush-Workspace-Box-ferridriver/memory/`.
The `feedback_never_claim_completion_without_rule9_tests.md` entry added
after the 8fa8afb mistake is the durable check against repeating it.
