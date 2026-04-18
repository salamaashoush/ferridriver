# Handover — next Playwright-parity session

This doc is the read-first for any session continuing Playwright-parity
work on `ferridriver`. Keep it current — overwrite with the new session's
summary at the end of each batch.

---

## Branch state

Branch: `main`, 23 commits ahead of `origin/main`, working tree clean.

Recent commits (newest first — same order as `git log --oneline`):

```
c27e256 feat(core): full ScreenshotOptions surface across all backends (task 3.3)
b96849e docs: record the LaunchOptions unification in PLAYWRIGHT_COMPAT
a3a42f0 scaffold(core): full ScreenshotOptions struct surface (task 3.3 WIP)
d6f810c fix(state): resolve Chrome binary with real headless flag (task 3.24 followup)
bed0b92 feat(core): emulateMedia full option bag + 3-state null semantic (task 3.24)
b6e0f6c feat(core): drag-and-drop Playwright option bag across all backends (task 3.10)
4fd3cbc docs: codify the non-negotiable Playwright-parity rules in CLAUDE.md
c6cff4e feat(core): locator/filter Playwright-faithful sigs (tasks 3.11, 3.13, 3.15, 3.16)
3cc6286 feat(core): Browser/Page lifecycle option bags + real version (3.2, 3.19, 3.20, 3.21, 3.23)
2a51aa8 feat(core): full Playwright PDFOptions surface (task 3.4)
```

## READ FIRST

1. `CLAUDE.md` — **10 non-negotiable Playwright-parity rules**, including
   the two added this session:
   - **Rule 9 (new this session)**: "Signatures alone are not parity —
     prove it works end-to-end on every backend." No `if (backend !== 'x')`
     skip guards. Every option gets a live-browser test that observes a
     DOM-side effect only the option can produce.
   - **Rule 10**: no escape hatches (unwraps, allow-attributes, etc).
2. `PLAYWRIGHT_COMPAT.md` — the tracker. Every task has a ref to
   `/tmp/playwright/...` for the canonical signature. Read that file
   before touching core.
3. `/tmp/playwright/packages/playwright-core/types/types.d.ts` — public
   TS surface. Byte-for-byte target for `.d.ts` generation.

## Completed this session

- **3.10 dragAndDrop** — full `(source, target, options)` signature
  across core + NAPI + QuickJS. Options: `force`, `noWaitAfter`,
  `sourcePosition`, `targetPosition`, `steps` (default 1), `strict`
  (page-only), `timeout`, `trial`. WebKit backend gained per-drag
  `mouseDragged:` dispatch + `_doAfterProcessingAllPendingMouseEvents:`
  drain so `steps` produces one DOM `mousemove` per step instead of
  AppKit-coalesced pairs. Closed B.2 (BiDi Firefox drag).
- **3.24 emulateMedia** — full 5-field option bag with
  `T | null | undefined` three-state semantics. New `MediaOverride` enum
  in `options.rs`. Page-level persistent state so partial updates
  compose (Playwright parity). NAPI uses `Option<Either<String, Null>>`.
  QuickJS walks the JS object manually so `null` → Disabled and
  `undefined` → Unchanged stay distinct. CDP sends all 4 features every
  call mirroring `crPage.ts:975`. BiDi returns typed `Unsupported` for
  fields Firefox can't do. WebKit uses `_setOverrideAppearance:` +
  companion `matchMedia` JS patch (native path breaks when composed
  with `setMediaType:` — confirmed WebKit platform quirk).
- **3.24 follow-up** (commit `d6f810c`) — `BrowserState::new` was
  hard-coding `resolve_chromium(false)` so every MCP-spawned ferridriver
  launched full Google Chrome regardless of `--headless`. Full Chrome
  inherits the macOS system appearance (`prefers-color-scheme: dark` on
  dark-mode hosts), which made the CDP `value: ""` reset look like a
  no-op. Fixed by routing everyone through `BrowserState::with_options(mode, LaunchOptions)`
  — single Playwright-shaped path. `BrowserState::new` deleted.
- **3.x Unified launch surface** — `BrowserState::with_options` is the
  only construction path. MCP server, NAPI, and the 5 in-tree state
  tests all go through it. CLI `--headless` help text corrected to
  state the real default (false).
- **3.3 ScreenshotOptions** — full 13-field Playwright surface:
  `animations`, `caret`, `clip`, `fullPage`, `type`, `mask`,
  `maskColor`, `omitBackground`, `path`, `quality`, `scale`, `style`,
  `timeout`. Shared `backend::screenshot_js` helpers for the DOM-side
  JS (caret hide, style inject, animation pause, mask overlay).
  CDP has everything. BiDi honours clip via the native
  `browsingContext.captureScreenshot.clip`, returns typed
  `Unsupported` for `omitBackground` / `scale: "css"`. WebKit returns
  typed `Unsupported` for `clip` / `omitBackground` / `scale: "css"`.

## Blocked items (do NOT attempt until deps land)

- **3.1 Navigation returns Response** → blocks on **1.4**
  (Request/Response lifecycle).
- **3.14 Locator.evaluate with arg** → blocks on **1.3** (JSHandle).

## Remaining Tier 3 — in increasing complexity

### XS / S (pick these for a quick tempo)

- **3.8 Frame sync accessors** — `mainFrame`, `frames`, `frame`,
  `parentFrame`, `childFrames`, `isDetached`, `name`, `url`. Currently
  async, Playwright is sync. Cache in `Page`/`Frame` state. No backend
  work; touches `page.rs`, `frame.rs`, NAPI + QuickJS bindings.
- **3.22 Page.opener + page.on('popup')** — track creator page for new
  targets. Emits `PopupEvent` to the parent. Touches `page.rs`,
  `events.rs`, CDP `Target.targetCreated` handling.
- **3.25 addInitScript(script, arg)** — current signature takes source
  string only; Playwright's second positional is a JS-serialisable
  arg. Touches `context.rs:445`, `page.rs`, the same JSHandle
  serializer path that blocks 1.3 (but a plain JSON-round-trippable
  arg does not need the full JSHandle infra — can ship a limited
  surface first).

### M

- **3.12 StringOrRegex for getBy* / waitForUrl / getAttribute compare**
  — Playwright accepts `string | RegExp` on `getByRole.name`,
  `getByText`, `getByLabel`, `getByPlaceholder`, `getByAltText`,
  `getByTitle`, `getByTestId`, `wait_for_url`, etc. Currently
  string-only. NAPI already has `JsRegExpLike` from 3.5; wire it
  through. Touches `options.rs`, `locator.rs`, every `getBy*` site.
- **3.17 Auto-waiting deadline parity** — fixed backoff
  `[0,0,20,50,100,100,500]` at `locator.rs:922` must become
  Playwright's exponential polling + deadline propagation;
  per-call timeout overrides `context.set_default_timeout`.
- **3.26 exposeBinding** — promote `page.expose_function` to
  `exposeBinding` with `source = { page, frame, context }`; add
  `{ handle: bool }` option. Touches `page.rs`, `context.rs`
  (context-level variant too).

### M/L

- **3.9 Frame action methods** — port 25+ methods to `Frame`
  (click/dblclick/fill/type/press/hover/check/uncheck/set_checked/tap/
  drag_and_drop/dispatch_event/select_option/set_input_files/
  text_content/inner_text/inner_html/get_attribute/input_value/
  is_checked/is_disabled/is_editable/is_enabled/is_hidden/is_visible/
  focus). Each method scopes the locator call to the frame's
  execution context.

## Recommended next task

**3.8 Frame sync accessors** — S-sized, mechanical, opens the door to
3.9 (Frame action methods) and gives us the `Frame` surface that 3.22
(popup event) may need. Pure Rust-core + binding work, no backend
protocol changes expected.

## Command cheat sheet

```bash
# Type-check
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings

# Rust unit tests
cargo test --workspace --lib

# NAPI tests (live browser)
cd crates/ferridriver-node && bun run build:debug && bun test

# All 4 backend suites via MCP (live browser)
cargo build -p ferridriver-cli
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1

# BDD features (quick)
just test-bdd
```

## Rebuild the WebKit IPC host

If you change `crates/ferridriver/src/backend/webkit/host.m`:

```bash
cargo build -p ferridriver
cp target/debug/fd_webkit_host crates/ferridriver-node/fd_webkit_host
cd crates/ferridriver-node && bun run build:debug
```

`build.rs` has a `cargo:rerun-if-changed` on the host binary so the
first two steps are usually automatic, but a manual copy + NAPI rebuild
is the reliable way to ensure `bun test` picks up the new host.

## Key source locations

| area | path |
|---|---|
| Option structs | `crates/ferridriver/src/options.rs` |
| Page API | `crates/ferridriver/src/page.rs` |
| Locator / Frame | `crates/ferridriver/src/{locator,frame}.rs` |
| Backend wire structs | `crates/ferridriver/src/backend/mod.rs` |
| Shared screenshot JS | `crates/ferridriver/src/backend/mod.rs::screenshot_js` |
| CDP backend | `crates/ferridriver/src/backend/cdp/mod.rs` (~88KB) |
| BiDi backend | `crates/ferridriver/src/backend/bidi/page.rs` |
| WebKit backend (Rust) | `crates/ferridriver/src/backend/webkit/mod.rs` |
| WebKit IPC host (Obj-C) | `crates/ferridriver/src/backend/webkit/host.m` |
| NAPI types | `crates/ferridriver-node/src/types.rs` |
| NAPI Page/Locator/Frame | `crates/ferridriver-node/src/{page,locator,frame}.rs` |
| QuickJS bindings | `crates/ferridriver-script/src/bindings/` |
| MCP server | `crates/ferridriver-mcp/src/server.rs` |
| MCP CLI args | `crates/ferridriver-cli/src/cli.rs` |
| Tracker | `PLAYWRIGHT_COMPAT.md` |
| Rules | `CLAUDE.md` (Playwright Parity Rules section) |

## Pitfalls logged this session (don't repeat)

1. **`BrowserState::new` hard-coded `resolve_chromium(false)`** —
   silently ran headless servers against full Chrome, made CDP resets
   look broken on dark-mode macOS. Fixed by collapsing everyone onto
   `with_options(mode, LaunchOptions)`. Lesson: one construction path
   per resource; headless/executable/args must be resolved from the
   same bag, not mutated after the fact.
2. **Removing a failing test instead of root-causing** — I did this
   for the MCP null-reset case. User (correctly) called it out as a
   shortcut. Always debug through the stack until you find the real
   cause; if you genuinely can't, block on it instead of papering over.
3. **WebKit AppKit input coalescing** — `[webview mouseMoved:]` is
   only delivered when no button is held; use `mouseDragged:` during a
   drag and drain `_doAfterProcessingAllPendingMouseEvents:` per-move
   to defeat intra-drag coalescing. Same pattern applies to any future
   rapid input sequence on WebKit.
4. **`serde_from_js` conflates JS `null` and `undefined`** — both fold
   to `Option::None`. When Playwright's contract distinguishes them
   (emulateMedia, likely future null-disables-override fields), walk
   the rquickjs `Object` manually.
5. **napi-rs `Option<String>` rejects JS `null`** — use
   `Option<Either<String, Null>>` where `Null` is
   `napi::bindgen_prelude::Null`. `ts_type` forces the precise union
   in the generated `.d.ts`.
6. **Pre-commit hook auto-stages `cargo fmt` changes on unstaged
   files** — if you `git add` selectively and then commit, extra files
   may land in the commit with an under-describing message. Either
   stage everything intentionally up-front, or run `cargo fmt` yourself
   before staging.

## State of memory

There are feedback memory files under
`/Users/sashoush/.claude/projects/-Users-sashoush-Workspace-Box-ferridriver/memory/`
that capture the durable preferences:

- drop "high-performance" marketing prose
- verify against `/tmp/playwright` before implementing
- rebuild NAPI + diff `.d.ts` against Playwright's types after every
  binding change
- no wire shapes in user-facing API
- no commits with failing tests
- no stubs — every backend fully wired
- script bindings update same commit as NAPI
- match Playwright JS API shape in all three layers

`CLAUDE.md` is the durable source — trust it over any memory divergence.

---

## Workflow for the next task

1. Read `PLAYWRIGHT_COMPAT.md` for the task.
2. Read `/tmp/playwright/...` for the canonical signature.
3. Implement Rust core first (options struct + method) with unit tests.
4. Update NAPI binding with `ts_args_type`/`ts_type` where inference
   would produce `any` / a struct name / a loose union.
5. Update QuickJS binding with a live-browser test via `run_script`.
6. `cargo clippy --workspace --all-targets -- -D warnings` — must be
   clean.
7. `cargo test --workspace --lib` — all green.
8. `cd crates/ferridriver-node && bun test` — all green.
9. `FERRIDRIVER_BIN=...debug/ferridriver cargo test -p ferridriver-cli --test backends -- --test-threads=1`
   — all 4 backends green.
10. `cargo fmt --all`.
11. Tick the `PLAYWRIGHT_COMPAT.md` checkbox in the same commit.
12. Descriptive commit message referencing the task ID and the
    Playwright source file used.
