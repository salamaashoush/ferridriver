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

1. **Resolve `expect()` coupling.** Move
   `crates/ferridriver-test/src/expect/{mod,locator,page}.rs` into
   `ferridriver` core (it needs only `ferridriver` + `regex`/`image`);
   repoint `ferridriver-node` and `ferridriver-test` to
   `ferridriver::expect`. Compile-gate before proceeding.
2. **Wire JS step loading into `ferridriver bdd`** (config/CLI glob for
   step files; `JsBddSession` per worker; see `DECISIONS.md`). Until this
   lands, do not remove the TS runner — it is the only JS step path.
3. **Slim `ferridriver-node` to a core binding.** Delete
   `test_runner.rs`, `bdd_registry.rs`, `test_fixtures.rs`,
   `test_info.rs`, `step_handle.rs`, `js_reporter.rs` (~2.6k LOC); trim
   `lib.rs` module list; drop `ferridriver-bdd` / `ferridriver-config`
   (and `ferridriver-test`, pending the expect decision) from
   `ferridriver-node/Cargo.toml`. No Rust crate has a path dep on
   `ferridriver-node` (cdylib), so slimming cannot break the workspace.
4. **Rewire config-types.** The TS CLI is the sole consumer of the
   generated config types: drop `ts-rs` from `ferridriver-config`, the
   `.cargo/config.toml` `TS_RS_EXPORT_DIR`, and the `config-types` /
   `check-config-types` justfile recipes; remove `check-config-types`
   from `just ready`.
5. **Delete `packages/ferridriver-test`** and `bench/fd-tests`; edit
   root `package.json` workspaces + devDeps; regenerate `bun.lock`.
6. **CI.** Remove the `napi` job (and its entry in `conclusion.needs`);
   replace the two `cli.ts install ... chromium` steps with
   `cargo run --bin ferridriver -- ...` or a surviving installer;
   remove the `@ferridriver/test` npm publish from `release.yml`.
7. **justfile.** Delete/rewrite `config-types`, `check-config-types`,
   `profile-ts`; strip `packages/ferridriver-test` lines from `release`.
   `just test` / `just bdd` / `just ready` themselves are pure Rust and
   need no change beyond step 4.
8. **Delete the dead `ferridriver-node/test/*.test.ts`** that import the
   removed package; keep only CORE-binding bun tests if a thin harness
   is retained.
9. **Docs.** `CLAUDE.md:50,74,154`, `site/docs/**`, `HANDOVER.md`,
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
