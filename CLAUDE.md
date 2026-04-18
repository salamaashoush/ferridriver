# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**ferridriver** is a high-performance browser automation library in Rust with a Playwright-compatible API. It supports multiple CDP backends and native WebKit, exposes an MCP server for AI agents, and provides Node.js/Bun bindings via NAPI-RS.

## Build Commands

Uses `just` (justfile) and cargo aliases (`.cargo/config.toml`):

| Command | Purpose |
|---|---|
| `just check` (or `just c`) | Type-check workspace |
| `just test` | Build binary + NAPI, run all Rust tests, TS tests, backend integration tests, BDD features |
| `just test-backend cdp_pipe` | Run tests for a single backend (`cdp_pipe`, `cdp_raw`, `webkit`) |
| `just test-ts` | NAPI/TypeScript tests with Bun |
| `just bdd *args` | Run BDD feature tests |
| `just lint` | `cargo clippy --workspace --all-targets -- -D warnings` |
| `just fmt` | Format check |
| `just fix` (or `just f`) | Auto-fix lint + format |
| `just ready` (or `just r`) | Full CI gate: fmt + lint + test |
| `just build` | Release build (full LTO, strip) |
| `just build-fast` | Release-fast (thin LTO, parallel codegen) |
| `just run` | Run MCP server (stdio) |
| `just run-http` | Run MCP server (HTTP, port 8080) |

Cargo aliases: `cargo ck`, `cargo lint`, `cargo lintfix`, `cargo release`, `cargo release-fast`.

## Workspace Structure

12 crates in `crates/`:

```
ferridriver              Core library: Browser, Page, Locator, Frame, backends
ferridriver-mcp          MCP server library (rmcp-based, stdio + HTTP transports)
ferridriver-cli          CLI binary (MCP server only: stdio + HTTP transports)
ferridriver-node         Node.js/Bun native addon via NAPI-RS (thin target over core)
ferridriver-test         E2E test runner: parallel workers, fixtures, reporters, retries
ferridriver-test-macros  Proc macros: #[ferritest], #[ferritest_each]
ferridriver-bdd          BDD/Cucumber framework: step registry, Gherkin parser, translators
ferridriver-bdd-macros   Proc macros: #[given], #[when], #[then], #[step]
```

TS packages in `packages/`:

```
packages/ferridriver-test    TS CLI + test API (test.each, describe, expect, BDD steps)
```

Dependency flow: `ferridriver-cli` -> `ferridriver-mcp` -> `ferridriver` <- `ferridriver-node`

Test framework flow: `ferridriver-cli` -> `ferridriver-bdd` -> `ferridriver-test` -> `ferridriver`

## Architecture

### Core Principle

Rust is the source of truth. NAPI is a thin target. TS is a thin wrapper.

- All filtering (grep, only, skip, fixme, shard, last-failed) happens in the core runner
- All expect/assertion polling happens in Rust via NAPI expect methods
- The NAPI test runner delegates to `TestRunner::run()` — no separate execution loop
- `TestAnnotation` is shared between Rust and TS via serde serialization
- Never duplicate logic in NAPI/TS that exists in Rust core

### Backend System (enum dispatch, not trait objects)

Three backends in `crates/ferridriver/src/backend/`:

- **CdpPipe** (default) — CDP over Unix pipes (fd 3/4), lowest latency, launches Chrome
- **CdpRaw** — CDP over WebSocket, fully parallel, can connect to running Chrome
- **WebKit** — macOS-only native WKWebView via Objective-C subprocess IPC

Backend directory structure:
```
backend/
  cdp/
    mod.rs          Unified CDP implementation (~87KB)
    pipe.rs         Pipe transport
    ws.rs           WebSocket transport
    transport.rs    Transport abstraction + CDP tracing
  webkit/
    mod.rs
    ipc.rs
```

### Test Runner (`ferridriver-test`)

- `TestRunner::run()` is the single execution pipeline for ALL test types (E2E, BDD, NAPI, CT)
- Workers launch browsers, create pages, inject fixtures, run hooks, handle retries
- `TestAnnotation` enum: Skip, Slow, Fixme (with condition), Fail, Only, Tag, Info
- Conditional fixme evaluates platform/browser/CI at runtime before test body runs
- `filter_by_only`, `filter_by_grep`, `filter_by_rerun`, `check_forbid_only` in discovery.rs
- Centralized logging via `ferridriver_test::logging::init()` — respects RUST_LOG, FERRIDRIVER_DEBUG, --verbose

### MCP Server (`ferridriver-mcp`)

- `McpServer` in `server.rs` holds shared `Arc<Mutex<BrowserState>>`
- Tools are organized by category in `tools/` (navigation, input, content, cookies, storage, emulation, network, bdd)
- `McpServerConfig` trait allows customization (chrome args, auth, metadata)

### Key Source Files

- `crates/ferridriver/src/page.rs` — Page API (~60KB)
- `crates/ferridriver/src/locator.rs` — Locator API (~36KB)
- `crates/ferridriver/src/backend/cdp/mod.rs` — Unified CDP backend (~87KB)
- `crates/ferridriver-mcp/src/server.rs` — MCP server core (~27KB)
- `crates/ferridriver-test/src/runner.rs` — Test runner orchestrator
- `crates/ferridriver-test/src/worker.rs` — Worker: browser, fixtures, hooks, retries
- `crates/ferridriver-test/src/expect/` — Auto-retrying assertions (Playwright-style errors)
- `crates/ferridriver-test/src/logging.rs` — Centralized tracing init

## Code Style & Linting

- **Nightly Rust** toolchain, edition 2024
- **2-space indentation**, 120 char line width (see `rustfmt.toml`)
- Clippy: `correctness`/`perf`/`suspicious` = **deny**, `style`/`complexity`/`pedantic` = warn
- `unwrap_used`, `expect_used`, `todo`, `dbg_macro` = warn (relaxed in tests via `clippy.toml`)
- `unsafe_code` = warn
- Uses `FxHashMap` (rustc-hash) instead of `std::HashMap` for performance
- `avoid-breaking-exported-api = false` — breaking API changes are acceptable

## Testing

~430 total tests: ~94 Rust tests + ~337 NAPI/TS tests (Bun) + 83 BDD scenarios (81 pass, 2 skip).

Tests require a Chrome/Chromium binary and Bun. `just test` handles everything automatically:
builds the CLI binary and NAPI .node addon, runs all Rust workspace tests (including backend
integration tests across all 4 backends), runs NAPI/TS tests, and runs BDD feature tests.

The CLI backend tests use `FERRIDRIVER_BIN` env var pointing to the built binary (set
automatically by `just test`). The backend test binary defaults to `target/debug/ferridriver`
if the env var is not set.

To run BDD features manually: `cd packages/ferridriver-test && bun run src/cli.ts bdd -- ../../tests/features/*.feature`

To build NAPI .node binary manually: `cd crates/ferridriver-node && bun run build:debug`

## Git Commits

- Never add `Co-Authored-By`, `Generated by`, or any AI/Claude/Anthropic attribution to commit messages
- Commit messages should look like they were written by the developer
- **Never commit with failing tests, failing clippy, or type errors.** Every commit must leave the tree fully green (`cargo clippy --workspace --all-targets -- -D warnings`, all Rust lib tests, all Bun tests, all script integration tests). Pre-existing failures get fixed in the current commit — no "unrelated," no follow-up tasks.

## Benchmarks

In `bench/` directory, Bun-based. See `bench/CLAUDE.md` for Bun conventions.

## Playwright Parity Rules (non-negotiable)

Governs all work tracked in `PLAYWRIGHT_COMPAT.md`. Memory-of-hard-learned-mistakes; every rule below exists because a prior session violated it.

### 1. Rust is the source of truth; NAPI and QuickJS are thin mirrors

The `ferridriver` core crate defines every public signature. `ferridriver-node` (NAPI) and `ferridriver-script` (QuickJS) are delegators that lower JS types into Rust types and forward to core — they contain zero business logic. If you're about to implement a behavior in the binding layer ("just do the filter composition in NAPI"), stop and put it in Rust core first.

### 2. Every public API mirrors Playwright's TS signature in all three layers

Canonical signature source: `/tmp/playwright/packages/playwright-core/src/client/*.ts`. Read the exact declaration before implementing. Every argument name, optional parameter, option-bag field, and overload union must match in:

1. **Rust core** — `Option<T>` mirrors TS `T | undefined`; overload unions become Rust enums (e.g. `LocatorLike`, `UrlMatcher`).
2. **NAPI** (`crates/ferridriver-node/src/`) — `#[napi(object)]` option structs use matching field names; unions use `napi::Either` + `ts_args_type` to force the precise TS type. Never let napi-rs infer when the result would be `any` or a struct name instead of a JS-level union.
3. **QuickJS** (`crates/ferridriver-script/src/bindings/`) — `#[qjs(rename = "...")]` names match; option bags parse from `rquickjs::Value` into the same fields; accept both class instances AND plain objects where Playwright's TS does.

If the three layers diverge, the parity work is incomplete regardless of test counts. Partial coverage is worse than missing the feature — it gives a false sense of completeness.

### 3. No wire shapes in user-facing API

Never expose Playwright's internal serialization format (`{regexSource, regexFlags}`, `{glob, regexSource, regexFlags, urlPattern}`) as a user-facing type. Accept native JS types:

- **RegExp** — bind via `napi_get_named_property` prototype-chain walking. A struct with `source: String, flags: Option<String>` fields reads a real `RegExp` instance via its prototype accessors. See `JsRegExpLike` in `crates/ferridriver-node/src/types.rs`.
- **Locator** — same trick: `LocatorRef { selector: String }` reads a real NAPI `Locator` class instance via its `.selector` getter.
- **Function predicates** — use `napi::threadsafe_function::ThreadsafeFunction`.

If the user sees `regexSource` or `glob` as a key in the generated `.d.ts`, that's the wire shape leaking. Before accepting "napi-rs can't bind X", read `~/.cargo/registry/src/index.crates.io-*/napi-*/src/bindgen_runtime/js_values/` and `napi-sys-*/src/functions.rs` — often the primitive exists under a different name.

### 4. Every backend gets a real implementation — no stubs, no placeholder strings

Every public API must work on every backend (`cdp-pipe`, `cdp-raw`, `bidi`, `webkit`). Not "stub returns a constant and we'll fix it later." Not "only CDP for now, others return Unsupported." If the protocol supports the operation, implement it — and if it genuinely cannot (e.g. `printToPDF` on `WKWebView`), return a typed `FerriError::Unsupported { reason }` with a clear explanation, not a placeholder value.

- **WebKit**: add an IPC op to `host.m` + `ipc.rs` if needed (`Op::GetWebKitVersion` is an example — queries `CFBundleShortVersionString` from the `com.apple.WebKit` bundle).
- **BiDi**: read `/tmp/playwright/packages/playwright-core/src/server/bidi/` to see what Playwright's own BiDi backend does; sometimes Playwright itself drops features BiDi can't support (e.g. `referer` on goto), sometimes it works around via `network.setExtraHeaders` — we can do better where possible.
- **CDP**: actual CDP calls capture real protocol values, don't reshape them.

Signal this is going wrong: you're about to write `match self { Self::X => real_impl, Self::Y => "Firefox".to_string() }` — stop. Go implement `Self::Y` properly.

### 5. Every API change updates NAPI AND QuickJS script bindings in the same commit

When a `pub` signature in `crates/ferridriver/src/` changes, grep both `crates/ferridriver-node/src/` and `crates/ferridriver-script/src/bindings/` for the method name. Both must be updated. A binding that compiles but was never called from JS (because it still passes `None` for new options "to make it compile") is a parity gap, not a completed task. Add a JS-side test that exercises the new surface via `run_script` for QuickJS and via `bun test` for NAPI.

### 6. Always verify against the cloned Playwright source before implementing

The cloned Playwright repo is at `/tmp/playwright/`. Read it before touching ferridriver code. Specifically:

- `packages/playwright-core/src/client/*.ts` — user-facing API shapes
- `packages/playwright/types/test.d.ts` — test runner types
- `packages/isomorphic/*.ts` — encoding primitives (glob-to-regex, URL matching, etc.)
- `packages/playwright-core/src/server/` — backend-specific implementations (CDP, BiDi, WebKit)

Never reconstruct a signature from memory or docs. `locator.locator(selectorOrLocator)` was previously shipped without the `options` parameter because nobody checked the TS declaration.

### 7. Rebuild NAPI and inspect the generated `.d.ts` after every binding change

`cd crates/ferridriver-node && bun run build:debug`. Open `crates/ferridriver-node/index.d.ts` and diff each changed method's signature against Playwright's `test.d.ts`. Relying on napi-rs inference alone tends to produce `any`, `unknown`, or struct names where Playwright has proper unions. Use `ts_args_type` to force the exact shape.

### 8. Workflow discipline

Per task, in order:

1. Read `PLAYWRIGHT_COMPAT.md` section for the task.
2. Read `/tmp/playwright/...` for the canonical signature.
3. Implement in Rust core (with tests exercising every option field + failure path).
4. Update NAPI binding (with `ts_args_type` where needed + rebuild).
5. Update QuickJS binding (with live-browser integration test).
6. `cargo clippy --workspace --all-targets -- -D warnings` must be clean.
7. `cargo test --workspace --lib` all green.
8. `cd crates/ferridriver-node && bun test` all green.
9. `cargo fmt`.
10. Tick the `PLAYWRIGHT_COMPAT.md` checkbox in the same commit.
11. Descriptive commit message referencing the task IDs and the Playwright source file used.

### 9. Signatures alone are not parity — prove it works end-to-end on every backend

Accepting an option bag in Rust core + NAPI + QuickJS without a test that
dispatches through the whole stack and observes the expected user-visible
effect is a false completion. For every Playwright option you wire through,
there must be an integration test that:

1. Exercises the option via the public JS API (NAPI via `bun test`, QuickJS
   via `run_script` in `crates/ferridriver-cli/tests/backends.rs`).
2. Observes a DOM-side or protocol-side effect that ONLY occurs when the
   option took effect (e.g. mousedown firing at `sourcePosition` rather than
   the element center, `trial: true` suppressing all mouse events, `steps`
   producing N `mousemove` samples — not just that the call didn't error).
3. Passes on every backend the API is claimed to support (`cdp-pipe`,
   `cdp-raw`, `bidi`, `webkit`). If a backend fails, FIX THE BACKEND — do
   not write `if (backend !== 'webkit')` or similar guards in the test.
   Backend-specific NSEvent coalescing, protocol timing, IPC buffering are
   all real problems and all have fixes. Skipping the assertion is a
   shortcut that hides the bug.
4. Is deterministic across runs (5×/10× loops shouldn't show flake). State
   leaking between tests (mouse-button-down, unresolved listeners,
   lingering timers) is your problem to clean up.

If you can't make it work on all backends, file the gap under Section B of
`PLAYWRIGHT_COMPAT.md` with the concrete symptom — never paper over it with
a conditional skip.

### 10. No escape hatches anywhere

- No `unwrap_used` / `expect_used` / `todo` / `unsafe` in non-test code without explicit justification.
- No `#[allow(clippy::...)]` suppressions — fix the underlying issue.
- No `eslint-disable` comments (the user doesn't use eslint).
- No `#[allow(dead_code)]` — delete unused code outright.
  - **Single exception** (see *Lessons learned*, "Keep phase scaffolding"): `#[allow(dead_code)]` on a field or method the CURRENT commit intentionally carries for the NEXT phase of a multi-phase task (PLAYWRIGHT_COMPAT.md phases C → F etc.) IS allowed when the item name is accompanied by a `/// Held so phase-X ...` comment. Never apply at file level; never outside a phase-boundary scenario.
- No `--no-verify` on commits.
- No `git reset --hard` / `git checkout --` to undo changes without user confirmation.
- No silent error swallowing — `FerriError::Unsupported { reason }` is preferred over `Ok(default)` for genuinely-unimplemented paths.

---

## User preferences (sessions on every device)

### Commit messages

- **No AI attribution.** Never add `Co-Authored-By: Claude <…>`, `Generated by Claude Code`, or any AI/Claude/Anthropic signature. Commits must look developer-authored.
- Conventional prefixes (`feat:`, `fix:`, `docs:`, `refactor:`) are fine where they match.
- Be specific about what shipped AND what's still missing — honest partial-completion statements ("timeout accepted but not propagated") beat optimistic "fully supported" claims (see "Never claim task completion without Rule 9 tests" lesson below).

### Tone in code and docs

- **No emojis** in code, docstrings, README, or markdown docs. User preference — carries across every file in the repo.
- Avoid "high-performance" and similar cargo-culted marketing phrasing in ferridriver branding (package.json description, README intro, site metadata). Preferred framing: "Rust-based" / "Rust-powered", lean into "you don't have to stick with JS for a Rust project."

### Git safety (non-negotiable)

- **Never `git stash`** without asking first. Stash can silently drop work; prefer `git branch backup-$(date +%F)`, copy-to-`/tmp`, or `git diff HEAD` patches for comparisons.
- **Never `git stash drop`** — let the user manage stash cleanup.
- **Never destructive operations without confirmation**: `git reset --hard`, `git push --force`, `git checkout -- <path>`, `git branch -D`, `git clean -f`. Check uncommitted work first; list it; ask.
- **Never move/copy/delete untracked files** without listing them and asking. Untracked files have no git backup.
- **Never force-push to `main`/`master`** — warn the user if they request it.
- **macOS filesystem is case-insensitive** (APFS default). `foo.md` and `FOO.md` are the same file. Never copy/create two files differing only in case into the same directory without checking for collisions.

### Rust / build discipline

- **Never skip or suppress lints or type errors** — fix them properly.
- **Never take shortcuts** or simplify code just to compile / pass tests. If a change requires rethinking, say so; don't silently drop the hard case.
- **Never commit with red tests**, even if the red is "pre-existing." Fix it in the same commit that noticed it.
- Cross-platform: repo must build on macOS AND Arch Linux (the user's two machines). No macOS-only syscalls in non-WebKit code.

---

## Lessons learned (hard-won; don't repeat)

Each of these came from a real incident. They're the in-repo canonical copy — previously lived only in the user's per-project auto-memory. Reading them once at the start of a session is faster than re-learning them.

### Rule 9 is real: signatures ≠ parity

A commit that says "full Tier 1.5 option bags shipped across all layers" after landing only the option-struct fields is a false completion. Reality was: `timeout` accepted on every option bag, honored on none; `force` only skipped Locator-level actionability; `tap` was JS-dispatched (`isTrusted: false`) even though CDP supports `Input.dispatchTouchEvent`; `HoverOptions`/`TapOptions` carried a `steps` field Playwright doesn't have. 12 of 13 methods had zero per-option integration tests.

**Before flipping any `[x]` in PLAYWRIGHT_COMPAT.md**: run the per-option test on every backend, cite the test file + backend matrix in the checkbox body. If tests don't exist, the checkbox is `[~]`, not `[x]`. In commit messages, state exactly what landed AND what's missing — optimism costs trust.

### Verify against cloned Playwright source before implementing

The canonical Playwright repo is at `/tmp/playwright/`. Read `packages/playwright-core/src/client/*.ts` + `packages/playwright/types/test.d.ts` + `packages/isomorphic/*.ts` **before** touching ferridriver code. Never reconstruct signatures from memory. `locator.locator(selectorOrLocator)` was once shipped without the `options` parameter because nobody opened the TS declaration. `Locator.and` was shipped as "descendant chain" and `Locator.or` as `:is()` — both wrong, both needed rework.

### Always rebuild NAPI + diff the generated `.d.ts`

After every change to `crates/ferridriver-node/src/*.rs`: `cd crates/ferridriver-node && bun run build:debug`. If the build fails, do not proceed. If it succeeds, open the regenerated `crates/ferridriver-node/index.d.ts` and compare each method's signature to `/tmp/playwright/packages/playwright/types/test.d.ts`. Any divergence is a parity bug. Use `#[napi(ts_args_type = "…")]` to force the precise TS union when napi-rs inference would produce `any` / `unknown` / a struct name.

### Never expose wire/serialization shapes as user-facing API

Playwright accepts native JS types (`string | RegExp | Function | URLPattern`) at the user boundary. Internal serialization like `{regexSource, regexFlags}` or `{glob, regexSource, regexFlags, urlPattern}` is the wire protocol (`packages/isomorphic/urlMatch.ts`), not the API. Users writing `{ regexSource: "/api/", regexFlags: "i" }` instead of `/api/i` is a parity regression.

Napi-rs doesn't ship a `RegExp` type directly, but `napi_get_named_property` walks the prototype chain — a `#[napi(object)]` struct with `source: String, flags: Option<String>` binds to a real `RegExp` via its getters. See `JsRegExpLike` in `crates/ferridriver-node/src/types.rs`. Set `ts_args_type = "url: string | RegExp"` so the generated `.d.ts` shows the native union. Before accepting "napi-rs can't bind X", read `~/.cargo/registry/src/index.crates.io-*/napi-*/src/bindgen_runtime/js_values/` — the primitive usually exists.

### No stubs, no placeholder strings, every backend real

When a test says `browser.version()` returns `"Firefox"` regardless of the real Firefox version (because the BiDi path hardcoded a constant), the silent lie only surfaces after someone spends hours debugging. For each backend:

- **WebKit**: add an IPC op to `host.m` + `ipc.rs` if needed. `Op::GetWebKitVersion` queries `[NSBundle bundleWithIdentifier:@"com.apple.WebKit"]` for real version strings — not a hardcode.
- **BiDi**: use session capabilities from `session.new` (`browserName` + `browserVersion` — real values). Read `/tmp/playwright/packages/playwright-core/src/server/bidi/` before falling back.
- **CDP**: capture what the protocol returns; don't reshape it.

Typed `FerriError::Unsupported { reason }` is OK where a backend genuinely cannot (e.g. `printToPDF` on `WKWebView`). Placeholder values are never OK.

### Every API change updates NAPI AND QuickJS bindings in the same commit

Three layers mirror every signature: Rust core (`crates/ferridriver/src/`), NAPI (`crates/ferridriver-node/src/`), QuickJS (`crates/ferridriver-script/src/bindings/`). After changing a `pub` signature, grep both binding layers for the method name. Both must be updated. A script binding that passes `None` for new options "to make it compile" is a parity gap, not a completed task.

Script-binding pattern: `#[rquickjs::methods]` + `#[qjs(rename = "camelCase")]`. Option bags use `serde_from_js(&ctx, value)` into a `JsFoo` struct, then translate to `ferridriver::options::Foo`. Optional JS args use `Opt<rquickjs::Value<'js>>`, not `Option<T>` — the former lets callers omit the arg entirely.

### Match Playwright's JS API shape in all three layers

Copy Playwright's exact signature into a doc comment before implementing:

```rust
/// Playwright: `locator(selectorOrLocator: string | Locator,
///                      options?: { has?, hasNot?, hasText?, hasNotText? }): Locator`
```

Anti-patterns:
- `Option<String>` where Playwright accepts `string | RegExp` → use an enum.
- `Option<String>` where Playwright accepts `string | Locator` → use `LocatorLike`.
- Missing `options?` bag → re-read the `.d.ts`.
- Script binding taking only a subset of NAPI's args → script parity gap.

### Keep phase scaffolding for multi-phase work

When landing a multi-phase task (e.g. `1.2` / `1.3` phases C → F on ferridriver), fields and methods the NEXT phase will consume belong in the CURRENT commit — not as a follow-up. Removing scaffolding forces re-adding it later, bloats diffs, breaks reviewers' mental model of what a phase actually ships.

Concrete exception to Rule 10's "no `#[allow(dead_code)]`": apply `#[allow(dead_code)]` to the item (never file-level) when the next phase is actively planned, with a `/// Held so phase-X action methods delegate...` justification comment. Example lived in `crates/ferridriver/src/element_handle.rs::ElementHandle::element` through phase C before phase E consumed it.

Bias toward continuing to implement the next phase in the same session so the scaffolding gets consumed before commit.

### Never commit with failing tests or red clippy

Run every time, before every commit:

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd crates/ferridriver-node && bun run build:debug && bun test
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1
```

If a pre-existing test is failing, fix it in the same commit. "Pre-existing failure unrelated to this task" / "flagging for follow-up" is the pattern to kill.

### Backend / wire / binding quirks that cost time the first time

- **rquickjs maps `Option::None` → JS `undefined`, not `null`.** Tests comparing with `=== null` fail. Use loose `== null` (matches both) or explicit `r === null || r === undefined`.
- **QuickJS `page.evaluate` JSON-stringifies primitive results.** Numbers come back as `"42"`. `Number(...)` inside JS or `.as_str().parse::<i64>()` in Rust.
- **QuickJS doesn't have `setTimeout`.** `await new Promise(r => setTimeout(r, 50))` throws `setTimeout is not defined`. Avoid synthetic sleeps in `run_script` tests; observe the next page round-trip or use `page.waitForLoadState`.
- **BiDi injects `data-fdref="<id>"` attributes on DOM elements it references.** `innerHTML` serialises as `<b data-fdref="4">world</b>` on BiDi where CDP/WebKit return bare `<b>world</b>`. Match substrings, not literals.
- **WebKit reuses `Op::Evaluate` for utility-script calls.** Because every handle is reachable from page-side JS via `window.__wr.get(ref_id)`, a full `callFunctionOn`-equivalent can be synthesised as an inline expression — no new IPC op needed. Saves a host.m change.
- **The utility-script `JSON.stringify` wrapper trick keeps the wire clean.** If the wrapper returns the raw isomorphic wire object directly, CDP / BiDi re-serialise it via their own `RemoteValue` format and corrupt the tags. Fix: `JSON.stringify` the result inside the page-side wrapper so the backend only ships flat strings; Rust-side `JSON.parse` back into `SerializedValue`. Canonical wrapper at `crates/ferridriver/src/backend/cdp/mod.rs::UTILITY_EVAL_WRAPPER` — shared with BiDi and WebKit.
- **CDP `Input.dispatchTouchEvent` needs `Emulation.setTouchEmulationEnabled` first** or DOM touch listeners never fire. Not obvious from the protocol docs; surfaced only via a test failure under full-suite vs. isolated contrast.

### Response brevity

Keep responses terse. The user reads diffs and test output directly; no need to narrate "I will now…" before every command or recap "in summary…" after every landing. End-of-turn messages are one to two sentences.
