# Handover — next Playwright-parity session

**Read-first for any session continuing Playwright-parity work on
`ferridriver`.** Overwrite with a fresh summary at the end of each batch.

## Cross-device setup

Everything needed to resume is committed in this repo — no local
per-project memory is required. The next session should read, in order:

1. `CLAUDE.md` — 10 Playwright-parity rules, user preferences, and
   the consolidated "Lessons learned" section (phase-scaffolding
   exception, BiDi `data-fdref` quirk, rquickjs `None → undefined`,
   QuickJS lacks `setTimeout`, the utility-script `JSON.stringify`
   wrapper trick, etc.). This file is the authoritative source for
   rules; don't try to reach into any local memory.
2. `PLAYWRIGHT_COMPAT.md` — the gap tracker, §1.2 and §1.3 for
   anything still `[~]`.
3. `docs/FINISH_TASKS_1.2_1.3.md` — a self-contained handoff prompt
   to finish the remaining surface of tasks 1.2 and 1.3 in a single
   session (14 numbered items).
4. This file (HANDOVER.md) — commit-level block summary below.

Set up the cloned Playwright source at `/tmp/playwright` (needed by
every rule-6 lookup) once per machine:

```bash
git clone https://github.com/microsoft/playwright /tmp/playwright
```

Same goes for dev deps on a fresh machine: `just test` handles
Chrome / NAPI / Bun dependencies automatically.

---

## Branch state

Branch: `main`, **47 commits ahead** of `origin/main` (4 new this block),
working tree clean except for this HANDOVER + `PLAYWRIGHT_COMPAT.md`.

Recent commits this block (newest first):

```
<this commit> docs: PLAYWRIGHT_COMPAT.md flip 1.2/1.3 to [~]; HANDOVER
              for end-of-block (phases C → F shipped)
<this commit> feat(core): page.querySelectorAll + locator.elementHandle{,s}
              materialisation surface (task 1.2/1.3 phase F)
badfe7b      feat(core): ElementHandle DOM methods — reads, state,
              geometry, actions (task 1.2 phase E)
20347f6      feat(core): page.evaluate(fn, arg) end-to-end across 4
              backends (task 1.3 phase D)
2c3f03f      feat(core): JSHandle + ElementHandle skeleton +
              per-backend dispose (task 1.2/1.3 phase C)
0591807      feat(injected): expose UtilityScript + isomorphic
              serializer on window.__fd (task 1.3 phase B)
7a29ce5      refactor(core): pivot SerializedValue to isomorphic
              (utilityScript) format
b355513      feat(core): tagged-union wire serializer for evaluate(fn,
              arg) + handles (task 1.3 phase A)
```

## What's landed this block

Tasks **1.2 (ElementHandle)** and **1.3 (JSHandle)** are now far enough
along to be marked **[~]** in `PLAYWRIGHT_COMPAT.md` — the core
lifecycle, the wire-level evaluate(fn, arg) path on every backend, the
common DOM-method surface, and the materialisation primitives all
ship. The remaining surface for [x] is mechanical follow-on work
(multi-arg evaluate, ~10 more action methods, getProperties /
getProperty, full screenshot option bag).

### Phase C — Lifecycle + dispose (`2c3f03f`)
- `JSHandle { page, remote, disposed: Arc<AtomicBool> }` and
  `HandleRemote { Cdp(Arc<str>), Bidi{shared_id, handle}, WebKit(u64) }`
  with infallible `to_handle_id` / `from_handle_id`.
- `ElementHandle` composes `JSHandle` + `Arc<AnyElement>`.
- `AnyPage::release_handle` dispatches to per-backend
  `release_object` (CDP) / `release_handle` (BiDi) / `release_ref`
  (WebKit) — new `Op::ReleaseRef = 73` IPC op + `host.m` handler.
- WebKit `window.__wr` migrated from a plain object to a `Map` for
  O(1) deletion; ref ids come from a monotonic `window.__wr_next`
  counter.
- `Page::query_selector(selector) -> Option<ElementHandle>` (the
  minimum minting path needed to test dispose).
- NAPI: `JSHandle` + `ElementHandle` classes; QuickJS: `JSHandleJs`
  + `ElementHandleJs`. Page `querySelector` + `$` alias on both
  binding layers.
- 22 Rule-9 bun tests + per-backend QuickJS tests for the lifecycle.

### Phase D — page.evaluate(fn, arg) (`20347f6`)
- New `EvaluateResult { Value, Handle }` in `js_handle.rs`.
- `AnyPage::call_utility_evaluate` dispatches to each backend's
  utility-script invocation.
- Shared wrapper function `backend::cdp::UTILITY_EVAL_WRAPPER`
  memoises `UtilityScript` on `window.__fd.__us`, JSON.parses the
  serialized arg, JSON.stringifies the result so only flat strings
  cross the backend boundary.
- CDP: `Runtime.callFunctionOn` with `executionContextId` /
  `objectId`. BiDi: `script.callFunction` with `target.context` and
  `sharedReference` handles. WebKit: inlined expression via
  `Op::Evaluate` (no new IPC op needed because `window.__wr` is
  page-addressable).
- `Page::evaluate_with_arg` / `evaluate_handle_with_arg`,
  `JSHandle::evaluate_with_arg` (handle as arg[0]) / `evaluate_handle_with_arg`.
- NAPI / QuickJS: `evaluateWithArg` (lossy JSON-like) +
  `evaluateWithArgWire` (raw isomorphic wire) + `evaluateHandleWithArg`
  on Page, JSHandle, ElementHandle.
- Rule-9 tests: primitive / object / null arg round-trip,
  evaluateHandle lifecycle, handle-as-arg `tagName` probe,
  ElementHandle.evaluate delegation, disposed-handle use error,
  rich-type round-trip via `evaluateWithArgWire` (Date / RegExp /
  NaN / Infinity / BigInt / undefined).

### Phase E — ElementHandle DOM methods (`badfe7b`)
- `BoundingBox` re-exported from `options::BoundingBox`;
  `ElementState` enum for the future `wait_for_element_state`.
- Reads via utility-script evaluate: `inner_html`, `inner_text`,
  `text_content`, `get_attribute(name)`, `input_value`.
- State predicates: `is_visible`, `is_hidden`, `is_disabled`,
  `is_enabled`, `is_checked` (ARIA + input), `is_editable`.
- Geometry: `bounding_box`.
- Actions: `click`, `dblclick`, `hover`, `type_str`, `focus`,
  `scroll_into_view_if_needed`, `screenshot(format)` — first six
  delegate to the backend's `AnyElement`, `focus` runs through the
  utility script.
- NAPI + QuickJS bindings for every method (Playwright-camelCase).
- 8 new bun tests + per-backend QuickJS test
  (`test_script_element_handle_methods`).
- BiDi's `data-fdref` attribute on referenced elements is
  accommodated by substring-matching innerHTML rather than literal
  comparison.

### Phase F — Materialisation (this commit)
- `Page::query_selector_all(selector)` — uses `selectors::query_all`
  to tag matches with `data-fd-sel='<i>'`, then resolves each by
  tag and wraps in `ElementHandle`. Tags cleaned up on completion.
- `Locator::element_handle()` / `Locator::element_handles()` — both
  reuse the same `selectors::query_one` / `query_all` plumbing the
  retry loop uses.
- NAPI + QuickJS: `Page.querySelectorAll` + `$$` alias,
  `Locator.elementHandle`, `Locator.elementHandles`.
- Rule-9 test (`test_script_handle_materialisation`):
  querySelectorAll length+texts, $$ alias, empty-match returns
  empty array, locator.elementHandle returns BUTTON, locator
  elementHandles returns N matches.

## Still owed for flipping 1.2 / 1.3 to [x]

See `PLAYWRIGHT_COMPAT.md` §1.2 + §1.3 for the full lists. Highlights:

**1.2 ElementHandle:**
- Action methods with Playwright option bags: `check`, `uncheck`,
  `set_checked`, `tap`, `fill`, `press`, `dispatch_event`,
  `select_option`, `select_text`, `set_input_files`. Most lower to
  the existing `actions::*` Locator helpers — pattern: derive a
  synthetic Locator from the handle (e.g. `internal:handle-ref=<id>`
  selector engine) and dispatch through the existing pipeline.
- `wait_for_element_state(state, opts)` — poll via
  `fd.checkElementStates` like the Locator paths.
- Handle-scoped `wait_for_selector(selector, opts)`.
- Frame accessors: `owner_frame`, `content_frame`.
- `eval_on_selector` / `eval_on_selector_all` — Playwright `$eval` /
  `$$eval` shortcuts.
- Full screenshot option bag: `path`, `omitBackground`, `animations`,
  `mask`, `style`, `clip`.

**1.3 JSHandle:**
- `getProperties()` → `Map<string, JSHandle>`, `getProperty(name)` →
  `JSHandle`.
- User-facing `jsonValue()` (currently expressible via
  `evaluate_with_arg("el => el", null)` but deserves its own method
  with the right return shape).
- Multi-arg evaluate: `handle.evaluate(fn, userArg)` currently
  ignores `userArg`. Bump `argCount` to 2 and thread a second
  serialized value through the utility-script wrapper.
- Rich input arg detection at NAPI / QuickJS: spot `Date`, `RegExp`,
  `BigInt`, typed arrays, AND `JSHandle` / `ElementHandle` class
  instances; emit `{h: idx}` + push the backend `HandleId` into the
  `handles` slot. Today only JSON-expressible values are supported.
- `asElement()` actually inspects the remote (CDP
  `RemoteObject.subtype === 'node'`, BiDi `RemoteValue::Node`,
  WebKit value-type round-trip).

## Next Tier-1 task: 1.4 Request / Response lifecycle

Largest remaining Tier-1 item. Today `events.rs` carries
`NetRequest` / `NetResponse` DTOs over the event bus; Playwright
exposes them as full lifecycle objects with `body()`, `redirected_*`,
`headers_array()`, `post_data*`, etc. See `PLAYWRIGHT_COMPAT.md` §1.4.

## Ground rules (non-negotiable, unchanged)

- **Rule 4**: Every public API must work on every backend, or return
  typed `FerriError::Unsupported { reason }` where the protocol
  genuinely can't.
- **Rule 9**: Per-option integration test on every backend before
  flipping any `[x]`. Signatures alone are not parity.
- **Rule 10**: No escape hatches. No `#[allow(clippy::...)]`, no
  `eslint-disable`, no `--no-verify`. **Exception** (per the
  `feedback_keep_phase_scaffolding.md` memory): `#[allow(dead_code)]`
  on phase-N+1 scaffolding fields/methods that the next phase will
  consume, with a `consumed in phase X` justification.
- Any new `opts.timeout` field that reaches a method MUST propagate
  to the retry loop deadline.
- Never claim "complete" in a commit unless every phase for that
  task is landed, tested on all 4 backends, and ticked in the tracker.

## Tests that must stay green

- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo test --workspace` — all green (118+ core, 4 cli backends,
  657+ NAPI, 14 injected, 29 protocol).
- `cd crates/ferridriver-node && bun run build:debug && bun test` —
  724 tests at last count (+27 phase-D, +8 phase-E this block).
- `FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver
     cargo test -p ferridriver-cli --test backends -- --test-threads=1`
  — all 4 backends green.

## Known flakes

- `context.setOffline toggles network` on the WebKit bun test
  intermittently fails in the full suite, passes in isolation.
  Pre-existing state leak, orthogonal to 1.3/1.2 work.

## Lessons logged this block — don't repeat

1. **`#[allow(dead_code)]` on phase scaffolding is OK and saves
   churn.** The user explicitly called this out mid-session
   ("keep scaffolding and continue implementing don't remove
   things") after I deleted an `element` field + `any_element()`
   method that phase-E was about to consume. Saved as
   `feedback_keep_phase_scaffolding.md`. Carry forward fields
   the next phase will consume; suppress dead_code with
   `#[allow(dead_code)]` + a `consumed in phase X` justification.

2. **rquickjs maps `Option::None` to `undefined`, not `null`.** The
   first cut of `page.querySelector` tests in
   `crates/ferridriver-cli/tests/backends.rs` used `=== null` and
   failed because the returned value was `undefined`. Use loose
   `== null` (matches both) or explicit
   `(r === null || r === undefined)`.

3. **QuickJS `page.evaluate` JSON-stringifies its result.** Numbers
   come back as the string `"42"`. Either `Number(...)` inside JS
   or parse the string in Rust via `.as_str().parse::<i64>()`.

4. **BiDi injects `data-fdref="<id>"` attributes on DOM elements
   it references.** Test innerHTML matchers must use `contains("<b")
   && contains("world</b>")` rather than literal `<b>world</b>`,
   which only matches CDP / WebKit. Tracked as a separate gap in
   Section B (BiDi-specific quirks).

5. **QuickJS doesn't have `setTimeout`.** `await new Promise(r =>
   setTimeout(r, 50))` throws `setTimeout is not defined`. For
   synchronous DOM updates, just observe the next page round-trip;
   for async waits, use `page.waitForLoadState` or similar.

6. **The CDP / BiDi utility-script `JSON.stringify` wrapper trick
   keeps the wire format clean.** First cut returned the raw
   isomorphic wire object; CDP / BiDi then re-serialised it via
   their own RemoteValue format and corrupted the tags. Fix:
   `JSON.stringify` the result inside the wrapper so the backend
   only ships flat strings; Rust JSON.parses back. Same trick lets
   WebKit's "envelope" pattern carry both `{kind: 'value', payload}`
   and `{kind: 'handle', ref}`.

7. **WebKit doesn't need a new `Op::CallFunctionOn`.** Because
   every handle is reachable from page-side JS via
   `window.__wr.get(ref_id)`, we synthesise an inline expression
   and reuse the existing `Op::Evaluate`. Saves a host-side change.

## Key source locations

| area | path |
|---|---|
| **Lifecycle types** | `crates/ferridriver/src/{js_handle,element_handle}.rs` |
| Wire serializer (isomorphic) | `crates/ferridriver/src/protocol/serializers.rs` |
| Injected utility script | `crates/ferridriver/src/injected/{utilityScript,isomorphic/utilityScriptSerializers}.ts` |
| Backend dispatch + `release_handle` + `call_utility_evaluate` + `element_handle_remote` | `crates/ferridriver/src/backend/mod.rs` |
| **Shared utility-script wrapper** | `crates/ferridriver/src/backend/cdp/mod.rs::UTILITY_EVAL_WRAPPER` |
| CDP per-page `call_utility_evaluate` + `release_object` | `crates/ferridriver/src/backend/cdp/mod.rs` |
| BiDi per-page `call_utility_evaluate` + `release_handle` | `crates/ferridriver/src/backend/bidi/page.rs` |
| WebKit per-page `call_utility_evaluate` + `release_ref` + `Op::ReleaseRef` | `crates/ferridriver/src/backend/webkit/{mod,ipc,host.m}.rs` |
| Page-level `evaluate_with_arg` / `query_selector{,_all}` | `crates/ferridriver/src/page.rs` |
| Locator `element_handle{,s}` | `crates/ferridriver/src/locator.rs` |
| NAPI handle bindings | `crates/ferridriver-node/src/{js_handle,element_handle}.rs` |
| QuickJS handle bindings | `crates/ferridriver-script/src/bindings/{js_handle,element_handle}.rs` |
| Test plan | `PLAYWRIGHT_COMPAT.md` §1.2 + §1.3 |
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
```

## Cross-device state (no local-only memory)

Everything that previously lived in the user's per-project auto-memory
has been consolidated into `CLAUDE.md` under "User preferences" and
"Lessons learned". Resuming on another device does not require
restoring any local files.

- 10 Playwright-parity rules — `CLAUDE.md` "Playwright Parity Rules"
  section.
- `#[allow(dead_code)]` phase-scaffolding exception — `CLAUDE.md`
  Rule 10 + "Keep phase scaffolding" lesson.
- rquickjs / QuickJS / BiDi / WebKit quirks from this block —
  `CLAUDE.md` "Backend / wire / binding quirks" lesson.
- Commit-message style, emoji policy, git safety — `CLAUDE.md`
  "User preferences" section.
- Finish-prompt for tasks 1.2/1.3 — `docs/FINISH_TASKS_1.2_1.3.md`.
