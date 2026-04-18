# Handover — next Playwright-parity session

**Read-first for any session continuing Playwright-parity work on
`ferridriver`.** Overwrite with a fresh summary at the end of each batch.

## Cross-device setup

Everything needed to resume is committed in this repo — no local
per-project memory is required. The next session should read, in order:

1. `CLAUDE.md` — 10 Playwright-parity rules, user preferences, the
   consolidated "Lessons learned" section. Authoritative source for
   cross-device rules.
2. `PLAYWRIGHT_COMPAT.md` — gap tracker, §1.2 and §1.3 for anything
   still `[~]`, §1.4 as the next Tier-1 item.
3. This file (HANDOVER.md) — commit-level summary below.

Set up the cloned Playwright source at `/tmp/playwright` (needed by
every rule-6 lookup) once per machine:

```bash
git clone https://github.com/microsoft/playwright /tmp/playwright
```

`just test` handles Chrome / NAPI / Bun dependencies automatically on
fresh machines.

---

## Branch state

Branch: `main`, **50 commits ahead** of `origin/main` (3 new this block),
working tree clean.

Recent commits this block (newest first):

```
828a2bc feat(node): NAPI bindings for new JSHandle + ElementHandle surface
769ff78 feat(core): ElementHandle $eval, frame accessors, wait helpers, action bridge
f3d3cdd feat(core): Playwright-shape evaluate primitive + JSHandle value/property/asElement
```

## What's landed this block

Tasks **1.2 (ElementHandle)** and **1.3 (JSHandle)** are now fully
implemented on the Rust core + NAPI binding layers. QuickJS bindings,
the NAPI/QuickJS rich-arg walker, and Rule-9 4-backend tests for the
new surface are the remaining gates before flipping `[x]` in
`PLAYWRIGHT_COMPAT.md` §1.2 / §1.3.

### Commit `f3d3cdd` — Playwright-shape evaluate primitive + JSHandle value/property/asElement

**Backend primitive.** Collapsed `call_utility_evaluate` to a single
Playwright-matching signature across CDP, BiDi, WebKit, and the
`AnyPage` dispatcher:

```rust
pub async fn call_utility_evaluate(
  &self,
  fn_source: &str,
  args: &[SerializedValue],
  handles: &[HandleId],
  frame_id: Option<&str>,
  is_function: Option<bool>,
  return_by_value: bool,
) -> Result<EvaluateResult>
```

The previous design carried a `receiver: Option<&HandleRemote>` parameter
on the theory that `handle.evaluate(fn, arg)` bound `this` to the handle.
Reading Playwright's `javascript.ts:161-163` proved the opposite — their
`JSHandle.evaluate(fn, arg)` literally calls `evaluate(ctx, true, fn, this, arg)`,
so the handle is the first variadic positional arg, not a `this` binding.
Dropped the receiver.

`UTILITY_EVAL_WRAPPER` now takes a JSON array of N serialized args so
`handle.evaluate(fn, arg)` can pack `[handle, userArg]` as two positional
parameters matching Playwright's `(handleValue, userArg) => ...` user
function shape. Helper `shift_handle_indices` relocates `{h:i}` refs
inside a user-arg sub-tree to `{h:i+1}` when merging with the receiver
handle at index 0.

**JSHandle additions.**

- `jsonValue()` — evaluates `h => h` through the utility script, picking
  up the full isomorphic serializer round-trip for rich types (Date,
  RegExp, NaN, Infinity, BigInt, typed arrays, undefined).
- `getProperty(name)` — evaluates `h => h[name]` with name JSON-escaped.
- `getProperties()` — two-phase: enumerate own keys via `Object.keys`,
  then mint a handle per key; returns `Vec<(String, JSHandle)>`.
- `asElement()` — probes `h instanceof Node`; on true, re-wraps the
  remote into an `ElementHandle` via a new `backend::element_from_remote`
  helper (CDP: `element_from_object_id`, BiDi: `element_from_shared_id`,
  WebKit: `element_from_ref_id`). The new ElementHandle shares the
  original JSHandle's dispose flag + page.

### Commit `769ff78` — ElementHandle $eval, frame accessors, wait helpers, action bridge

- `eval_on_selector` / `eval_on_selector_all` ($eval / $$eval). Probe-
  and-delegate: mint an intermediate handle via evaluateHandle through
  the element's subtree (throwing when $eval misses), then run the user
  fn_source via `JSHandle::evaluate_with_arg`. Empty-match errors for
  $eval; empty-array for $$eval.
- `owner_frame` / `content_frame`. `owner_frame` returns the page's
  main frame when the element is connected; multi-frame attribution
  via `DOM.describeNode` is owed. `content_frame` probes the element's
  IFRAME/FRAME tag + attributes and scans the frame cache for a child
  match on name/id/src (covers the common named-iframe case).
- `wait_for_element_state` with Playwright's `[0, 0, 20, 50, 100, 100, 500]`
  backoff, honouring the page default timeout when `opts.timeout` is
  None. Returns `Timeout` on deadline.
- `wait_for_selector` — polls `el.querySelector(sel)` via evaluateHandle
  and promotes the first non-null match via `JSHandle::as_element`.
- Temp-tag bridge. `data-fd-eh='<nonce>'` attribute + page-scoped
  Locator built from the nonce selector lets every action (`fill`,
  `check`, `uncheck`, `set_checked`, `tap`, `press`, `dispatch_event`,
  `select_option`, `set_input_files`) delegate through the existing
  Locator retry + actionability pipeline with full option-bag parity.
  Best-effort untag in a finally-style cleanup.
- `select_text` — JS-only path covering input/textarea/select plus
  contentEditable ranges.
- `screenshot_with_opts(ScreenshotOpts)` accepts the full bag; backend
  path currently honours `format` only — additional fields carried at
  the handle layer until the shared locator-level screenshot gets the
  full surface.

### Commit `828a2bc` — NAPI bindings for new JSHandle + ElementHandle surface

- `JSHandle.asElement` async now, returns `Promise<ElementHandle | null>`.
- `JSHandle.jsonValue` / `jsonValueWire`.
- `JSHandle.getProperty` (name → handle) / `getProperties` (returns
  `Record<string, JSHandle>`).
- `ElementHandle.evalOnSelector` / `evalOnSelectorAll`.
- `ElementHandle.ownerFrame` / `contentFrame`.
- `ElementHandle.waitForElementState` (state, timeout?).
- `ElementHandle.waitForSelector` (selector, timeout?) returning
  `Promise<ElementHandle | null>`.
- `ElementHandle.fill` / `check` / `uncheck` / `setChecked` / `tap` /
  `press` / `dispatchEvent` / `selectOption` via the core temp-tag
  bridge, reusing the existing NAPI option types (`FillOptions`,
  `CheckOptions`, `TapOptions`, `PressOptions`, `DispatchEventOptions`,
  `SelectOptionOptions`).
- `ElementHandle.selectText`, `setInputFiles`.
- 742 Bun tests pass.

## Still owed for flipping 1.2 / 1.3 to [x]

### QuickJS bindings (`crates/ferridriver-script/src/bindings/`)

- `js_handle.rs`: `jsonValue`, `getProperty`, `getProperties`, updated
  async `asElement`.
- `element_handle.rs`: the entire phase-G surface (`$eval`, `$$eval`,
  `ownerFrame`, `contentFrame`, `waitForElementState`,
  `waitForSelector`, `fill`, `check`, `uncheck`, `setChecked`, `tap`,
  `press`, `dispatchEvent`, `selectOption`, `selectText`,
  `setInputFiles`, `screenshotWithOpts`).

### Rich-arg walker (both binding layers)

NAPI + QuickJS should detect `JSHandle` / `ElementHandle` class
instances at the user-arg boundary and emit `{h: idx}` + push the
backend `HandleId` automatically. Today the binding's
`build_serialized_argument` only handles JSON-expressible values —
passing a handle as an `arg` requires manually constructing the
`SerializedArgument`.

- NAPI: `ClassInstance<JSHandle>::from_unknown(arg)` /
  `ClassInstance<ElementHandle>::from_unknown(arg)` detect; extract
  `HandleRemote` via `inner_ref().remote().clone()`.
- QuickJS: `rquickjs::Class::<JSHandleJs>::from_value(&v)` / same for
  `ElementHandleJs`.
- Nested-inside-object detection is nice-to-have; top-level is enough
  for the phase-MVP.

### Rule-9 integration tests

Every shipped method needs a Rule-9 per-backend test on
`cdp-pipe` / `cdp-raw` / `bidi` / `webkit`:

- `crates/ferridriver-cli/tests/backends.rs` — extend
  `test_script_element_handle_methods` and
  `test_script_handle_lifecycle` with per-method assertions for the
  phase-G surface. Runs through QuickJS, so the QuickJS bindings
  must land first.
- `crates/ferridriver-node/test/handles.test.ts` — add NAPI tests for
  the phase-G surface on the 3 NAPI-supported backends (`cdp-pipe`,
  `cdp-raw`, `webkit`).

### Screenshot full-opts plumbing

`ElementHandle::screenshot_with_opts` accepts `ScreenshotOpts` but the
backend `AnyPage::screenshot_element` only takes `ImageFormat` today.
Threading `omitBackground`, `animations`, `mask`, `mask_color`,
`style`, `clip`, `scale`, `quality`, `caret`, `path` through each
backend's screenshot primitive is a separate locator-level gap.

## Planned follow-up: evaluate API consolidation

The current user-facing evaluate surface has three generations of
methods coexisting (`Page::evaluate(expr)` legacy,
`Page::evaluate_str(expr)` legacy, `Page::evaluate_with_arg(fn, arg)`
new; similarly on Frame + Locator). The architectural work to collapse
them onto Playwright's exact surface is planned as a standalone commit:

### Target API

- **Page**: `evaluate(fn, arg)` + `evaluate_handle(fn, arg)` — 2 methods
- **Frame**: `evaluate(fn, arg)` + `evaluate_handle(fn, arg)` — 2 methods
  (canonical impl lives here; Page delegates to main frame)
- **Locator**: `evaluate(fn, arg)` + `evaluate_all(fn, arg)` +
  `evaluate_handle(fn, arg)` — 3 methods. `evaluate` resolves + delegates
  to `ElementHandle.evaluate`; `evaluate_all` does `$$eval` on the frame.
- **JSHandle/ElementHandle**: `evaluate(fn, arg)` +
  `evaluate_handle(fn, arg)` — 2 methods

### Migration scope

- Rename `evaluate_with_arg` → `evaluate`, `evaluate_handle_with_arg`
  → `evaluate_handle` everywhere on Page + JSHandle + ElementHandle.
- Delete legacy `Page::evaluate(expr)`, `evaluate_str(expr)`,
  `Frame::evaluate(expr)`, `evaluate_str(expr)`,
  `Locator::evaluate(expr)`, `evaluate_all(expr)`.
- Build `Frame::evaluate(fn, arg)` / `evaluate_handle(fn, arg)` as
  the canonical Playwright primitive; Page delegates to main frame
  via `self.main_frame().evaluate(fn, arg)`.
- Build `Locator::evaluate(fn, arg)` / `evaluate_handle(fn, arg)`
  via the retry-resolve + delegate-to-ElementHandle pattern
  Playwright uses (`locator.ts:129/137`). `Locator::evaluate_all`
  delegates to `Frame::$$eval`.
- Migrate ~74 legacy call sites across `crates/ferridriver-node`,
  `crates/ferridriver-script`, `crates/ferridriver-mcp`,
  `crates/ferridriver-test`, `crates/ferridriver-bdd`, and
  `crates/ferridriver/tests` using
  `.evaluate(expr, None).await?.to_json_like()` for the legacy
  return-shape compat.

This is planned as a single standalone commit with its own scope
rather than interleaved with the 1.2 / 1.3 finish-line work.

## Next Tier-1 task: 1.4 Request / Response lifecycle

Largest remaining Tier-1 item. Today `events.rs` carries
`NetRequest` / `NetResponse` DTOs over the event bus; Playwright
exposes them as full lifecycle objects with `body()`,
`redirected_*`, `headers_array()`, `post_data*`, etc. See
`PLAYWRIGHT_COMPAT.md` §1.4.

## Ground rules (non-negotiable, unchanged)

- **Rule 4**: Every public API must work on every backend, or return
  typed `FerriError::Unsupported { reason }` where the protocol
  genuinely can't.
- **Rule 9**: Per-option integration test on every backend before
  flipping any `[x]`. Signatures alone are not parity.
- **Rule 10**: No escape hatches. `#[allow(dead_code)]` phase-N+1
  scaffolding is OK per the `feedback_keep_phase_scaffolding`
  exception.
- Any new `opts.timeout` field that reaches a method MUST propagate
  to the retry loop deadline.
- Never claim "complete" in a commit unless every phase for that
  task is landed, tested on all 4 backends, and ticked in the tracker.

## Tests that must stay green

- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo test --workspace` — all green (119+ core, 4 cli backends).
- `cd crates/ferridriver-node && bun run build:debug && bun test` —
  742 tests at last count (+18 phase-G this block).
- `FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver
     cargo test -p ferridriver-cli --test backends -- --test-threads=1`
  — all 4 backends green.

## Known flakes

- `context.setOffline toggles network` on the WebKit bun test
  intermittently fails in the full suite, passes in isolation.
  Pre-existing state leak, orthogonal to 1.2 / 1.3 work.

## Lessons logged this block — don't repeat

1. **Playwright's `handle.evaluate(fn, arg)` is NOT a `this` binding.**
   `JSHandle.evaluate(fn, arg)` at `javascript.ts:161` calls
   `evaluate(ctx, true, fn, this, arg)` — `this` is the first
   positional arg to the user function, not a `Runtime.callFunctionOn`
   receiver. Our initial design carried a `receiver: Option<&HandleRemote>`
   that was always `None`; removing it simplified the backend primitive
   to match Playwright exactly. Read the Playwright source before
   designing the API shape.

2. **The utility-script wrapper accepts a JSON array of N args, not a
   single slot.** When extending from single-arg page.evaluate to
   multi-arg handle.evaluate, the `serializedArg` string becomes
   `serializedArgs` — a JSON-stringified array the wrapper
   `JSON.parse`s once. `count` still mirrors Playwright's
   `argCount` into the utility script.

3. **User args carrying `{h: i}` refs need index relocation when
   merged with a receiver handle at position 0.**
   `shift_handle_indices` walks the `SerializedValue` tree and bumps
   every `Handle(i)` by `offset`. Without this, merging a user arg's
   private handle table with the receiver table produces wrong wire
   references.

## Key source locations

| area | path |
|---|---|
| **Handle types** | `crates/ferridriver/src/{js_handle,element_handle}.rs` |
| **Backend primitive** | `crates/ferridriver/src/backend/mod.rs::AnyPage::call_utility_evaluate`, `backend/cdp/mod.rs::UTILITY_EVAL_WRAPPER` |
| Wire serializer (isomorphic) | `crates/ferridriver/src/protocol/serializers.rs` |
| Injected utility script | `crates/ferridriver/src/injected/{utilityScript,isomorphic/utilityScriptSerializers}.ts` |
| Per-backend eval + release + element-from-remote | `crates/ferridriver/src/backend/{cdp,bidi,webkit}/*` |
| Page-level evaluate + query_selector{,_all} | `crates/ferridriver/src/page.rs` |
| Locator materialisation | `crates/ferridriver/src/locator.rs` |
| NAPI handle bindings | `crates/ferridriver-node/src/{js_handle,element_handle}.rs` |
| QuickJS handle bindings (owes phase-G surface) | `crates/ferridriver-script/src/bindings/{js_handle,element_handle}.rs` |
| Rules + lessons | `CLAUDE.md` (Playwright Parity Rules section) |

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
```
