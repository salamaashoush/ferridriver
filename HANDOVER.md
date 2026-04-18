# Handover — next Playwright-parity session

This doc is the read-first for any session continuing Playwright-parity
work on `ferridriver`. Keep it current — overwrite with the new session's
summary at the end of each batch.

---

## Branch state

Branch: `main`, **28 commits ahead** of `origin/main`, working tree clean.

Most recent commits (newest first):

```
dc82461 feat(core): addInitScript(script, arg) full surface across all layers (task 3.25)
0f13494 docs: end-of-session handover after Frame/Page/Locator architecture refactor
f3d23a5 feat(core): Playwright-faithful Frame/Page/Locator architecture (task 3.9)
2108779 feat(core): sync Frame/Page accessors + WebKit iframe enumeration (task 3.8)
8fd86a7 docs: end-of-session handover + Section B refinement
c27e256 feat(core): full ScreenshotOptions surface across all backends (task 3.3)
```

## READ FIRST

1. `CLAUDE.md` — **10 non-negotiable Playwright-parity rules**. The ones
   that bit this session:
   - Rule 1 (Rust is source of truth — NAPI/QuickJS are thin delegators).
     The first cut of 3.25 put the Function-to-source serialization
     helper inside `ferridriver-node/src/types.rs`; the user pushed back
     hard. The committed version has all semantic lowering in
     `ferridriver::options::evaluation_script`; NAPI and QuickJS just
     accept JS values, call `.toString()` once on functions (engine-local
     step), and delegate. See the 3.25 commit message for the pattern.
   - Rule 4 (every backend fully wired — no stubs, no "only CDP for
     now"). This is the load-bearing constraint on Tier 1.5 below.
   - Rule 5 (NAPI **and** QuickJS same commit).
2. `PLAYWRIGHT_COMPAT.md` — tracker. Each task lists a canonical
   `/tmp/playwright/...` reference; read before touching core.
3. `/tmp/playwright/packages/playwright-core/types/types.d.ts` — public
   TS surface; byte-for-byte target for the generated `.d.ts`.

## Completed this session

### Task 3.25 — `addInitScript(script, arg)` full surface (commit `dc82461`)

Mirror Playwright's `Function | string | { path?, content? }` union plus
optional `arg` at every layer. Wire stays source-only so the backend
protocol is unchanged; all semantic lowering lives in Rust core.

- **Core** (`options.rs`): new `InitScriptSource` enum (`Function { body
  } | Source | Content | Path`) + `evaluation_script(script, arg)`
  helper mirroring Playwright's `evaluationScript` at
  `/tmp/playwright/packages/playwright-core/src/client/clientHelper.ts:31`.
  Composes `(body)(arg)` with `arg` JSON-stringified, renders absent
  `arg` as the literal `undefined`, preserves `null` (JSON `"null"`),
  reads `{ path }` from disk and appends `//# sourceURL=…`, and rejects
  `(source|content|path) + arg` with Playwright's exact "Cannot evaluate
  a string with arguments" via `FerriError::InvalidArgument`. 10 unit
  tests cover every branch.
- **NAPI**: `NapiInitScript` custom `FromNapiValue` synchronously turns
  `Function | string | object` into a `Send`-safe enum (function
  `.toString()` fires at unmarshal time, sidestepping the `!Send`
  `Unknown<'_>` across-await problem). `NapiInitScriptArg` custom
  `FromNapiValue` distinguishes JS `undefined` (→ `None`) from explicit
  `null` (→ `Some(Value::Null)`). `#[napi(ts_args_type = …)]` forces the
  generated `.d.ts` union byte-for-byte. 6 new `bun test` cases.
- **QuickJS**: shared `bindings/convert::init_script_from_js` does the
  same lowering (`String(fn)` for function source, `.is_null()` /
  `.is_undefined()`, `content` wins over `path`). `PageJs::addInitScript`
  + `PageJs::removeInitScript` added (previously Context-only). 1 new
  backends test exercising all forms + the string+arg error across all
  four backends.

## Session discipline lessons (don't repeat)

1. **Rule 1 violation on first cut**: I added the full serialisation
   helper (including function lowering, path reads, arg stringify) into
   `crates/ferridriver-node/src/types.rs`. The user correctly called
   that out. All semantic logic belongs in Rust core; the binding is
   only responsible for the engine-local step (`.toString()` / reading
   rquickjs Value types).
2. **Async-fn parameter Send issue**: `napi::Unknown<'_>` is not `Send`.
   A `#[napi] pub async fn foo(..., x: Unknown<'_>) -> ...` fails the
   runtime's `Send` bound even if `x` is consumed before the first
   `.await`, because napi-rs's macro expansion captures the parameter
   into the future. Fix: write a custom `FromNapiValue` that produces a
   `Send`-safe owned type (strings, enums) so the JS-scope-bound value
   is converted **before** the async body runs.
3. **Option<T> collapses null + undefined**: napi-rs's `FromNapiValue`
   for `Option<T>` treats JS `null` and `undefined` the same (→
   `None`). For Playwright semantics that distinguish the two (e.g.
   `arg: null` should render as `"null"` while `arg: undefined` renders
   as `undefined`), write a custom wrapper type (see
   `NapiInitScriptArg`).
4. **Path to finishing a task**: the script backends test needed two
   `JSON.parse` hops (once for `page.evaluate`'s stringify wrapping,
   once for whatever the page returned). Existing precedent is
   `test_script_emulate_media_all_fields` at `backends.rs:710`.

## Blocked items (do NOT attempt until deps land)

- **3.1 Navigation returns `Response`** → blocks on **1.4**
  (Request/Response lifecycle).
- **3.14 `Locator.evaluate` with arg** → blocks on **1.3** (JSHandle).

## Recommended next task — Tier 1.5 (ClickOptions first)

The user explicitly requested "move to Tier 1 and fix and finish all of
them". Current Tier 1 state:

- **1.1 Structured error taxonomy** — [x] done (pre-existing).
- **1.2 ElementHandle** — [ ] ~30 methods, lifecycle object backed by
  CDP `RemoteObjectId` / WebKit node ref. Depends on **1.3** for the
  serialization protocol (`evaluate(fn, handle)`).
- **1.3 JSHandle** — [ ] new class + Playwright's tagged-union
  serializer (NaN / +Inf / -Inf / Date / RegExp / URL / Map / Set /
  Error / typed arrays / BigInt). See
  `/tmp/playwright/packages/playwright-core/src/protocol/serializers.ts`.
  Unblocks 3.14.
- **1.4 Request / Response / WebSocket as lifecycle objects** — [ ]
  replace event-DTO `NetRequest`/`NetResponse` with full lifecycle
  objects (`response.body()`, `request.timing()`, etc.). Unblocks 3.1.
- **1.5 Action option bags on Locator + Page** — [ ] 10 methods × 10
  fields each, all 4 backends. Highest-leverage first item since the
  backend already has partial wiring (`click_at_opts(x, y, button,
  click_count)` is plumbed; modifiers / position / delay / force /
  trial / steps are not).

Sequencing recommendation: **1.5 → 1.3 → 1.2 → 1.4** (1.5 has zero
dependencies; 1.3 unblocks 1.2; 1.4 is separable).

### Tier 1.5: Full design for ClickOptions (first method to land)

Start here next session. Design is settled; implementation requires
edits across all 4 backends + NAPI + QuickJS + tests. Budget: one full
session per method; 10 methods total so 1.5 is a ~10-session
undertaking even at full pace.

#### Canonical TS surface (Playwright)

```ts
locator.click(options?: {
  button?: 'left' | 'right' | 'middle';
  clickCount?: number;
  delay?: number;
  force?: boolean;
  modifiers?: Array<'Alt' | 'Control' | 'ControlOrMeta' | 'Meta' | 'Shift'>;
  noWaitAfter?: boolean;
  position?: { x: number; y: number };
  steps?: number;
  timeout?: number;
  trial?: boolean;
}): Promise<void>;
```

Source: `/tmp/playwright/packages/playwright-core/types/types.d.ts:12986`.

#### Rust core types to add (`crates/ferridriver/src/options.rs`)

I scaffolded these types at ~line 304 during this session but reverted
before commit because the backend wiring is not yet done and leaving
unreferenced types in the tree violates Rule 10 (no dead code). The
definitions were:

- `MouseButton` enum (`Left | Right | Middle`) with `as_cdp()` returning
  `"left" | "right" | "middle"` and `parse(&str) -> Option<Self>`.
- `Modifier` enum (`Alt | Control | ControlOrMeta | Meta | Shift`) with
  `cdp_bit() -> u8` returning the `Input.dispatchMouseEvent.modifiers`
  bitmask bit (Alt=1, Control=2, Meta=4, Shift=8; ControlOrMeta resolves
  platform-aware via `cfg!(target_os = "macos")`). `key_name() ->
  &'static str` for keydown/keyup events. `parse(&str) -> Option<Self>`.
- `modifiers_bitmask(&[Modifier]) -> u32` helper.
- `ClickOptions { button, click_count, delay, force, modifiers,
  no_wait_after, position, steps, timeout, trial }` struct with
  `resolved_*` convenience methods for the default-applied view.

Re-introduce these **only** when a caller exists (the new
`Locator::click(Option<ClickOptions>)` / `Page::click`).

#### Backend plumbing

The backend's existing click surface is minimal:

```rust
// crates/ferridriver/src/backend/mod.rs:1024
pub async fn click(&self) -> Result<(), String> { /* element_dispatch */ }

// crates/ferridriver/src/backend/mod.rs:678
pub async fn click_at_opts(&self, x: f64, y: f64, button: &str, click_count: u32)
```

Needs to grow to accept the full bag. Recommended shape:

```rust
// crates/ferridriver/src/backend/mod.rs (new)
pub struct BackendClickArgs {
  pub button: MouseButton,
  pub click_count: u32,
  pub delay_ms: u64,
  pub modifiers: u32,  // CDP bitmask already resolved
  pub position: Option<(f64, f64)>,  // resolved viewport coords
  pub steps: u32,
}

impl AnyElement {
  pub async fn click_with(&self, args: BackendClickArgs) -> Result<(), String> {
    element_dispatch!(self, click_with(args))
  }
}
```

Per backend:

- **CDP** (`backend/cdp/mod.rs::CdpElement::click` at ~line 3071): add
  modifier keydown via `Input.dispatchKeyEvent { type: "keyDown", key,
  code, windowsVirtualKeyCode }` for each modifier in the bitmask,
  mousePressed with `modifiers: <bitmask>`, `tokio::time::sleep` for
  `delay_ms`, mouseReleased with same, modifier keyup. Interpolate
  `steps-1` mousemoves from current cursor to target before press if
  steps > 1. `position` is resolved by the Locator-level `actions::`
  helper to account for iframe-offset accumulation (precedent in
  `CdpElement::click` at ~line 3086 — the
  `window.frameElement.getBoundingClientRect()` walk).
- **CDP raw** — shares `CdpElement` with CDP pipe, no separate work.
- **BiDi** (`backend/bidi/page.rs`): `input.performActions` with an
  action source containing `keyDown` for modifiers + `pointerDown` +
  pause(`delay_ms`) + `pointerUp` + `keyUp` for modifiers.
  `/tmp/playwright/packages/playwright-core/src/server/bidi/bidiInput.ts`
  is the reference for the BiDi action encoding.
- **WebKit** (`backend/webkit/mod.rs` + `host.m`): host.m needs a new
  IPC op `Click` (or extend the existing `ClickElement`) that takes
  `{ modifiers: u32, button: u8, click_count: u32, delay_ms: u64,
    position_x: f64?, position_y: f64? }`. The Obj-C handler posts
  `NSEventTypeKeyDown` for each modifier (via `CGEventCreateKeyboardEvent`
  because `WKWebView` swallows synthesised `NSEvent`s otherwise),
  synthesises the `mousedown` with `CGEventCreateMouseEvent`
  carrying `modifierFlags`, `usleep(delay_ms * 1000)`, `mouseup`, then
  modifier keyups. Rebuild: `cargo build -p ferridriver && cp
  target/debug/fd_webkit_host crates/ferridriver-node/fd_webkit_host &&
  (cd crates/ferridriver-node && bun run build:debug)`.

#### Locator/Page wiring

```rust
// crates/ferridriver/src/locator.rs
pub async fn click(&self, opts: Option<ClickOptions>) -> Result<()> {
  let opts = opts.unwrap_or_default();
  retry_resolve!(self, timeout = opts.timeout, |el, page| async move {
    if !opts.is_force() {
      actions::check_click_guard(&el, page).await?;
      actions::wait_for_actionable(&el, page).await.ok();
    }
    if opts.is_trial() {
      // Per Playwright: modifiers ARE still pressed around the no-op
      // so trial-mode tests can gate behavior on modifier-only visibility.
      page.press_modifiers(&opts.modifiers).await?;
      page.release_modifiers(&opts.modifiers).await?;
      return Ok(());
    }
    actions::click_with_opts(&el, page, &opts).await
  })
}
```

`actions::click_with_opts` handles the center/position resolution (with
iframe-offset accumulation, current logic in `CdpElement::click`'s JS
center function — extract it into a shared helper on `actions` so all
backends get the same offset logic) and then calls
`el.click_with(BackendClickArgs { ... })`.

#### NAPI wiring (`crates/ferridriver-node/src/locator.rs` + `page.rs`)

```rust
#[napi(object)]
pub struct ClickOptions {
  #[napi(ts_type = "'left' | 'right' | 'middle'")]
  pub button: Option<String>,
  pub click_count: Option<u32>,
  pub delay: Option<f64>,
  pub force: Option<bool>,
  #[napi(ts_type = "Array<'Alt' | 'Control' | 'ControlOrMeta' | 'Meta' | 'Shift'>")]
  pub modifiers: Option<Vec<String>>,
  pub no_wait_after: Option<bool>,
  pub position: Option<Point>,
  pub steps: Option<u32>,
  pub timeout: Option<f64>,
  pub trial: Option<bool>,
}

// Conversion: napi ClickOptions -> ferridriver::options::ClickOptions
// errors via FerriError::InvalidArgument for bad button / modifier strings.
```

#### QuickJS wiring

In `crates/ferridriver-script/src/bindings/locator.rs` /
`page.rs`, parse the option bag via `serde_from_js` into a local struct,
convert button/modifiers strings into the typed enums.

#### Tests

- Core: unit tests on `MouseButton::parse`, `Modifier::cdp_bit`,
  `modifiers_bitmask`, `ClickOptions::resolved_*`.
- NAPI `bun test`: per-option tests (button=right fires contextmenu;
  click_count=2 fires detail=2; delay=100 measurable; force bypasses a
  pointer-events:none guard; modifiers=['Shift'] fires a shift-click
  that `window.event.shiftKey === true`; position overrides center;
  trial skips mouse events but presses modifiers; timeout short-enough
  fails with TimeoutError).
- Backends test (`crates/ferridriver-cli/tests/backends.rs`): one
  QuickJS-dispatched test per option field, green across all 4
  backends.

#### Rule-4 acceptance

The click feature ships when all 4 backends pass every per-option test.
Do NOT commit until WebKit (modifiers / delay / position through
`host.m`) and BiDi (modifiers via `input.performActions`) both pass
their per-option tests. Partial wiring is a Rule-4 violation; use
`FerriError::Unsupported { reason }` **only** if the protocol genuinely
cannot implement the option — which is not the case for any of these
on any of the four backends.

### After Click lands

Fan out the same pattern to:

- `dblclick` — reuses `ClickOptions` minus `clickCount` (force=2); same
  backends.
- `hover` — subset of `ClickOptions` (force, modifiers, no_wait_after,
  position, timeout, trial).
- `fill` — `{ force, no_wait_after, timeout }`.
- `type` — `{ delay, no_wait_after, timeout }` (note: Playwright's
  `type` has `delay` meaning per-character delay).
- `press` — `{ delay, no_wait_after, timeout }`.
- `check` / `uncheck` / `setChecked` — `{ force, no_wait_after,
  position, timeout, trial }`.
- `tap` — `{ force, modifiers, no_wait_after, position, timeout,
  trial }`.
- `dispatchEvent` — `{ event_init: serde_json::Value, timeout }`.
- `selectOption` — accepts `string | { value, label, index } |
  ElementHandle` (ElementHandle blocks on 1.2).
- `setInputFiles` — `FilePayload { name, mime_type, buffer }` +
  `{ no_wait_after, timeout }`.

## Pitfalls logged this session (don't repeat)

1. **Don't synthesize semantics in the binding layer.** Function
   `.toString()` is engine-local (can only be done where the JS engine
   lives, i.e. in NAPI / QuickJS). Everything else — path reads, arg
   serialisation, wrapper composition, error messages — lives in Rust
   core. The binding passes JS-extracted strings through to core.
2. **`napi::Unknown<'_>` across `.await` fails compile.** Custom
   `FromNapiValue` producing an owned `Send`-safe type is the
   workaround; the unmarshal runs synchronously before the async body.
3. **`Option<serde_json::Value>` collapses `null` and `undefined`.**
   Write a wrapper if the Playwright semantic distinguishes the two.
4. **`page.evaluate` in the QuickJS binding returns JSON-stringified
   strings.** Backends tests that probe the page must
   `JSON.parse(await page.evaluate(...))` to get back raw JS values; for
   string values use `JSON.parse('"string"')` to strip the outer quotes.
   Double-parse (`JSON.parse(JSON.parse(raw))`) is needed only when the
   page side itself stringifies — prefer single-parse by letting the
   page return a raw value.

## Workflow for the next task

1. Read `PLAYWRIGHT_COMPAT.md` for the task.
2. Read `/tmp/playwright/...` for the canonical signature.
3. Implement Rust core first (types + method signature + unit tests).
4. Update **all four backends** in the same commit. If one backend
   can't support an option, use `FerriError::Unsupported { reason: ... }`
   — do not silently no-op and do not stub.
5. Update NAPI binding with `ts_type` / `ts_args_type` where inference
   would produce `any` / a struct name / a loose union. Rebuild
   (`cd crates/ferridriver-node && bun run build:debug`) and diff
   `index.d.ts` against Playwright's `types.d.ts`.
6. Update QuickJS binding (`crates/ferridriver-script/src/bindings/`)
   with a live-browser test in `crates/ferridriver-cli/tests/backends.rs`
   via `c.script_value(...)`.
7. `cargo clippy --workspace --all-targets -- -D warnings` — must be
   clean (clippy `doc_markdown` wants backticks around `BiDi`, `WebKit`,
   `QuickJS`, `PLAYWRIGHT_COMPAT.md`, etc.).
8. `cargo test --workspace` — all green (not just `--lib`).
9. `cd crates/ferridriver-node && bun run build:debug && bun test` —
   all green.
10. `FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
      cargo test -p ferridriver-cli --test backends -- --test-threads=1`
    — all 4 backends green.
11. `cargo fmt --all`.
12. Tick the `PLAYWRIGHT_COMPAT.md` checkbox in the same commit.
13. Commit message references the task ID + the `/tmp/playwright/...`
    source file used. **No AI attribution.** Do not add `Co-Authored-By`
    or "Generated with" lines.

## Benchmark status (deferred)

User asked to confirm we don't sacrifice perf. Pre-refactor baseline
(commit `8fd86a7`, `bench/results/comparison.txt`):

```
--- Headless Shell ---
Workers      │   Playwright │  ferridriver │  Speedup
1            │      9691ms │      4261ms │   2.27x
2            │      5261ms │      2530ms │   2.08x
4            │      5046ms │      2650ms │   1.90x
8            │      5076ms │      2684ms │   1.89x
```

Run `cd bench && bash run_comparison.sh` to compare post-refactor.
Takes ~5 minutes (3 runs × 4 worker counts × 2 modes). Needs
`bench/pw-bench/node_modules/playwright` installed (auto-installs on
first run) and the ferridriver-test CLI built
(`cd packages/ferridriver-test && bun run build:cli`).

## Command cheat sheet

```bash
# Type-check
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings

# Rust tests (workspace, includes integration tests)
cargo test --workspace

# NAPI tests (live browser)
cd crates/ferridriver-node && bun run build:debug && bun test

# All 4 backend suites via MCP (live browser)
cargo build -p ferridriver-cli
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1

# BDD features
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  bun run packages/ferridriver-test/src/cli.ts test tests/features/<feature>

# Rebuild the injected JS engine after editing crates/ferridriver/src/injected/
cd crates/ferridriver/src/injected && bun build.ts

# Rebuild the WebKit IPC host after editing host.m
cargo build -p ferridriver
cp target/debug/fd_webkit_host crates/ferridriver-node/fd_webkit_host
(cd crates/ferridriver-node && bun run build:debug)

# Rebuild ferridriver-test CLI (needed by bench)
cd packages/ferridriver-test && bun run build:cli

# Bench
cd bench && bash run_comparison.sh
```

## Key source locations

| area | path |
|---|---|
| Option structs | `crates/ferridriver/src/options.rs` |
| Page (facade over mainFrame) | `crates/ferridriver/src/page.rs` |
| Frame (resolution primitive) | `crates/ferridriver/src/frame.rs` |
| Locator (carries Frame) | `crates/ferridriver/src/locator.rs` |
| Actions (shared helpers) | `crates/ferridriver/src/actions.rs` |
| Selector engine (Rust parser) | `crates/ferridriver/src/selectors.rs` |
| Selector engine (Playwright TS) | `crates/ferridriver/src/injected/*.ts` |
| Backend wire structs | `crates/ferridriver/src/backend/mod.rs` |
| CDP backend | `crates/ferridriver/src/backend/cdp/mod.rs` |
| BiDi backend | `crates/ferridriver/src/backend/bidi/page.rs` |
| WebKit backend (Rust) | `crates/ferridriver/src/backend/webkit/mod.rs` |
| WebKit IPC host (Obj-C) | `crates/ferridriver/src/backend/webkit/host.m` |
| NAPI types | `crates/ferridriver-node/src/types.rs` |
| NAPI Page/Frame/Locator/Context | `crates/ferridriver-node/src/` |
| QuickJS convert helpers | `crates/ferridriver-script/src/bindings/convert.rs` |
| QuickJS bindings | `crates/ferridriver-script/src/bindings/` |
| MCP server | `crates/ferridriver-mcp/src/server.rs` |
| MCP CLI args | `crates/ferridriver-cli/src/cli.rs` |
| Tracker | `PLAYWRIGHT_COMPAT.md` |
| Rules | `CLAUDE.md` (Playwright Parity Rules section) |

## State of memory

Auto-memory entries under
`/Users/sashoush/.claude/projects/-Users-sashoush-Workspace-Box-ferridriver/memory/`
capture durable preferences. They were stable through this session — no
new memories added. `CLAUDE.md` is the durable source — trust it over
any memory divergence.
