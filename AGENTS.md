# AGENTS.md

Working rules for coding agents in ferridriver. `CONTRIBUTING.md` covers the
same workflow for humans; this file adds what agents most often get wrong.

## Shape of the repo

ferridriver is browser automation in Rust with Playwright's API. The core crate
`crates/ferridriver` is the single source of truth, and everything above it is
a thin translator:

- `ferridriver-node` (NAPI) and `ferridriver-script` (QuickJS bindings) lower
  JS values into Rust types and forward to core. Business logic lives in core.
- `ferridriver-test` owns the one execution pipeline, `TestRunner::run()`, for
  every test type: Rust `#[ferritest]`, TS specs (`ferridriver test`), Gherkin
  (`ferridriver bdd`). Filtering (grep, only, skip, fixme, shard, last-failed),
  `expect` polling and `TestAnnotation` live there.
- `ferridriver-config` owns the config schema. Keys are camelCase on the wire,
  consumed Rust-side only.
- The JS runtime (QuickJS event loop, Node and web standard library, sandbox,
  `fetch`, rolldown-to-bytecode) is the separate
  [ferrijs](https://github.com/salamaashoush/ferrijs) repository, consumed by
  version. To develop both, add a `[patch.crates-io]` pointing at
  `../ferrijs/crates/*` rather than editing the versions.

Crate list: the README's "Project layout". Dependency flow:
`ferridriver-cli -> ferridriver-mcp -> ferridriver <- ferridriver-node`, and
`ferridriver-cli -> ferridriver-bdd -> ferridriver-test -> ferridriver`.

Four backends under `crates/ferridriver/src/backend/`, enum dispatch:

- `cdp-pipe` (default): CDP over fd 3/4, launches Chrome.
- `cdp-raw`: CDP over WebSocket, can attach to a running Chrome.
- `bidi`: Firefox over WebDriver BiDi.
- `webkit`: Playwright's WebKit build through `pw_run.sh`, NUL-delimited JSON
  over fd 3/4, the same transport on Linux and macOS.

## Changing public API

Every public API mirrors Playwright's TypeScript signature in all three layers
and works on all four backends. Work in this order; a step is done only when
its criterion holds.

1. **Read the Playwright source.** `/tmp/playwright` does not survive a reboot,
   and an interrupted clone leaves a `.git` husk, so confirm the working tree
   is there or run
   `git clone --depth 1 https://github.com/microsoft/playwright.git /tmp/playwright`.
   Done when you hold the exact declaration, copied into a doc comment on the
   Rust item. Sources: `packages/playwright-core/src/client/*.ts` (API),
   `packages/playwright/types/test.d.ts` (runner types),
   `packages/isomorphic/*.ts` (encodings such as glob-to-regex),
   `packages/playwright-core/src/server/` (per-backend behaviour).
2. **Rust core.** `Option<T>` for `T | undefined`; overload unions become enums
   (`LocatorLike`, `UrlMatcher`). Done when every option field is honoured on
   every backend. Where a protocol genuinely cannot do it (`page.pdf()` off
   Chromium, as in Playwright), return `FerriError::Unsupported { reason }`.
3. **NAPI** (`crates/ferridriver-node/src/`). `#[napi(object)]` option structs
   use the same field names; unions use `napi::Either` with `ts_args_type`.
   Accept native JS types at the boundary: `RegExp` through `JsRegExpLike`, a
   `Locator` through `LocatorRef`, predicates through `ThreadsafeFunction`.
   Done when `cd crates/ferridriver-node && bun run build:debug` succeeds and
   every changed method in the regenerated `index.d.ts` matches Playwright's
   `test.d.ts`, with the native union where Playwright has one: no `any`,
   `unknown`, struct names, or wire keys like `regexSource` and `glob`.
4. **QuickJS** (`crates/ferridriver-script/src/bindings/`).
   `#[qjs(rename = "camelCase")]`; option bags parse with `serde_from_js` into a
   `JsFoo` struct; optional arguments take `Opt<rquickjs::Value>`; accept class
   instances and plain objects wherever Playwright does. Done when it forwards
   every option the NAPI layer forwards.
5. **Spec-facing types.** Extend the hand-written
   `crates/ferridriver-types/packages/test/index.d.ts`. Done when
   `cd tests && bun x tsc --noEmit -p tsconfig.json` exits 0.
6. **Prove it.** Per option, a `tests/e2e/*.test.ts` spec (QuickJS path) and a
   `bun test` case (NAPI path) that observe an effect occurring only when the
   option took effect: mousedown at `sourcePosition` rather than the centre,
   `trial: true` suppressing every mouse event, `steps` producing N moves. Done
   when it passes on all four projects (`just test-e2e`) across a 5x loop. A
   backend that fails is a backend bug: fix the backend and keep the assertion
   the same on every project.

The commit names the Playwright file it was checked against and states what
landed and what is still missing.

## Gates

- `just lint` runs clippy with `-D warnings` over the workspace. `just test`
  runs `test-rust` (Rust, CLI integration, e2e on four backends, BDD) then
  `test-node` (NAPI build, `bun test`, e2e typecheck). `just ready` is format,
  lint and test: what CI runs.
- Every commit leaves the tree green. A red test met along the way is fixed in
  the same commit, whoever caused it.
- CLI integration tests run serially (`--test-threads=1`). Concurrent browsers
  starve each other and Firefox dies with SIGKILL at startup; that cascade is
  the harness, not your change.
- Tests need Chromium (`ferridriver install --with-deps chromium axe`), the
  Firefox and WebKit builds for the other projects, and the
  `ferridriver-fixtures` server the recipes build.
- Browser behaviour tests belong in `tests/e2e/`. The Rust `mcp_smoke` suite
  holds only what the runner cannot reach: MCP dispatch, `run_script` session
  semantics, trace and HAR validators, multi-page session binding, rmcp
  protocol features.

## Code

- Every crate inherits `[workspace.lints]`. Fix each clippy finding at its
  cause; levels and suppressions stay as they are. `unwrap`, `expect`, `todo`
  and `unsafe` belong in tests only. Delete unused code.
- Failures surface: a typed `FerriError` beats `Ok(default)`, and real protocol
  values beat placeholder strings.
- `FxHashMap` (rustc-hash) over `std::collections::HashMap`. 2-space indent,
  120 columns.
- Comments carry the why the code cannot. Prose anywhere in the repo stays
  plain: no emoji, and "Rust-based" rather than "high-performance".

## Generated files

Edit the source, then regenerate:

- `crates/ferridriver/src/injected/dist/*.min.js` from `injected/*.ts`:
  `bun crates/ferridriver/src/injected/build.ts`.
- `site/docs/contributing.md` and `site/docs/changelog.md` from the root
  `CONTRIBUTING.md` and `CHANGELOG.md`: `node site/scripts/sync-root-docs.mjs`.
- `CHANGELOG.md` from Conventional Commits, by git-cliff at release.
- `Cargo.lock` and `bun.lock` through `cargo update` and `bun install`.

## Before you touch

- **A backend, event plumbing, or a QuickJS binding**: read
  [`docs/backend-quirks.md`](docs/backend-quirks.md). Each entry cost a
  debugging session.
- **`ferridriver-perf`**: it ports the DevTools trace engine, so agreement with
  upstream on real traces is the bar, not its own tests. Run `just perf-diff`
  and read `scripts/perf-diff/README.md`.
- **Authority** (the sandbox, `allow.net`, `allow.commands`): read ferrijs's
  `docs/SANDBOX.md`. A session holds one permission container that only
  narrows; a tool's `allow.net` attenuates what its handler was handed rather
  than adding a second policy.
- **A CLI flag, API signature or config key**: update `site/docs/` in the same
  commit.

## Commits and git

- Conventional Commits with a scope where it helps (`fix(webkit): ...`).
  Messages read as developer-authored, with no AI attribution or
  `Co-Authored-By` trailer.
- Uncommitted work outlives every operation. Before a reset, path checkout,
  clean, rebase or file move, back it up with a commit, a
  `git branch backup-$(date +%F)`, or a copy, and confirm with the maintainer.
  Stash only when asked, and leave `git stash drop` to them. List untracked
  files before moving or deleting any: git holds no copy.
- `main` is never force-pushed.
- macOS filesystems are case-insensitive: check for a case collision before
  creating a file that differs from a neighbour only in case.
- Code must build and run on macOS and Linux, WebKit included.
