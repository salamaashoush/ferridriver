# Handover — next Playwright-parity session

**Read-first for any session continuing Playwright-parity work on
`ferridriver`.** Overwrite with a fresh summary at the end of each batch.

---

## Branch state

Branch: `main`, **40 commits ahead** of `origin/main`, working tree clean.

Recent commits (newest first):

```
0591807 feat(injected): expose UtilityScript + isomorphic serializer on window.__fd (task 1.3 phase B)
7a29ce5 refactor(core): pivot SerializedValue to isomorphic (utilityScript) format
b355513 feat(core): tagged-union wire serializer for evaluate(fn, arg) + handles (task 1.3 phase A)
682fb53 docs: 1.5 remediation progress — 7 of 11 sub-items proven, 4 pending Phase 5 tests
cb0e8b9 fix(core): dispatchEvent timeout + selectOption force/timeout (task 1.5 phase 4c + 4d)
6ffe86b fix(core): check/uncheck verify final state + reject radio uncheck (task 1.5 phase 4b)
ea3da35 fix(core): fill.force actually bypasses [visible,enabled,editable] (task 1.5 phase 4a)
170bc3d feat(core): CDP native tap via Input.dispatchTouchEvent (task 1.5 phase 3, Rule 4)
e2bdc85 fix(core): honor opts.timeout on every Locator action (task 1.5 phase 2)
b77b8c7 fix(core): drop steps from Hover/Tap options (task 1.5 phase 1)
```

## What's landed this block

Two independent work streams completed:

### 1.5 remediation — done (save for phase-5 tests)

Phases 1 → 4d fixed the real semantic gaps the prior session's false
completion claim missed. Details in commit messages; summary:

- `steps` spec divergence dropped from Hover/Tap.
- `opts.timeout` threaded through every action's `retry_resolve!` deadline.
- CDP native tap via `Input.dispatchTouchEvent`; BiDi/WebKit return typed
  `Unsupported` per Rule 4.
- `fill.force` bypasses `['visible','enabled','editable']`.
- `check/uncheck/setChecked` verify post-click state via `fd.getChecked`
  (ARIA-aware), reject uncheck-of-checked-radio with Playwright's exact
  message.
- `dispatchEvent` honors `opts.timeout` via retry (Playwright's own
  dispatchEvent does not run actionability — we match).
- `selectOption` honors `opts.timeout` + `opts.force`.

Still owed for flipping the top-level 1.5 `[x]`: per-option integration
tests on **dblclick, press, type, setInputFiles**. Each is a distinct
backends.rs + NAPI test session, no code changes. The tracker's
"Partial" bucket lists probe suggestions.

### 1.3 JSHandle + 1.2 ElementHandle — phases A + B landed

Shared plumbing for `page.evaluate(fn, arg)` + JSHandle marshaling:

- **Phase A + pivot (b355513 → 7a29ce5)**: new `ferridriver::protocol`
  module. The first cut implemented Playwright's *channels* wire format
  (struct-of-optionals, `{n:42}` / `{b:true}` / `{s:"hi"}` wrapping of
  primitives) — that's the one Playwright uses only on its
  client↔server WebSocket RPC. For ferridriver there is no such RPC,
  so we pivoted in 7a29ce5 to the *isomorphic* format
  (`/tmp/playwright/packages/injected/src/utilityScriptSerializers.ts`)
  where primitives pass through raw and only rich types ride inside
  single-key tagged objects (`{v: ...}`, `{d: ...}`, `{h: ...}`, etc.).
  That's the format the page's utility script parses over CDP
  `Runtime.callFunctionOn.arguments[].value`. Every variant Playwright
  supports has a builder + round-trip test: primitives, all 6
  specials, Date, URL, BigInt, Error, RegExp, TypedArray (11 kinds),
  ArrayBuffer, Array, Object, Handle, Reference. Custom `Serialize`
  / `Deserialize` impls keep the exact Playwright byte-for-byte wire
  shape. `SerializationContext` / `SerializedArgument` /
  `HandleId { Cdp, Bidi, WebKit }` companions are in place.

- **Phase B (0591807)**: exposed the already-imported `UtilityScript`
  class + `parseEvaluationResultValue` + `serializeAsCallArgument` on
  `window.__fd`. Added a `newUtilityScript()` factory — that's the
  anchor the Rust side calls via `Runtime.evaluate` to mint a
  per-execution-context JSHandle for the receiver of every subsequent
  `Runtime.callFunctionOn`. Rebuilt the injected bundle.
  `backends.rs::test_script_utility_script_exposed` proves the class
  and every rich-type deserialization works end-to-end on all 4
  backends (deserialize `{v: 'NaN'}` → `NaN`, `{d: ...}` → `Date`,
  etc.; round-trip `{d: ...}` → `Date` → re-serialized wire shape).

## What's still owed for 1.3 + 1.2

### Phase C — JSHandle + ElementHandle skeleton

Brand-new public types. Rough scope:

- `crates/ferridriver/src/js_handle.rs` — `JSHandle` struct wrapping a
  backend-agnostic remote reference. Fields:
  - `context: Arc<Page>` / frame reference so the handle knows which
    execution context it lives in.
  - `remote: HandleId` — reuse the `protocol::HandleId` enum
    (`Cdp(String)` / `Bidi{shared_id, handle}` / `WebKit(u64)`).
  - `disposed: AtomicBool` for idempotent dispose + stale-use error.
- `crates/ferridriver/src/element_handle.rs` — `ElementHandle` wrapping
  a `JSHandle` (composition, not inheritance — Rust analog of
  Playwright's `ElementHandle extends JSHandle`).
- **Dispose on every backend** (Rule 4):
  - CDP: new `AnyPage::release_object(remote_object_id: &str)` →
    `Runtime.releaseObject { objectId }`. The existing
    `CdpElement::object_id()` caches the objectId but never releases;
    fix that too by calling release_object from the handle's dispose.
  - BiDi: `script.disown { handles: [sharedId], target: {context: ...} }`.
  - WebKit: **new IPC op**. Today `window.__wr[ref_id]` lives forever
    on the page. Add an `Op::ReleaseRef(view_id, ref_id)` in
    `backend/webkit/host.m` + `ipc.rs` that executes
    `window.__wr && window.__wr.delete({ref_id})` (or whatever shape
    `__wr` uses — it may be an array, not a Map). If it's an array,
    convert to a Map first so deletion is O(1).
- NAPI: `ferridriver_node::JSHandle` + `ElementHandle` classes with
  `dispose()` method and `[Symbol.asyncDispose]` for
  `await using h = ...`.
- QuickJS: parallel bindings.
- **No `evaluate(fn, arg)` in phase C** — just the lifecycle + `jsonValue()`
  (which calls `UtilityScript.jsonValue` via an existing CDP
  evaluate, no arg-serialization needed).

Tests (Rule 9, all 4 backends):

- Handle survives an unrelated iframe navigation (still valid, still
  resolves via its objectId).
- `dispose()` actually releases the CDP reference — follow-up
  `Runtime.callFunctionOn` with the stale objectId returns a protocol
  error matching `No object with id ...`.
- Double-dispose is idempotent.
- Using a disposed handle raises `FerriError::Other("JSHandle is
  disposed")` or a dedicated `FerriError::HandleDisposed` variant
  (decide at the time — Playwright uses `JavaScriptErrorInEvaluate`).
- BiDi's `script.disown` and WebKit's new `OP_RELEASE_REF` actually
  release on their protocols.

### Phase D — `page.evaluate(fn, arg)` end-to-end

Once JSHandle lifecycles work:

- Wire the Rust side of `evaluate(fn, arg)`:
  - At the NAPI boundary, walk the incoming `napi::JsUnknown` arg tree
    to produce a `protocol::SerializedArgument { value, handles }`.
    Use the existing `SerializedValue` enum; for every JSHandle
    encountered, push its `HandleId` into `handles` and emit
    `SerializedValue::Handle(index)`.
  - Same walker for QuickJS over `rquickjs::Value`.
  - In Rust core, hand the `SerializedArgument` to a new
    `Page::evaluate_with_args(expression, arg: SerializedArgument) ->
    Result<SerializedValue>` that:
    1. Mints (or reuses, via a per-context cache) a utility-script
       handle via `Runtime.evaluate("window.__fd.newUtilityScript()")`.
    2. Builds `Runtime.callFunctionOn` with `objectId =
       utilityScriptHandle._objectId`, `functionDeclaration =
       "(utilityScript, ...args) => utilityScript.evaluate(...args)"`,
       `arguments = [{objectId}, ...values.map(v => ({value: v})),
       ...handles.map(h => ({objectId: h}))]`.
    3. Parses the returned `RemoteObject` — if `returnByValue`, parse
       `.value` via `parseEvaluationResultValue`-equivalent on the
       Rust side; if handle, wrap in a new `JSHandle`.
- BiDi equivalent via `script.callFunction` with
  `arguments: [{type: 'sharedReference', sharedId}]` for handles +
  serialized primitives.
- WebKit: new IPC op `Op::CallFunctionOn(receiver_ref_id, function,
  args, handles) → result`.
- `return returnByValue ? json : new JSHandle(...)` in both NAPI and
  QuickJS bindings.

Tests (Rule 9):

- `page.evaluate(x => x + 1, 41)` returns `42`.
- `page.evaluate(x => x.map(v => v * 2), [1, 2, 3])` returns
  `[2, 4, 6]`.
- Every rich type round-trips: Date, RegExp, Map, Set, typed arrays,
  BigInt, NaN, ±Inf, undefined.
- Handle arg: `const h = await page.evaluateHandle(() => document.body);
  await page.evaluate(el => el.tagName, h)` returns `'BODY'`.
- Disposed-handle arg raises the exact Playwright error.
- All 4 backends.

### Phase E — ElementHandle action methods (~25)

Mostly mechanical — most actions already exist as `AnyElement` methods
via Locator's resolve path. ElementHandle needs:

- Delegating methods for `click`, `dblclick`, `hover`, `tap`, `fill`,
  `press`, `type`, `check`, `uncheck`, `setChecked`, `focus`,
  `dispatchEvent`, `scrollIntoViewIfNeeded`, `selectOption`,
  `setInputFiles`, `selectText` — all route through the existing
  `actions::*` helpers that Locator uses. The option bags are the
  same (reuse `ClickOptions`, `FillOptions`, etc.).
- State predicates routing through `objectId`, not a selector:
  `isChecked`, `isDisabled`, `isEditable`, `isEnabled`, `isHidden`,
  `isVisible`. Each evaluates a small JS on the element.
- Content reads: `innerHTML`, `innerText`, `textContent`,
  `getAttribute(name)`, `inputValue`.
- Frame access: `ownerFrame()`, `contentFrame()`. Depends on how the
  backend resolves an element's owning Frame — need to add a
  `element.owner_frame_id()` to each backend if it's not already there.
- `boundingBox()` (return a `{x, y, width, height}`).
- `screenshot(opts)` — element-scoped version of `page.screenshot`.
  Wire through existing `actions::screenshot_element`.
- `waitForElementState(state, opts)` — `'visible' | 'hidden' |
  'stable' | 'enabled' | 'disabled' | 'editable'`. Reuse
  `fd.checkElementStates` polling.
- `waitForSelector(selector, opts)` — scoped to this element.
- NAPI + QuickJS bindings for all of the above.

Tests (Rule 9): each method on all 4 backends. Because the action
path is shared with Locator, many of these can cross-reference
existing Locator tests — but we still need to prove the handle path
works (different resolution, no re-query).

### Phase F — `query_selector` + `element_handle()` materialization

The materialization surface:

- `page.query_selector(selector) -> Result<Option<ElementHandle>>`
  — the first match or None.
- `page.query_selector_all(selector) -> Result<Vec<ElementHandle>>`.
- `locator.element_handle(opts) -> Result<ElementHandle>` — resolves
  and returns a handle to whatever the locator currently points at.
- `locator.element_handles() -> Result<Vec<ElementHandle>>`.
- Frame-level equivalents.

Tests:

- Handle → interact via ElementHandle → still valid.
- Handle → navigate another frame → original still valid (CDP
  `Runtime.releaseObject` only releases on explicit dispose or
  context destruction).
- Cross-context handle error: resolve a handle in frame A, try to
  use it from an evaluate scoped to frame B, assert the Playwright-
  exact error (`JSHandles can be evaluated only in the context they
  were created`).

## Ground rules (non-negotiable, unchanged)

- **Rule 4**: Every public API must work on every backend, or return
  typed `FerriError::Unsupported { reason }` where the protocol
  genuinely can't. Dispose of a CDP handle via `Runtime.releaseObject`,
  BiDi via `script.disown`, WebKit via the new IPC op — all three.
- **Rule 9**: Per-option integration test on every backend before
  flipping any `[x]`. Signatures alone are not parity.
- **Rule 10**: No escape hatches. No `#[allow(clippy::...)]`, no
  `eslint-disable`, no `--no-verify`.
- Any new `opts.timeout` field that reaches a method MUST propagate
  to the retry loop deadline.
- Never claim "complete" in a commit unless every phase for that
  task is landed, tested on all 4 backends, and ticked in the tracker.

## Tests that must stay green

- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo test --workspace` — all green.
- `cd crates/ferridriver-node && bun run build:debug && bun test` —
  all green (651+ at last count).
- `FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver
     cargo test -p ferridriver-cli --test backends -- --test-threads=1`
  — all 4 backends green.

## Known flake (pre-existing)

- `context.setOffline toggles network` on the WebKit bun test
  intermittently fails in the full suite, passes in isolation.
  Pre-existing state leak, orthogonal to 1.3/1.2 work.

## Workflow for the remaining phases

1. Pick the next phase from the "What's still owed" list above.
2. Read `PLAYWRIGHT_COMPAT.md` + `/tmp/playwright/...` for the
   canonical signatures.
3. Implement in Rust core — types, methods, unit tests.
4. Update **all four backends** in the same commit. Typed Unsupported
   where the protocol genuinely can't do it.
5. Update NAPI (rebuild + diff `index.d.ts` against Playwright's
   `types.d.ts`) and QuickJS bindings.
6. Clippy, workspace tests, NAPI tests, all 4 backends green.
7. Integration test proving the new surface works page-side on every
   backend. No "accepts X but ignores it."
8. `cargo fmt --all`.
9. Tick `PLAYWRIGHT_COMPAT.md` only for sub-items whose integration
   tests are green on all 4 backends.
10. Commit message describes what landed AND what's still missing.

## Lessons logged this session — don't repeat

1. **Check which Playwright serializer you're mirroring.** First cut
   of Phase A modeled the *channels* format (client↔server RPC,
   struct-of-optionals). We have no such RPC, so it was wrong code.
   Corrected in 7a29ce5 to the *isomorphic* format the injected
   utility script actually parses. Before starting any new wire
   protocol work, verify WHICH Playwright endpoint emits the bytes
   you'll be reading.
2. **`page.evaluate` in QuickJS already JSON-stringifies its result.**
   So probe scripts should return a native value and then do ONE
   `JSON.parse` in the outer script — not `JSON.stringify(x)` inside
   and then `JSON.parse` outside (that double-stringifies and returns
   a quoted string).
3. **innerHTML-based setContent doesn't execute `<script>` tags** in
   all browsers consistently. Use `goto("data:text/html,...")` when
   the test needs the HTML parser to run a `<script>`.
4. **CDP `Input.dispatchTouchEvent` needs `Emulation.setTouchEmulationEnabled`
   first** or DOM touch listeners never fire. Not obvious from the
   protocol page; only surfaced via test failure under
   full-suite + isolated contrast.

## Key source locations

| area | path |
|---|---|
| Option structs | `crates/ferridriver/src/options.rs` |
| Shared actions helpers (click/hover/tap/fill/select) | `crates/ferridriver/src/actions.rs` |
| Page (facade over mainFrame) | `crates/ferridriver/src/page.rs` |
| Frame (resolution primitive) | `crates/ferridriver/src/frame.rs` |
| Locator + `retry_resolve!` macro | `crates/ferridriver/src/locator.rs` |
| **Wire serializer (isomorphic)** | `crates/ferridriver/src/protocol/serializers.rs` |
| Backend wire structs + dispatch + `AnyPage::kind()` | `crates/ferridriver/src/backend/mod.rs` |
| CDP backend (+ `tap_at_with` native) | `crates/ferridriver/src/backend/cdp/mod.rs` |
| BiDi backend | `crates/ferridriver/src/backend/bidi/page.rs` |
| WebKit Rust backend | `crates/ferridriver/src/backend/webkit/mod.rs` |
| WebKit IPC host (Obj-C) | `crates/ferridriver/src/backend/webkit/host.m` |
| Error type (`FerriError::Unsupported`, From<String> upgrade) | `crates/ferridriver/src/error.rs` |
| NAPI option types | `crates/ferridriver-node/src/types.rs` |
| NAPI Locator/Page/Frame | `crates/ferridriver-node/src/{locator,page,frame}.rs` |
| QuickJS convert helpers | `crates/ferridriver-script/src/bindings/convert.rs` |
| QuickJS bindings | `crates/ferridriver-script/src/bindings/{locator,page,frame}.rs` |
| **Injected UtilityScript + serializer (TS)** | `crates/ferridriver/src/injected/{utilityScript,isomorphic/utilityScriptSerializers}.ts` |
| **Injected `window.__fd` installer** | `crates/ferridriver/src/injected/index.ts` (install-on-window section) |
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
The `feedback_never_claim_completion_without_rule9_tests.md` entry
from the 1.5 session remains load-bearing.
