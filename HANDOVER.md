# Handover — next Playwright-parity session

This doc is the read-first for any session continuing Playwright-parity
work on `ferridriver`. Keep it current — overwrite with the new session's
summary at the end of each batch.

---

## Branch state

Branch: `main`, **25 commits ahead** of `origin/main`, working tree clean
after the upcoming task-3.8 commit. (The previous session left it at 24.)

Most recent commit (this session) at the top of the upcoming commit:

```
feat(core): sync Frame/Page frame accessors + WebKit iframe enumeration (task 3.8)
8fd86a7 docs: end-of-session handover + Section B refinement
c27e256 feat(core): full ScreenshotOptions surface across all backends (task 3.3)
b96849e docs: record the LaunchOptions unification in PLAYWRIGHT_COMPAT
a3a42f0 scaffold(core): full ScreenshotOptions struct surface (task 3.3 WIP)
d6f810c fix(state): resolve Chrome binary with real headless flag (task 3.24 followup)
bed0b92 feat(core): emulateMedia full option bag + 3-state null semantic (task 3.24)
b6e0f6c feat(core): drag-and-drop Playwright option bag across all backends (task 3.10)
4fd3cbc docs: codify the non-negotiable Playwright-parity rules in CLAUDE.md
c6cff4e feat(core): locator/filter Playwright-faithful sigs (tasks 3.11, 3.13, 3.15, 3.16)
```

## READ FIRST

1. `CLAUDE.md` — **10 non-negotiable Playwright-parity rules**. Especially
   Rule 9 (every option gets a live-browser test on every backend, no
   conditional skips) and Rule 5 (every Rust signature change updates
   NAPI **and** QuickJS in the same commit).
2. `PLAYWRIGHT_COMPAT.md` — the tracker. Each task lists the canonical
   `/tmp/playwright/...` reference; read that file before touching core.
3. `/tmp/playwright/packages/playwright-core/types/types.d.ts` — public
   TS surface; byte-for-byte target for the generated `.d.ts`.

## Completed this session

- **3.8 Frame async-vs-sync parity** — full Playwright-mirror sync
  accessor surface across Rust core + NAPI + QuickJS.
  - New `crates/ferridriver/src/frame_cache.rs`: Page-owned
    `FxHashMap<Arc<str>, FrameRecord>` + insertion-order Vec + cached
    main-frame id. Seeded by `Page::init_frame_cache().await` and kept
    fresh by a tokio listener that consumes `FrameAttached` /
    `FrameDetached` / `FrameNavigated` from the page emitter.
  - `Frame` is now just `(Arc<Page>, Arc<str>)` — every accessor reads
    live state from the cache. Sync API: `name`, `url`, `is_main_frame`,
    `parent_frame`, `child_frames`, `is_detached`, plus `Page::main_frame`,
    `Page::frames`, `Page::frame(selector)`.
  - New `FrameSelector { name, url }` lookup struct in
    `crates/ferridriver/src/options.rs` mirrors Playwright's
    `string | { name?, url? }` union. URL field is exact-match for now;
    task 3.12 extends to `StringOrRegex`.
  - All page-creation sites now `.await page.init_frame_cache()`:
    `BrowserContext::new_page`, `BrowserContext::pages`, `MCP::page`,
    `MCP::page_and_context`, and `navigation::page` "new"/"select" arms.
  - **NAPI**: sync methods (`name()`, `url()`, etc.) — getter style was
    dropped in favour of method calls to match Playwright's TS shape.
    `page.frame(selector)` accepts `string | { name?, url? }` via
    `napi::Either<String, FrameSelectorBag>` + `ts_args_type`.
  - **QuickJS**: new `FrameJs` class with the same sync surface plus
    `evaluate`/`evaluateStr`/`title`/`content`/`locator`. `PageJs` gains
    `mainFrame`, `frames`, `frame(selector)` — the union arg walks the JS
    object by hand to support both `frame("alpha")` and
    `frame({ name: "alpha" })`. Action methods (`click`, `fill`, etc.)
    are 3.9's job.
  - **WebKit backend**: `get_frame_tree` previously returned only the
    main frame. Now also probes the DOM via JS for `<iframe>` elements,
    emitting one `FrameInfo` per iframe with synthesized
    `iframe-<view>-<idx>` ids. Lets `frame()`/`frames()` work on WebKit.
    Frame-scoped JS evaluation (`evaluate_in_frame`) still falls back to
    the main frame — that's a separate gap.

- **Pre-existing failures fixed in-place** (per Rule 9):
  - **`page_api_tests` (filter has_text)** — `crates/ferridriver/src/selectors.rs`
    wasn't aliasing Playwright's `internal:has`, `internal:has-text`,
    `internal:has-not`, `internal:has-not-text` engine prefixes. Filter
    selectors composed in 3.11 fell through to CSS and matched nothing.
    Fixed by aliasing in `parse_part` + listing in `is_rich_selector`.
  - **`locator_or_and_tests` (and())** — the test asserted descendant
    behaviour from `.and()`. Playwright's `.and()` is intersection on the
    same element. Test rewritten with `<p class='a b'>` to exercise the
    actual semantics.

## Blocked items (do NOT attempt until deps land)

- **3.1 Navigation returns Response** → blocks on **1.4**
  (Request/Response lifecycle).
- **3.14 Locator.evaluate with arg** → blocks on **1.3** (JSHandle).

## Recommended next task

**3.9 Frame action methods** (M/L). Port 25+ methods (`click`, `dblclick`,
`fill`, `type`, `press`, `hover`, `check`, `uncheck`, `set_checked`,
`tap`, `drag_and_drop`, `dispatch_event`, `select_option`,
`set_input_files`, `text_content`, `inner_text`, `inner_html`,
`get_attribute`, `input_value`, `is_checked`, `is_disabled`,
`is_editable`, `is_enabled`, `is_hidden`, `is_visible`, `focus`) to
`Frame`. Each method scopes the locator call to the frame's execution
context. Ferri's QuickJS `FrameJs` is now a stub for everything beyond
sync accessors + locator + evaluate — those need to land here.

If you want something shorter first:

- **3.22 Page.opener + page.on('popup')** — track creator page for new
  targets via CDP `Target.targetCreated`, emit `PopupEvent` on the
  parent page. S-sized.
- **3.25 addInitScript(script, arg)** — current sig takes source string
  only; Playwright's second positional is a JS-serialisable arg. S-sized
  if you ship JSON-only first; full JSHandle support is 1.3 territory.

## Remaining Tier 3 — in increasing complexity

### XS / S (pick these for a quick tempo)

- **3.22 Page.opener + page.on('popup')** — track creator page for new
  targets via CDP `Target.targetCreated`, emit `PopupEvent`.
- **3.25 addInitScript(script, arg)** — Playwright's second positional
  is a JS-serialisable arg.

### M

- **3.12 StringOrRegex on getBy*/waitForUrl/getAttribute compare** —
  `JsRegExpLike` exists in NAPI from 3.5; wire through `getBy*` /
  `waitForUrl` / `getAttribute`. `FrameSelector::url` is a natural
  follow-up extension here.
- **3.17 Auto-waiting deadline parity** — fixed backoff
  `[0,0,20,50,100,100,500]` at `locator.rs:922` must become Playwright's
  exponential polling + deadline propagation; per-call timeout overrides
  `context.set_default_timeout`.
- **3.26 exposeBinding** — promote `page.expose_function` to
  `exposeBinding` with `source = { page, frame, context }`; add
  `{ handle: bool }` option.

### M / L

- **3.9 Frame action methods** — see above. Now blocks on QuickJS too:
  `FrameJs` needs the full action surface added.

## Pitfalls logged this session (don't repeat)

1. **rquickjs maps `Option::None` returns to JS `undefined`, not `null`.**
   Live-browser tests that assert `=== null` on a `None` return will
   fail on QuickJS. Use `== null` (loose equality, matches both) or
   `=== undefined`. Documented in `test_script_frame_sync_accessors`
   inline comment in `crates/ferridriver-cli/tests/backends.rs`.
2. **MCP page-creation paths bypass `BrowserContext::new_page`.** The
   `ferridriver-mcp` crate constructs `Page::new(any_page)` directly in
   `server.rs::page`, `server.rs::page_and_context`, and `tools/navigation.rs`
   "new" + "select" arms. After adding any required-await page hook to
   `BrowserContext::new_page`, you must also add it to those four sites
   or scripts will see uninitialised state. (This bit me with
   `init_frame_cache` until I plumbed all five callers.)
3. **`Page::new` and `Page::with_context` are sync but the cache needs an
   await.** Pattern: caller does `let page = Page::new(...);
   page.init_frame_cache().await?;`. Don't try to do the seed inside the
   sync constructor.
4. **Pre-existing failures in `cargo test --workspace` are real
   regressions** — `cargo test -p ferridriver-cli --test backends` is
   not a superset. The previous session's "all green" only covered the
   four CLI backend suites + bun. `cargo test --workspace` runs the
   in-tree integration tests under `crates/ferridriver/tests/` too;
   those revealed stale tests + a missing engine alias.
5. **`internal:has-text` / `internal:has` are wire prefixes, not user
   input.** Playwright's client/locator.ts emits them; the server
   accepts both `has-text` and `internal:has-text`. Aliased in
   `selectors.rs::parse_part` + `is_rich_selector`.
6. **WebKit's `get_frame_tree` is JS-probed, not WKFrameInfo-driven.**
   Synthesized ids let the JS surface work, but don't expect
   `evaluate_in_frame(child_id)` to actually run inside the iframe on
   WebKit yet — it falls back to the main frame.

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
   clean. (rustfmt-only doc fixes also count — clippy `doc_markdown`
   wants backticks around code-shaped identifiers.)
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
| Frame API | `crates/ferridriver/src/frame.rs` |
| Frame cache | `crates/ferridriver/src/frame_cache.rs` |
| Locator | `crates/ferridriver/src/locator.rs` |
| Selector engine (Rust) | `crates/ferridriver/src/selectors.rs` |
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

## State of memory

Auto-memory entries under
`/Users/sashoush/.claude/projects/-Users-sashoush-Workspace-Box-ferridriver/memory/`
capture durable preferences:

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
