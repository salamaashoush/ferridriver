# Prompt — finish Playwright-parity tasks 1.2 & 1.3

This file is a **self-contained handoff prompt**. Paste its entire contents
into a fresh Claude Code session (on any device) to resume work on tasks
1.2 (ElementHandle) and 1.3 (JSHandle). No local-device state is required —
everything the session needs is committed in this repo.

---

Finish Playwright-parity tasks **1.2 (ElementHandle)** and **1.3 (JSHandle)** on ferridriver. Both are currently `[~]` in `PLAYWRIGHT_COMPAT.md`. Your goal this session: land everything still owed, flip both to `[x]`, update `HANDOVER.md`.

## READ FIRST (in order)

1. `HANDOVER.md` — end-of-block summary from the last session (commits `2c3f03f` / `20347f6` / `badfe7b` / `1d6ab03`). The "Lessons logged" section is load-bearing — don't re-learn them.
2. `PLAYWRIGHT_COMPAT.md` §1.2 and §1.3 — authoritative "still owed" lists.
3. `CLAUDE.md` — the 10 Playwright-parity rules, user preferences, and the consolidated "Lessons learned" section. Rule 4 (every backend real or typed `Unsupported`), Rule 9 (per-option test on all 4 backends), Rule 10 (no escape hatches — with the single documented "scaffolding" exception). **CLAUDE.md is the authoritative source for all cross-device rules. Do not try to retrieve local memory; it's all been consolidated there.**
4. `crates/ferridriver/src/{js_handle,element_handle}.rs` — the shipped core. Read everything.
5. `crates/ferridriver/src/backend/cdp/mod.rs::UTILITY_EVAL_WRAPPER` and `::call_utility_evaluate` — the wire path every evaluate goes through. Same method also on `BidiPage` and `WebKitPage`.
6. `crates/ferridriver/src/locator.rs::retry_resolve!` macro — the auto-wait retry loop you're going to reuse for handle-scoped action methods.
7. `/tmp/playwright/packages/playwright-core/src/client/{jsHandle,elementHandle}.ts` — canonical Playwright signatures. Read every method you implement. If `/tmp/playwright` is empty on this device, `git clone https://github.com/microsoft/playwright /tmp/playwright` first.

## What's already shipped (don't redo)

- `JSHandle` + `ElementHandle` types with shared `Arc<AtomicBool>` dispose flag.
- Per-backend release (CDP `Runtime.releaseObject`, BiDi `script.disown`, WebKit `Op::ReleaseRef = 73`).
- `page.evaluate(fn, arg)` / `evaluateHandle` / `handle.evaluate(fn)` on all 4 backends via shared wrapper function. Rich-type wire round-trip (Date/RegExp/NaN/Infinity/BigInt/undefined/typed arrays).
- ElementHandle methods: `inner_html`, `inner_text`, `text_content`, `get_attribute`, `input_value`, `is_visible/hidden/disabled/enabled/checked/editable`, `bounding_box`, `click`, `dblclick`, `hover`, `type`, `focus`, `scroll_into_view_if_needed`, `screenshot(format)`.
- Materialisation: `page.querySelector` + `$`, `page.querySelectorAll` + `$$`, `locator.elementHandle`, `locator.elementHandles`.
- Existing Locator API unchanged.

## Remaining scope — land it all

### 1.3 JSHandle gaps (5 items)

1. **`json_value()`** — core method on `JSHandle` that runs `utilityScript.jsonValue(true, remote)` and returns `SerializedValue`. NAPI + QuickJS method `jsonValue()` returning the JSON-like projection. Tests: each rich type round-trips.

2. **`get_property(name)` → `JSHandle`** — core method evaluates `(h, n) => h[n]` with the handle as arg[0] and property name baked in via serde-escape. Returns new `JSHandle`. NAPI + QuickJS. Test: chain `h.getProperty('length')` on an array handle.

3. **`get_properties()` → `Vec<(String, JSHandle)>` (core) / `Map<string, JSHandle>` (NAPI)** — iterate own enumerable properties. Two-phase evaluate: first get `Object.keys(h)`, then for each key mint a handle via `evaluateHandle("(h, k) => h[k]")`. NAPI returns a plain object `{ [key]: JSHandle }`; QuickJS does the same. Test: `{a:1, b:2}` handle returns `{a, b}` with handles that round-trip to 1 and 2 via `jsonValue`.

4. **Multi-arg `handle.evaluate(fn, userArg)`** — currently drops `userArg`. Fix the core `JSHandle::evaluate_with_arg` to merge the handle ref (`{h:0}`) with the user's serialized arg into a multi-slot argument list. Utility script takes `argCount=2` and receives `[handle, userArg]`; the user function signature becomes `(el, userArg) => …`. Update `call_utility_evaluate` in all three backends to support `argCount > 1` (it's already set up for one slot — extend to N). Same thing on `ElementHandle::evaluate_with_arg` delegation. Test: `handle.evaluate((el, x) => el.tagName + x, '!')` returns `'BODY!'`.

5. **`JSHandle::as_element()` inspects remote type.** Today it returns `None`. Simpler uniform approach: `evaluate_with_arg("h => h instanceof Node", null)` returns `bool`. If true, construct `ElementHandle` from the existing `HandleRemote` by building an `AnyPage::element_from_remote(&HandleRemote) -> AnyElement` helper. Add the helper on all 3 backends:
    - CDP: `CdpElement { object_id, node_id: None }` minted from the object_id.
    - BiDi: `BidiElement::new(session, context_id, shared_id)`.
    - WebKit: `WebKitElement { client, view_id, ref_id }`.
    NAPI + QuickJS update stubs to call through. Test: `evaluateHandle(() => document.body).asElement()` is non-null; `evaluateHandle(() => 42).asElement()` is null.

### 1.2 ElementHandle gaps (action methods + reads + waits)

6. **`$eval` / `$$eval`** — Playwright's `page.$eval(selector, fn, arg)` and `$$eval(selector, fn, arg)`. Core: resolve via `query_selector` / `query_selector_all`, then run `evaluate_with_arg` with the element (or array) baked into the receiver. Cleanest: `eval_on_selector` mints a handle and calls `handle.evaluate_with_arg(fn, userArg)`, then disposes. `eval_on_selector_all` is trickier — need to pass an array of elements. Simplest: `evaluate_with_arg("sel => Array.from(document.querySelectorAll(sel)).<fn>", null)` with `sel` and `fn` inlined into the expression. NAPI + QuickJS.

7. **`owner_frame()` / `content_frame()`** — `element_handle.owner_frame()` returns the Frame the element belongs to (needs a new `AnyPage::frame_for_element(&HandleRemote) -> Option<Frame>` backend call; CDP uses `DOM.describeNode`). `element_handle.content_frame()` returns the Frame of an `<iframe>` (evaluate `el.contentDocument.defaultView.frameElement` — get the iframe's frame_id). NAPI + QuickJS.

8. **`wait_for_element_state(state, opts)`** — polls `fd.checkElementStates` (the helper `check`/`uncheck` already uses). Poll in a loop with the same `retry_resolve!`-style deadline. `state` is `ElementState` (already defined in element_handle.rs). Options bag: `{ timeout }`. NAPI + QuickJS.

9. **`wait_for_selector(selector, opts)` scoped to the element** — poll until `querySelector` called on the handle's subtree returns non-null. Use `evaluate_with_arg("el => el.querySelector(<sel>)")` with the selector baked in; when non-null, mint an `ElementHandle` from the returned handle. NAPI + QuickJS.

10. **Action methods with Playwright option bags: `fill`, `check`, `uncheck`, `set_checked`, `tap`, `press`, `dispatch_event`, `select_option`**. **Simplest implementation strategy**: use a "temp-tag" trick that reuses every existing Locator action. For each method on `ElementHandle`:
    ```rust
    // 1. Tag: element evaluate("el => el.setAttribute('data-fd-eh', '<nonce>')")
    // 2. Build a Locator: self.page().locator(&format!("[data-fd-eh='{nonce}']"), None)
    // 3. Dispatch via the existing Locator action (gets full retry_resolve + options).
    // 4. Untag: element evaluate("el => el.removeAttribute('data-fd-eh')") in a finally
    //    path (ensure it runs on error too).
    ```
    Helper: `ElementHandle::as_locator() -> (Locator, DropGuard)` where the guard untags on drop. This way each action method is ~3 lines: tag, dispatch, untag. All 8 methods get their full option bag for free. NAPI + QuickJS each get their wrappers.
    Alternative (cleaner but more work): add a `internal:handle-ref=<id>` selector engine entry. Temp-tag is simpler and works today.

11. **`select_text()`** — JS-only: `el => { if (el.isContentEditable) { const r = document.createRange(); r.selectNodeContents(el); ...} else if (el.select) el.select(); }`. NAPI + QuickJS.

12. **`set_input_files(files)`** — polymorphic: `string | string[] | FilePayload | FilePayload[]`. Tag the element, delegate to existing `AnyPage::set_file_input(selector, paths)`. FilePayload writes bytes to a temp file first, uploads, deletes after. Look at existing Locator `set_input_files` path for the pattern. NAPI + QuickJS.

13. **Full `ElementHandle::screenshot(opts)` option bag** — currently only accepts `format`. Extend to the full `ScreenshotOpts` struct from `crates/ferridriver/src/backend/mod.rs` (`path`, `omit_background`, `animations`, `caret`, `mask`, `mask_color`, `style`, `quality`, `scale`, `clip`). Wire through — `AnyPage::screenshot_element` already accepts a `selector` + `format`; extend the backend path to accept `ScreenshotOpts` and bound it to the element's bounding box as the default `clip` when `full_page: false`. NAPI + QuickJS accept the option object.

### 1.3 arg-walker (close out)

14. **NAPI/QuickJS arg walker detects `JSHandle` / `ElementHandle` class instances.** When user passes one to `page.evaluate(fn, handleArg)` or `handle.evaluate(fn, otherHandle)`:
    - NAPI: use `ClassInstance<JSHandle>::from_unknown(arg)` / `ClassInstance<ElementHandle>::from_unknown(arg)` to detect. Extract `HandleRemote` via the inner ref.
    - QuickJS: `rquickjs::Class::<JSHandleJs>::from_value(&v)` / same for `ElementHandleJs`. Extract `inner.remote().clone()`.
    - Build `SerializedArgument { value: SerializedValue::handle(0), handles: vec![handle_id] }` when the top-level is a handle. For nested handles (inside object/array), walk the structure similarly and emit `{h: idx}` refs at the leaves. For phase-MVP just handle top-level handle args; nested is a nice-to-have.
    Test (all 4 backends): `const h = await page.evaluateHandle(() => document.body); await page.evaluate((el, suffix) => el.tagName + suffix, h, '!')` returns `'BODY!'`. Also rich types (Date, RegExp, BigInt) should walk through — extend the JSON walker to detect them via `value.constructor.name` or `instanceof` checks and emit the tagged wire shape directly.

## Tests — Rule 9 compliance

Every new method needs a Rule-9 integration test on **all 4 backends** (`cdp-pipe`, `cdp-raw`, `webkit`, `bidi`) via `crates/ferridriver-cli/tests/backends.rs` (QuickJS path). Add or extend `test_script_handle_*` functions and register them in `run_all_tests`. Mirror in `crates/ferridriver-node/test/handles.test.ts` for the NAPI path on 3 backends (`cdp-pipe`, `cdp-raw`, `webkit`).

## Gate (must pass before commit)

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd crates/ferridriver-node && bun run build:debug && bun test
cargo build -p ferridriver-cli
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1
```

## Commit cadence

One commit per "bundle" (roughly the groups above: 1.3 simple extras / multi-arg / asElement / 1.2 reads+waits / 1.2 actions / 1.2 select+files+screenshot / arg-walker). Each commit must pass the full gate. No `#[allow(clippy::…)]` shortcuts except `dead_code` on scaffolding that commit actually consumes.

## Close-out

After every item above lands with Rule-9 tests on 4 backends:

- Flip `PLAYWRIGHT_COMPAT.md` §1.2 `[~]` → `[x]` and §1.3 `[~]` → `[x]`.
- Rewrite `HANDOVER.md` with the next Tier-1 gap (`1.4 Request/Response/WebSocket` per the tracker).
- Final commit `docs: flip 1.2/1.3 to [x]; pivot HANDOVER to task 1.4`.

## Ground rules (non-negotiable, from CLAUDE.md)

- Every backend real, no stubs. Typed `FerriError::Unsupported { reason }` only where the protocol genuinely can't (document in commit message).
- No `#[allow(clippy::*)]` in non-test code without explicit `reason = ""` justification; never on `dead_code` except for the "keep phase scaffolding" exception documented in CLAUDE.md.
- No `--no-verify` on commits.
- NAPI AND QuickJS updated in the SAME commit as the core change.
- When adding an `opts.timeout` field, propagate to the retry loop deadline.
- Read `/tmp/playwright/...` before implementing every method; never guess the signature.
- **No emojis** in code, docstrings, or markdown. No AI attribution in commit messages.
