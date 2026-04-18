# Handover — next Playwright-parity session

Read-first for any session continuing work. Overwrite this file with a
fresh summary at the end of each block.

## Cross-device setup

Everything needed is committed. Next session should read, in order:

1. `CLAUDE.md` — the Playwright-parity rules and consolidated
   lessons. Authoritative cross-device source.
2. `PLAYWRIGHT_COMPAT.md` — gap tracker. §1.4 is the next Tier-1
   item after the consolidation below.
3. `docs/CONSOLIDATE_EVALUATE_API.md` — the self-contained prompt
   for the evaluate-API consolidation session. Paste that prompt
   into a fresh Claude Code run to resume.
4. This file — block-level commit summary below.

Set up the cloned Playwright at `/tmp/playwright` if it isn't there:

```bash
git clone https://github.com/microsoft/playwright /tmp/playwright
```

## Branch state

`main`, **54 commits ahead** of `origin/main` (recent this block):

```
7cd7a41 feat(core): primitive-value JSHandle backing + Rule-9 tests; tick 1.2 / 1.3
0b1519f feat(bindings): QuickJS phase-G surface + NAPI/QuickJS handle-arg walker + BiDi non-node wire fix
843989b docs: task 1.2/1.3 end-of-session snapshot; delete FINISH_TASKS prompt
828a2bc feat(node): NAPI bindings for new JSHandle + ElementHandle surface
769ff78 feat(core): ElementHandle $eval, frame accessors, wait helpers, action bridge
f3d3cdd feat(core): Playwright-shape evaluate primitive + JSHandle value/property/asElement
```

## What's landed

**Tasks 1.2 (ElementHandle) and 1.3 (JSHandle) are `[x]` in
`PLAYWRIGHT_COMPAT.md`** — full surface shipped on core + NAPI +
QuickJS, Rule-9 tests on all four backends.

Highlights from the block:

- **Backend evaluate primitive** (`call_utility_evaluate(fn_source,
  args, handles, frame_id, is_function, return_by_value)`) matches
  Playwright's `evaluateExpression` exactly. One method per backend,
  variadic args, no receiver parameter (handles go positional via
  `{h: 0}` + `handles[0]` — same shape Playwright uses).

- **`JSHandle` dual backing** (`JSHandleBacking::Remote` /
  `JSHandleBacking::Value`) matches Playwright's `_objectId` /
  `_value` split. All three backends emit the correct variant:
  CDP reads `objectId` vs inline `value`; BiDi distinguishes Node
  / handle-bearing objects / primitives; WebKit's utility-script
  wrapper emits a `{kind: 'valueHandle'}` envelope for non-object
  results so primitives ride inline instead of allocating
  `window.__wr` entries.

- **BiDi wire fix**: non-node objects (Array / Object / Map / Set /
  Function / …) store their BiDi handle string in the
  `HandleRemote::Bidi::handle` slot with `shared_id` empty. The
  argument-emit path prefers `{type: "handle", handle}` when
  present, only falls back to `{type: "sharedReference", sharedId}`
  for node-typed remotes. The old code wrongly put the handle
  string in both slots and always emitted sharedReference, which
  BiDi rejected with "no such node".

- **Rich-arg walker**: NAPI (`NapiEvaluateArg::FromNapiValue`) and
  QuickJS (`bindings::convert::quickjs_arg_to_serialized`) detect
  top-level `JSHandle` / `ElementHandle` class instances at the
  boundary and emit via the shared core helper
  `JSHandleBacking::to_serialized_argument`. Per Rule 1, detection
  is binding-specific (unavoidable) but the packaging is core.

- **`Locator::press` focuses the element first** — real parity gap
  surfaced by the element-handle press test; Playwright's
  `server/dom.ts::_press` focuses before key dispatch.

- **Integration test restructure**: `crates/ferridriver-cli/tests/backends.rs`
  was growing past 2,700 lines. Shared helpers + the new handle-
  surface tests moved to `tests/backends_support/{client,
  handle_surface}.rs`. `backends.rs` has a NOTE at the top telling
  next sessions not to extend it — add new groups as well-named
  files under `backends_support/`.

- **Memory entry** `feedback_no_task_phase_annotations_in_source`:
  no more "Task 1.2 phase E" / rule numbers / commit SHAs in
  source comments or filenames across sessions.

## Next session: evaluate API consolidation

Playwright-parity evaluate surface is **functionally complete** but
the user-facing method names are still a mess from incremental
iteration:

```
Page::evaluate(expr)             legacy, expression-only
Page::evaluate_str(expr)         legacy stringify variant
Page::evaluate_with_arg(fn, arg) new Playwright shape
Page::evaluate_handle_with_arg   new
Frame::evaluate(expr)            legacy
Frame::evaluate_str(expr)        legacy
Locator::evaluate(expr)          legacy
Locator::evaluate_all(expr)      legacy
JSHandle::evaluate_with_arg      new
JSHandle::evaluate_handle_with_arg new
ElementHandle::evaluate_with_arg new (delegates to js_handle)
ElementHandle::evaluate_handle_with_arg new
```

Playwright's public API is:

```
Page:          evaluate(fn, arg) + evaluateHandle(fn, arg)
Frame:         evaluate(fn, arg) + evaluateHandle(fn, arg)
Locator:       evaluate(fn, arg) + evaluateAll(fn, arg) + evaluateHandle(fn, arg)
JSHandle:      evaluate(fn, arg) + evaluateHandle(fn, arg)
ElementHandle: inherits JSHandle; adds $eval / $$eval (ElementHandle only)
```

The consolidation has its own self-contained prompt at
`docs/CONSOLIDATE_EVALUATE_API.md`. **Do not interleave with other
parity work** — it's a focused rename + delete + migrate pass
touching ~74 call sites across the workspace. Ship in its own
commit.

## After consolidation: Tier-1 gap 1.4

Once the API is clean, `PLAYWRIGHT_COMPAT.md` §1.4 (Request /
Response / WebSocket lifecycle objects) is the next Tier-1 item —
the largest remaining one in Tier 1.

## Ground rules (unchanged)

- Rule 1: core is source of truth; NAPI / QuickJS bindings mirror.
- Rule 4: every backend real — no stubs; typed
  `FerriError::Unsupported { reason }` only where the protocol
  genuinely can't.
- Rule 9: per-option integration test on every backend before
  flipping `[x]`.
- Rule 10: no escape hatches. No `#[allow(clippy::...)]` without
  explicit `reason = "…"`; never on `dead_code` except the
  phase-scaffolding exception in `CLAUDE.md`.
- No task / phase / rule-number annotations in source comments or
  filenames — see `memory/feedback_no_task_phase_annotations_in_source.md`.
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

Current baseline: 119 core tests, 727 NAPI bun tests, 4/4 backend
integration tests, workspace-lib clippy clean.

## Known flakes

- `context.setOffline toggles network` on WebKit bun occasionally
  fails under the full suite but passes in isolation. Pre-existing
  state leak, unrelated to 1.2 / 1.3 / consolidation.

## Key source locations

| area | path |
|---|---|
| Handle types + dual backing | `crates/ferridriver/src/{js_handle,element_handle}.rs` |
| Backend primitive | `crates/ferridriver/src/backend/mod.rs::AnyPage::call_utility_evaluate`, `backend/cdp/mod.rs::UTILITY_EVAL_WRAPPER` |
| Per-backend eval + release + element-from-remote | `crates/ferridriver/src/backend/{cdp,bidi,webkit}/*` |
| Wire serializer (isomorphic) | `crates/ferridriver/src/protocol/serializers.rs` |
| Injected utility script | `crates/ferridriver/src/injected/{utilityScript,isomorphic/utilityScriptSerializers}.ts` |
| Page-level evaluate + query_selector{,_all} | `crates/ferridriver/src/page.rs` |
| Frame-level evaluate (today: expression-only) | `crates/ferridriver/src/frame.rs` |
| Locator | `crates/ferridriver/src/locator.rs` |
| NAPI bindings | `crates/ferridriver-node/src/{js_handle,element_handle,page}.rs` |
| QuickJS bindings | `crates/ferridriver-script/src/bindings/{js_handle,element_handle,page}.rs` |
| Integration test client + helpers | `crates/ferridriver-cli/tests/backends_support/client.rs` |
| Handle-surface tests | `crates/ferridriver-cli/tests/backends_support/handle_surface.rs` |
| NAPI tests | `crates/ferridriver-node/test/handles.test.ts` |
| Rules + lessons | `CLAUDE.md` |
