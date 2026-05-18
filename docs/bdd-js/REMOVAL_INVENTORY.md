# Removal inventory: TS CLI + NAPI test runner

Phased, with what each removal breaks and the fix. Nothing here is
deleted yet — this is the plan to execute once the JS-step CLI path is
wired and accepted.

## Key facts (verified)

- `ferridriver bdd` is already 100% Rust and does **not** shell out to
  Bun. `crates/ferridriver-cli/src/main.rs::run_bdd` ->
  `ferridriver_bdd::run_bdd_with` -> core `TestRunner`.
- `CLAUDE.md:154` ("`bun run src/cli.ts bdd`") is stale; there is no
  `bdd` subcommand in `packages/ferridriver-test/src/cli.ts`.
- The TS CLI (`packages/ferridriver-test`) and the NAPI test runner are
  an independent JS-facing stack, removable without touching the
  `ferridriver bdd` code path.
- Two non-obvious couplings:
  1. `expect()` lives in `ferridriver-test`, and `ferridriver-node`'s
     CORE `locator.rs`/`page.rs` call `ferridriver_test::expect::expect`.
  2. `ts-rs` config-type generation writes into
     `packages/ferridriver-test/src/config-types` and `just
     check-config-types` (in `just ready`) gates on it.

## Phase order

> **Superseded plan note (expect):** the original step 1 (move
> `expect()` into `ferridriver` core) was **not** taken. Final decision:
> `expect()` stays in `crates/ferridriver-test/src/expect/` untouched;
> `ferridriver-test` (Rust crate) is kept (`ferridriver bdd` uses its
> `TestRunner`/config/expect). `ferridriver-node` instead **drops the
> expect surface entirely** — Playwright's core `playwright` library has
> no `expect`, neither does the slimmed node binding.

1. ~~Resolve `expect()` coupling by moving it to core.~~ **Superseded**
   (see note above). `expect()` left in `ferridriver-test`; node's
   expect bindings deleted instead.
2. [x] **Wire JS step loading into `ferridriver bdd`** (rolldown ->
   QuickJS bytecode -> core `TestRunner`; `--steps` glob + `[test].steps`).
   Landed before this removal.

**Phase A — slim `ferridriver-node` to a core-only browser binding:**

3. [x] Delete `test_runner.rs`, `bdd_registry.rs`, `test_fixtures.rs`,
   `test_info.rs`, `step_handle.rs`, `js_reporter.rs`; also deleted the
   now-dead `playwright_namespace.rs` (only the removed `{ playwright }`
   fixture consumed it; top-level `chromium()`/`firefox()`/`webkit()` in
   `browser_type.rs` are the real entry points). Trimmed `lib.rs`
   module list; removed the dead `ApiRequestContext::wrap`.
3a. [x] Removed the entire `expect_*` surface from node's CORE
   `locator.rs` (incl. `parse_screenshot_options`) and `page.rs`
   (`expect_title`/`expect_url`).
3b. [x] `ferridriver-node/Cargo.toml`: dropped `ferridriver-test`,
   `ferridriver-bdd`, `ferridriver-config` (and now-unused
   `async-trait`, `serde`, `async-channel`); kept only `ferridriver`
   (+ napi/runtime). Dropped the `profiling` feature. No remaining
   `ferridriver_test::`/`ferridriver_bdd::`/`ferridriver_config::` paths.
8. [x] **Deleted the dead `ferridriver-node/test/*.ts`** that imported
   the removed `@ferridriver/test`/expect/test surface (11 files incl.
   `_test-helpers.ts`); 22 pure CORE-binding bun test files retained.
   Rebuilt the addon — `index.d.ts` is now core-only (no
   TestRunner/expect/BDD/Playwright* symbols).

**Phase B — delete TS/JS surface + rewire:**

4. [ ] **Rewire config-types.** TS CLI is the sole consumer: drop
   `ts-rs` from `ferridriver-config`, the `.cargo/config.toml`
   `TS_RS_EXPORT_DIR`, and the `config-types` / `check-config-types`
   justfile recipes; remove `check-config-types` from `just ready`.
5. [ ] **Delete `packages/ferridriver-test`**, `packages/ct-*`,
   `examples/ct-*`, `bench/fd-tests`, `tests/steps/*.ts`; edit root
   `package.json` workspaces + devDeps; regenerate `bun.lock`; drop the
   Rust ct examples from root `Cargo.toml` members/default-members.
6. [ ] **CI.** Remove the `napi` job (and its `conclusion.needs`
   entry); replace the two `cli.ts install ... chromium` steps with a
   surviving installer; remove the `@ferridriver/test` npm publish from
   `release.yml`.
7. [ ] **justfile.** Delete `config-types`, `check-config-types`,
   `profile-ts`; remove `check-config-types` from `ready`; strip
   `packages/ferridriver-test` lines from `release`.
9. [ ] **Docs.** `CLAUDE.md:50,74,154`, `site/docs/**`, `HANDOVER.md`,
   `BDD_TODO.md`, `PLAYWRIGHT_COMPAT.md`, `plans/*` — doc-only.

## Highest-risk missable points

- `expect()` in `ferridriver-node` CORE `locator.rs`/`page.rs` (silent
  if you grep module names only).
- `.cargo/config.toml` `TS_RS_EXPORT_DIR` -> deleted dir breaks
  `cargo test -p ferridriver-config`.
- `bench/fd-tests` is a root `package.json` workspace member -> `bun
  install` fails if left dangling.
- CI `napi` job listed in `conclusion.needs` -> branch protection
  breaks if the job is removed but not de-listed.

## Crate disposition

| Crate | Action |
|---|---|
| `ferridriver`, `-config`, `-mcp`, `-cli`, `-script`, `-bdd`, `-bdd-macros`, `-test`, `-test-macros` | keep (script gains a `bdd` binding; bdd gains a `js` module + `ferridriver-script` dep) |
| `ferridriver-node` | slim to a core Playwright-in-Rust binding (~2.6k LOC + deps removed) |
| `packages/ferridriver-test`, `bench/fd-tests` | delete |
