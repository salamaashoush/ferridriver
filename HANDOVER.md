# Handover — next Playwright-parity session

This doc is the read-first for any session continuing Playwright-parity
work on `ferridriver`. Keep it current — overwrite with the new session's
summary at the end of each batch.

---

## Branch state

Branch: `main`, **26 commits ahead** of `origin/main`, working tree clean.

Most recent commits (newest first):

```
f3d23a5 feat(core): Playwright-faithful Frame/Page/Locator architecture (task 3.9)
2108779 feat(core): sync Frame/Page accessors + WebKit iframe enumeration (task 3.8)
8fd86a7 docs: end-of-session handover + Section B refinement
c27e256 feat(core): full ScreenshotOptions surface across all backends (task 3.3)
b96849e docs: record the LaunchOptions unification in PLAYWRIGHT_COMPAT
a3a42f0 scaffold(core): full ScreenshotOptions struct surface (task 3.3 WIP)
d6f810c fix(state): resolve Chrome binary with real headless flag (task 3.24 followup)
bed0b92 feat(core): emulateMedia full option bag + 3-state null semantic (task 3.24)
b6e0f6c feat(core): drag-and-drop Playwright option bag across all backends (task 3.10)
4fd3cbc docs: codify the non-negotiable Playwright-parity rules in CLAUDE.md
```

## READ FIRST

1. `CLAUDE.md` — **10 non-negotiable Playwright-parity rules**. The ones
   that bit this session:
   - Rule 1 (Rust is source of truth, NAPI/QuickJS are thin mirrors).
   - Rule 4 (real implementation per backend — no "Frame doesn't have X
     yet" shortcuts).
   - Rule 5 (NAPI **and** QuickJS update same commit).
   - Rule 10 (no `expect_used` / panics in non-test code without
     justification — collapse the construction-path race instead).
2. `PLAYWRIGHT_COMPAT.md` — the tracker. Each task lists the canonical
   `/tmp/playwright/...` reference; read that file before touching core.
3. `/tmp/playwright/packages/playwright-core/types/types.d.ts` — public
   TS surface; byte-for-byte target for the generated `.d.ts`.
4. `/tmp/playwright/packages/playwright-core/src/client/{page,frame,locator}.ts`
   — the Page-as-facade-over-mainFrame pattern is now load-bearing in
   our codebase. Mirror it.

## Completed this session

### Task 3.8 — sync Frame accessors (commit `2108779`)

- Page-owned `FrameCache` (`crates/ferridriver/src/frame_cache.rs`) +
  WebKit iframe enumeration via DOM probe.
- `mainFrame`, `frames`, `frame`, `parentFrame`, `childFrames`,
  `isDetached`, `name`, `url` are sync across Rust core, NAPI, QuickJS.
- Selector-engine fix: aliased `internal:has{,-text,-not,-not-text}` to
  the bare engines (Playwright wire format).

### Task 3.9 — Playwright-faithful Frame/Page/Locator (commit `f3d23a5`)

End-to-end architecture refactor. Mirrors Playwright's design verbatim:

- **Frame is the resolution primitive.** `Page::main_frame()` returns
  `Frame` (non-null). `Page::new` and `Page::with_context` are now
  **async** (returning `Result<Arc<Self>>`); they seed the frame cache
  and spawn the FrameAttached/Navigated/Detached listener inside the
  constructor. The separate `init_frame_cache().await?` plumbing is
  gone — every page-construction path is now `Page::new(any_page).await?`
  in 5 call sites.
- **Page is a pure facade over `mainFrame`.** Every `Page::locator`,
  `Page::get_by_*`, `Page::frame_locator`, `Page::click`/`fill`/`hover`/
  every state check etc. reduces to `self.main_frame().<method>(...)`.
  No re-implementation, no shims.
- **Locator carries `Frame`** (was `(Arc<Page>, Option<Arc<str>>)`).
  Constructor: `Locator::new(frame, selector)`. `Locator::page() ->
  &Arc<Page>` derives from `frame.page_arc()`; new `Locator::frame() ->
  &Frame`. Every action path threads
  `self.frame.is_main_frame() ? None : Some(self.frame.id())` to the
  backend so element resolution runs in the right execution context.
- **`FrameLocator` is a sync selector-builder** that produces standard
  parent-frame `Locator`s with `>> internal:control=enter-frame >>`
  selector chains — verbatim Playwright's `client/locator.ts::FrameLocatorImpl`
  model. There is no separate iframe-aware Locator type. Methods:
  `for_iframe_in`, `locator(sel, opts)`, `get_by_*`, `owner`,
  `frame_locator`, `first`, `last`, `nth`. The async `resolve_frame_id`
  is gone.
- **Frame gains the full surface**: action methods (`click`, `dblclick`,
  `hover`, `tap`, `focus`, `fill`, `type`, `press`, `check`, `uncheck`,
  `set_checked`, `select_option`, `set_input_files`, `drag_and_drop`,
  `dispatch_event`, `text_content`, `inner_text`, `inner_html`,
  `get_attribute`, `input_value`, `is_visible`/`hidden`/`enabled`/
  `disabled`/`editable`/`checked`) plus the previously-missing locator
  builders `get_by_alt_text`, `get_by_title`, `frame_locator`. Page
  delegates without local shims.

### Backend changes (commit `f3d23a5`)

- **Single `evaluate_to_element(js, frame_id: Option<&str>)`** per
  backend (CDP/BiDi/WebKit). No `_in_frame` duplicates. CDP threads
  `Runtime.evaluate.contextId`; BiDi threads the browsing-context realm;
  WebKit falls back to main page (per-frame `WKFrameInfo` evaluation
  tracked under Section B).
- **`selectors::query_one` / `query_all` take `frame_id: Option<&str>`**
  — strict-mode tagging path (`[data-fd-sel='0']`) now resolves in the
  same frame, so the tagged element is bound to the right execution
  context.
- **CDP engine injection upgrade**: `InjectedScriptManager::ensure` now
  uses `Page.addScriptToEvaluateOnNewDocument({source, runImmediately:
  true})` instead of `Runtime.evaluate`. Auto-injects `window.__fd` into
  every current document (main + already-loaded iframes) and every
  future document (page navigations + new iframes). Without this, an
  iframe-bound Locator's `evaluate_to_element(js, Some(iframe_id))`
  would query a context with no `window.__fd` and fail silently.
- **CdpElement::click iframe coords**: walks the frame chain via
  `window.frameElement.getBoundingClientRect()` and accumulates per-iframe
  offsets so an iframe button lands at top-level page coords. Playwright
  uses per-frame CDP sessions; we have a single session, so the offset
  math runs in JS at click time.

### Selector engine = verbatim Playwright (commit `f3d23a5`)

Replaced ferridriver's port of the injected engine with the upstream
files at HEAD:

- 14 files in `crates/ferridriver/src/injected/` ←
  `/tmp/playwright/packages/injected/src/`.
- 10 files in `crates/ferridriver/src/injected/isomorphic/` ←
  `/tmp/playwright/packages/isomorphic/`.

Build (`bun build.ts`) gained an `inlineCssPlugin` that resolves
Playwright's `import css from './highlight.css?inline'` Vite-style
import to the file contents — sources stay literally byte-for-byte.
Deleted the redundant `highlightCss.ts` shim. Engine bundle: 163.9 KB
minified.

### NAPI / QuickJS (commit `f3d23a5`)

- NAPI `Frame` exposes the full action surface (sync getters + async
  actions). `Page::main_frame()` returns `Frame` (non-null, matches
  Playwright). `Page::set_checked` and selector-form `Page::tap` added.
  `innerHTML` uses `js_name = "innerHTML"` so the generated `.d.ts`
  matches Playwright's TS exactly.
- QuickJS `FrameJs` exposes the same 25+ action methods as NAPI.
  `PageJs::mainFrame()` returns `FrameJs` (non-null).

## Blocked items (do NOT attempt until deps land)

- **3.1 Navigation returns `Response`** → blocks on **1.4**
  (Request/Response lifecycle).
- **3.14 `Locator.evaluate` with arg** → blocks on **1.3** (JSHandle).

## Recommended next task

**3.22 `Page.opener` + `page.on('popup')`** — S-sized, distinct
subsystem (CDP `Target.targetCreated` event handling), good clean
follow-up after the architecture refactor.

If you want a more substantive item:

- **3.12 StringOrRegex on getBy* / waitForUrl / getAttribute compare**
  — M-sized. `JsRegExpLike` exists in NAPI from 3.5; wire through
  `getBy*` / `waitForUrl` / `getAttribute`. `FrameSelector::url` is a
  natural follow-up extension here. Now that Frame is the resolution
  primitive, this is straightforward to plumb through the get_by_*
  builders on Frame and have Page inherit via the facade.
- **3.17 Auto-waiting deadline parity** — M. Replace fixed backoff at
  `locator.rs:922` with Playwright's exponential polling + deadline
  propagation; per-call timeout overrides `context.set_default_timeout`.

## Remaining Tier 3 — in increasing complexity

### S (pick these for a quick tempo)

- **3.22 Page.opener + page.on('popup')** — CDP `Target.targetCreated`,
  `PopupEvent`.
- **3.25 addInitScript(script, arg)** — Playwright's second positional
  is a JS-serialisable arg (current sig is source-only).

### M

- **3.12 StringOrRegex on getBy*/waitForUrl/getAttribute compare**.
- **3.17 Auto-waiting deadline parity**.
- **3.26 exposeBinding** — promote `page.expose_function` to
  `exposeBinding` with `source = { page, frame, context }` + `{ handle:
  bool }`.

### Tier 4+ items are bigger (BrowserContext options, route, APIRequest,
test runner, NAPI sweep). The architecture refactor in this session
unblocks most of them — see PLAYWRIGHT_COMPAT.md.

## Pitfalls logged this session (don't repeat)

1. **Don't add `_in_frame` duplicates of existing methods.** The user
   pushed back hard on this. If a method needs frame scoping, modify
   the existing signature to accept `frame_id: Option<&str>` rather
   than introducing a parallel function. Update all callers in the same
   commit. Same principle for type renames — fix the build, not the
   import (e.g. add a Bun plugin for `?inline` CSS rather than
   rewriting Playwright's import).
2. **Page::new and Page::with_context are async now.** Every direct
   caller must `await`. Five call sites currently:
   `BrowserContext::new_page`, `BrowserContext::pages`, `MCP::page`,
   `MCP::page_and_context`, `tools/navigation::page` "new"+"select".
   If you add a sixth, await it.
3. **rquickjs maps `Option::None` returns to JS `undefined`, not `null`.**
   Test assertions on `frame.parentFrame() == null` (loose equality)
   work for both; `=== null` fails on QuickJS.
4. **iframe srcdoc HTML attributes don't honor backslash escapes.**
   Use `&quot;` entity, single-quoted inner attributes, or a
   `<script>` tag with addEventListener.
5. **iframe button coords need frame-chain offset accumulation.** CDP
   `Input.dispatchMouseEvent` is top-level; without walking
   `window.frameElement.getBoundingClientRect()`, iframe clicks land at
   the wrong page coords. The fix lives in `CdpElement::click`'s JS
   center function.
6. **CDP engine injection must be `addScriptToEvaluateOnNewDocument`,
   not `Runtime.evaluate`.** Without `runImmediately: true`, iframes
   never receive `window.__fd`.
7. **Selector engine `internal:control=enter-frame` returns `[]`** in
   the JS engine itself — Playwright handles iframe traversal
   server-side. We currently rely on the Locator carrying the right
   `Frame` (resolved once via `FrameLocator::for_iframe_in`); a future
   refactor to handle the selector-chain split in our backend would let
   `FrameLocator::frame_locator` recurse properly through nested
   iframes.

## Workflow for the next task

1. Read `PLAYWRIGHT_COMPAT.md` for the task.
2. Read `/tmp/playwright/...` for the canonical signature.
3. Implement Rust core first (options struct + method) with unit tests.
4. Update NAPI binding with `ts_args_type` / `ts_type` where inference
   would produce `any` / a struct name / a loose union. Rebuild
   (`cd crates/ferridriver-node && bun run build:debug`) and diff
   `index.d.ts` against Playwright's `types.d.ts`.
5. Update QuickJS binding (`crates/ferridriver-script/src/bindings/`)
   with a live-browser test in `crates/ferridriver-cli/tests/backends.rs`
   via `c.script_value(...)`.
6. `cargo clippy --workspace --all-targets -- -D warnings` — must be
   clean (clippy `doc_markdown` wants backticks around code-shaped
   identifiers like `BiDi`, `WebKit`, `PLAYWRIGHT_COMPAT.md`).
7. `cargo test --workspace` — all green (NOT just `--lib`).
8. `cd crates/ferridriver-node && bun run build:debug && bun test` —
   all green.
9. `FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
     cargo test -p ferridriver-cli --test backends -- --test-threads=1`
   — all 4 backends green.
10. `cargo fmt --all`.
11. Tick the `PLAYWRIGHT_COMPAT.md` checkbox in the same commit.
12. Descriptive commit message referencing the task ID + the
    `/tmp/playwright/...` source file used. No AI attribution.

## Benchmark status (deferred)

User asked to confirm we don't sacrifice perf. The pre-refactor baseline
(commit `8fd86a7`, `bench/results/comparison.txt`):

```
--- Headless Shell ---
Workers      │   Playwright │  ferridriver │  Speedup
1            │      9691ms │      4261ms │   2.27x
2            │      5261ms │      2530ms │   2.08x
4            │      5046ms │      2650ms │   1.90x
8            │      5076ms │      2684ms │   1.89x

--- Regular Chrome ---
Workers      │   Playwright │  ferridriver │  Speedup
1            │     20993ms │     16845ms │   1.25x
2            │     16896ms │     13738ms │   1.23x
4            │     15375ms │     13265ms │   1.16x
8            │     13967ms │     11372ms │   1.23x
```

Run `cd bench && bash run_comparison.sh` to compare post-refactor.
Takes ~5 minutes (3 runs × 4 worker counts × 2 modes). The run_script
needs `bench/pw-bench/node_modules/playwright` installed (auto-installs
on first run) and the ferridriver-test CLI built
(`cd packages/ferridriver-test && bun run build:cli`).

If numbers regress, suspects (in order of likelihood):
1. CDP `Page.addScriptToEvaluateOnNewDocument` for engine injection
   adds latency vs the prior `Runtime.evaluate` (engine runs on every
   document load now, not just main).
2. Iframe-offset JS in `CdpElement::click` adds ~1 extra `Runtime.evaluate`
   round-trip per click — Playwright sources are larger than ours so
   parsing the new injected engine takes ~5-10ms more on first load.
3. Locator cloning bumps two Arc refcounts (`Frame.page` + `Frame.id`)
   instead of one — negligible but worth measuring.

## Command cheat sheet

```bash
# Type-check
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings

# Rust tests (workspace, includes integration tests in crates/ferridriver/tests/)
cargo test --workspace

# NAPI tests (live browser)
cd crates/ferridriver-node && bun run build:debug && bun test

# All 4 backend suites via MCP (live browser)
cargo build -p ferridriver-cli
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1

# BDD features (from repo root)
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  bun run packages/ferridriver-test/src/cli.ts test tests/features/<feature>

# Rebuild the injected JS engine after editing crates/ferridriver/src/injected/
cd crates/ferridriver/src/injected && bun build.ts

# Rebuild ferridriver-test CLI (needed by bench)
cd packages/ferridriver-test && bun run build:cli

# Bench
cd bench && bash run_comparison.sh
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
| Page (facade over mainFrame) | `crates/ferridriver/src/page.rs` |
| Frame (resolution primitive) | `crates/ferridriver/src/frame.rs` |
| Frame cache | `crates/ferridriver/src/frame_cache.rs` |
| Locator (carries Frame) | `crates/ferridriver/src/locator.rs` |
| FrameLocator (sync selector builder) | `crates/ferridriver/src/locator.rs::FrameLocator` |
| Selector engine (Rust parser) | `crates/ferridriver/src/selectors.rs` |
| Selector engine (Playwright TS) | `crates/ferridriver/src/injected/*.ts` (verbatim Playwright HEAD) |
| Engine bundler | `crates/ferridriver/src/injected/build.ts` (Bun + inlineCssPlugin) |
| Backend wire structs | `crates/ferridriver/src/backend/mod.rs` |
| CDP backend | `crates/ferridriver/src/backend/cdp/mod.rs` (~88KB) |
| BiDi backend | `crates/ferridriver/src/backend/bidi/page.rs` |
| WebKit backend (Rust) | `crates/ferridriver/src/backend/webkit/mod.rs` |
| WebKit IPC host (Obj-C) | `crates/ferridriver/src/backend/webkit/host.m` |
| NAPI types | `crates/ferridriver-node/src/types.rs` |
| NAPI Page/Frame/Locator | `crates/ferridriver-node/src/{page,frame,locator}.rs` |
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
