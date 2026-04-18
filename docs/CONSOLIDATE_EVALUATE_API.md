# Prompt — consolidate the evaluate API to Playwright's exact shape

Paste this entire file into a fresh Claude Code session on any device.
No local-device state is needed.

---

Your job: collapse ferridriver's user-facing evaluate surface to match
Playwright's public API **exactly**. Today the surface has three
generations of methods coexisting because prior sessions bolted on
the Playwright-shape variants next to the legacy ones. Ship this as a
single focused commit. Do not interleave with other parity work.

## Read first

1. `CLAUDE.md` — the Playwright-parity rules. Rule 1 (Rust is source
   of truth; NAPI / QuickJS are thin mirrors) and Rule 10 (no escape
   hatches) are the binding constraints here.
2. `HANDOVER.md` — block summary that precedes this work.
3. `PLAYWRIGHT_COMPAT.md` — tracker. This task does not flip any
   checkbox; it's a rename + delete + migrate.
4. `/tmp/playwright/packages/playwright-core/src/client/{page,frame,
   locator,jsHandle,elementHandle}.ts` — the canonical signatures.
   If `/tmp/playwright` is empty on this device,
   `git clone https://github.com/microsoft/playwright /tmp/playwright`
   first.
5. `/tmp/playwright/packages/playwright-core/src/server/javascript.ts`
   — especially `evaluateExpression` (the user-entry server
   primitive) and `JSHandle.evaluate` at line 161 (how the handle
   rides through as a variadic arg, not a `this` binding).

## Current state

Functionally complete but the method names are a mess:

```
Page::evaluate(expr)                 — legacy, expression-only
Page::evaluate_str(expr)             — legacy, stringify variant
Page::evaluate_with_arg(fn, arg)     — new Playwright shape
Page::evaluate_handle_with_arg(...)  — new

Frame::evaluate(expr)                — legacy
Frame::evaluate_str(expr)            — legacy

Locator::evaluate(expr)              — legacy
Locator::evaluate_all(expr)          — legacy

JSHandle::evaluate_with_arg(...)     — new
JSHandle::evaluate_handle_with_arg   — new

ElementHandle::evaluate_with_arg     — new (delegates to js_handle)
ElementHandle::evaluate_handle_with_arg
ElementHandle::eval_on_selector      — Playwright's $eval
ElementHandle::eval_on_selector_all  — Playwright's $$eval
```

## Target state (Playwright's public API, verbatim)

```
Page.evaluate(fn, arg?)           Page.evaluateHandle(fn, arg?)
Frame.evaluate(fn, arg?)          Frame.evaluateHandle(fn, arg?)
Locator.evaluate(fn, arg?)        Locator.evaluateHandle(fn, arg?)   Locator.evaluateAll(fn, arg?)
JSHandle.evaluate(fn, arg?)       JSHandle.evaluateHandle(fn, arg?)
ElementHandle.evaluate(fn, arg?)  ElementHandle.evaluateHandle(fn, arg?)
ElementHandle.$eval(sel, fn, arg?)        ElementHandle.$$eval(sel, fn, arg?)
```

No expression-only variants. No `_str` variants. One evaluate per
Playwright type, always `(fn, arg?)`.

## Scope

### Core (`crates/ferridriver/src/`)

1. **Rename on `Page` / `JSHandle` / `ElementHandle`:**
   - `evaluate_with_arg` → `evaluate`
   - `evaluate_handle_with_arg` → `evaluate_handle`
   - Keep the raw-wire helper `evaluate_with_arg_wire` as
     `evaluate_wire` (it's binding-only; keep or drop depending on
     whether any user API needs it — audit binding callers).

2. **Delete from `Page`:**
   - `pub async fn evaluate(&self, expression: &str) -> Result<Option<serde_json::Value>>`
   - `pub async fn evaluate_str(&self, expression: &str) -> Result<String>`

3. **Delete from `Frame`:**
   - `pub async fn evaluate(&self, expression: &str) -> Result<Option<serde_json::Value>>`
   - `pub async fn evaluate_str(&self, expression: &str) -> Result<String>`

4. **Delete from `Locator`:**
   - `pub async fn evaluate(&self, expression: &str) -> Result<Option<serde_json::Value>>`
   - `pub async fn evaluate_all(&self, expression: &str) -> Result<Option<serde_json::Value>>`

5. **Build `Frame::evaluate(fn, arg?)` and `Frame::evaluate_handle(fn, arg?)`
   as the canonical frame-scoped primitive.** Frame's today only has
   expression-only evaluate; build the Playwright-shape variants that
   pass the frame's execution context to
   `AnyPage::call_utility_evaluate`'s `frame_id` parameter for child
   frames (`self.id()`) and `None` for the main frame.

6. **Rewire `Page::evaluate` / `Page::evaluate_handle` to delegate to
   the main frame.** Playwright's `page.evaluate` at
   `/tmp/playwright/packages/playwright-core/src/client/page.ts:515`
   calls `this._mainFrame.evaluate(...)`. Ours should do the same.

7. **Build `Locator::evaluate(fn, arg?)` / `evaluate_handle(fn, arg?) /
   evaluate_all(fn, arg?)` via the retry-resolve + delegate pattern.**
   Playwright's `locator.evaluate` at
   `client/locator.ts:129` is `this._withElement(async (h, t) =>
   h.evaluate(fn, arg))`. Our version: `retry_resolve!` inside
   Locator that mints the ElementHandle, then calls
   `element_handle.evaluate(fn, arg)`. `evaluate_all` is
   `this._frame.$$eval(this._selector, fn, arg)` in Playwright —
   mirror that by delegating to Frame's `$$eval` equivalent.

### Bindings

8. **NAPI** (`crates/ferridriver-node/src/{page,frame,locator,js_handle,element_handle}.rs`):
   rename `evaluate_with_arg` → `evaluate` (via `#[napi(js_name)]` if
   needed to keep camelCase), `evaluate_handle_with_arg` →
   `evaluateHandle`. Drop the legacy `evaluate(expression)` NAPI
   methods on Page / Frame / Locator. Generated `index.d.ts` should
   show exactly Playwright's signatures — diff against
   `/tmp/playwright/packages/playwright/types/test.d.ts` after
   rebuild.

9. **QuickJS** (`crates/ferridriver-script/src/bindings/{page,frame,
   locator,js_handle,element_handle}.rs`): same rename. QuickJS
   methods use `#[qjs(rename = "...")]` for camelCase names.

### Migrate ~74 call sites

Inventory from the last audit (`crates/ferridriver-cli/tests/backends.rs`
counts are old — they may be lower after the extraction). Start
with a fresh grep:

```bash
grep -rn '\.evaluate("\|\.evaluate_str("\|\.evaluate_all("' crates/ tests/ packages/ bench/ examples/
```

Expected migration pattern for most call sites (legacy returned
`Option<serde_json::Value>`; new returns `SerializedValue`):

```rust
// old:
let v = page.evaluate("window.foo").await?;
assert_eq!(v, Some(json!("bar")));

// new:
let v = page.evaluate("() => window.foo", None).await?.to_json_like();
assert_eq!(v, Some(json!("bar")));
```

- `to_json_like()` bridges `SerializedValue` → `Option<serde_json::Value>`
  for the JSON-expressible subset. Rich types return `None`.
- For callers that ignore the return value: just `.await?;`.
- For `evaluate_str` callers: `.await?.to_json_like().and_then(|v|
  v.as_str().map(str::to_string)).unwrap_or_default()`. Or add a
  `SerializedValue::as_string()` helper if it's hit often.

The `fn_source` argument is now a function literal string (e.g.
`"() => document.title"`), not a raw expression. The utility script's
`is_function` auto-detect handles both, but per Playwright parity,
callers should pass function form.

## Architecture invariants (don't break)

- Core is source of truth. The evaluate primitive
  `AnyPage::call_utility_evaluate` has the Playwright-matching
  signature already — **do not change it**. This task only renames
  the user-facing wrappers above it.
- Every backend already works end-to-end; you should not need to
  touch `crates/ferridriver/src/backend/**`.
- `JSHandle` already has dual backing (`Remote` | `Value`). Don't
  rip that up.
- The rich-arg walker (NAPI `NapiEvaluateArg`, QuickJS
  `quickjs_arg_to_serialized`) already detects class instances at
  the boundary. Don't rip that up either.

## Tests

- All four backend tests must stay green:

  ```bash
  cargo build -p ferridriver-cli
  FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
    cargo test -p ferridriver-cli --test backends -- --test-threads=1
  ```

- NAPI bun tests:

  ```bash
  cd crates/ferridriver-node && bun run build:debug && bun test
  ```

- Core unit tests:

  ```bash
  cargo test -p ferridriver --lib
  ```

- Workspace clippy:

  ```bash
  cargo clippy --workspace --all-targets -- -D warnings
  ```

Current baseline before the consolidation: 119 core, 727 NAPI bun,
4/4 backend, workspace-lib clippy clean.

Callers inside tests are part of the migration (they use the legacy
methods). Expect to touch:

- `crates/ferridriver-cli/tests/backends.rs` — many test functions
  assert on `page.evaluate(expr)` today.
- `crates/ferridriver/tests/page_api.rs` — significant caller set.
- `crates/ferridriver-test/src/` — component-test mounting uses
  `page.evaluate`.
- `crates/ferridriver-test/src/expect/locator.rs` — expect
  assertions use `locator.evaluate`.
- `crates/ferridriver-bdd/src/steps/{javascript,interaction,storage,
  frame,network}.rs` — BDD step definitions.
- `crates/ferridriver-mcp/src/tools/content.rs` — the `evaluate` MCP
  tool endpoint.
- `packages/ferridriver-test/src/cli.ts` — TS side for CT.

## Commit shape

One commit: `refactor: consolidate evaluate API to Playwright shape`.

Body should list:
- Renames (evaluate_with_arg → evaluate, etc.).
- Deletions (six legacy methods named verbatim).
- New Frame::evaluate / evaluate_handle as canonical; Page delegates.
- Locator: evaluate / evaluate_handle / evaluate_all via
  retry-resolve + delegate.
- NAPI + QuickJS binding updates.
- ~N call sites migrated (real count, not estimate).
- Baseline numbers all green.

Do not flip any `[x]` in `PLAYWRIGHT_COMPAT.md` — this is a code-
shape refactor, not a parity expansion.

## Ground rules (non-negotiable, from CLAUDE.md)

- Rule 1: Rust core is source of truth. Bindings are thin delegators.
- Rule 2: NAPI + QuickJS + core all mirror Playwright's TS
  signatures in the same commit that changes them.
- Rule 4: every backend real — you should not need to touch
  backend code here, but if something regresses, fix it in core
  rather than skipping a backend.
- Rule 6: always verify against the cloned Playwright source at
  `/tmp/playwright/...` before implementing. Don't reconstruct
  signatures from memory.
- Rule 7: rebuild NAPI with `bun run build:debug` and diff
  `crates/ferridriver-node/index.d.ts` against Playwright's
  `test.d.ts`. Any divergence is a parity bug.
- Rule 10: no `#[allow(clippy::*)]` escape hatches. No `--no-verify`
  on commits.
- No task / phase / rule-number annotations in source comments or
  filenames — that metadata belongs in commits and trackers, not
  code.
- No emojis. No AI attribution in commit messages.

## Close-out

After the consolidation lands, **do not delete this prompt file** —
overwrite it with a pointer note that the consolidation is done and
update `HANDOVER.md` to pivot to §1.4 (Request / Response /
WebSocket lifecycle). `PLAYWRIGHT_COMPAT.md` stays untouched by
this commit (no checkboxes flipped).
