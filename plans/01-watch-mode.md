# [DONE] Feature: Watch Mode

## Context
Developers expect instant feedback when editing tests or source files. Watch mode re-runs affected tests automatically on file change, dramatically reducing the edit-run-debug cycle. This is table-stakes for modern test frameworks (Vitest, Jest, Playwright) and is the single most impactful DX feature for adoption.

## Design

### Architecture
| Crate/Package | Role |
|---|---|
| `ferridriver-test` | Core `Watcher` struct, change classification, interactive key handler |
| `ferridriver-bdd` | `.feature` + step file change detection, scenario-level invalidation |
| `ferridriver-cli` | `--watch` flag on `test` and `bdd` subcommands |
| `packages/ferridriver-test` | `--watch` flag forwarded to Rust CLI |
| `ferridriver-napi` | Expose `runWatch()` binding |

### Core Changes (ferridriver-test)
- New module `crates/ferridriver-test/src/watch.rs`:
  - `Watcher` struct wrapping `notify::RecommendedWatcher` with debounce (100ms).
  - `ChangeKind` enum: `TestFile(PathBuf)`, `SourceFile(PathBuf)`, `Config`.
  - On `TestFile` change: re-discover only that file, re-run its tests.
  - On `SourceFile` change: re-run all tests (no dependency graph yet).
  - On `Config` change: reload config, full re-run.
  - Classify files using `test_match` globs from config vs everything else in watched dirs.
- Interactive key handler (reads raw stdin):
  - `a` — run all tests
  - `f` — run only previously failed tests
  - `q` — quit
  - `p` — enter pattern filter mode (type regex, Enter to apply)
  - `Enter` — re-run last filter
- `TestRunner::run_watch(&mut self, plan_factory)` — outer loop that owns the watcher + key handler, calls `self.run()` per cycle.
- Between runs: keep browser alive (don't re-launch). Store `Arc<Browser>` across iterations.

### BDD Integration (ferridriver-bdd)
- Watch `.feature` files: on change, re-parse only that feature, re-run its scenarios.
- Watch step definition files (Rust source in `steps/` dir): on change, re-run all scenarios (step registry may have changed).
- The BDD runner already builds a `TestPlan` from features — the watcher just needs to re-invoke discovery for changed files.

### NAPI + TypeScript (ferridriver-napi, packages/ferridriver-test)
- `ferridriver-napi`: expose `runWatch(config: TestConfig)` that enters the watch loop.
- `packages/ferridriver-test`: CLI passes `--watch` through to the NAPI binary.
- TS test files: on change, re-evaluate the file (Bun's module cache must be invalidated).

### CLI (ferridriver-cli)
- Add `--watch` / `-w` flag to both `TestArgs` and `BddArgs`.
- When `--watch` is set, call `TestRunner::run_watch()` instead of `TestRunner::run()`.
- Print interactive key hints after each run: `[a] all  [f] failed  [p] filter  [q] quit`.

### Component Testing (ferridriver-ct-*)
- CT adapters should trigger re-run when component source files change.
- Watch the component's source directory (derived from CT config) in addition to test files.

## Implementation Steps
1. Add `notify = "7"` dependency to `ferridriver-test/Cargo.toml`.
2. Create `crates/ferridriver-test/src/watch.rs` with `Watcher`, `ChangeKind`, debounce logic.
3. Create `crates/ferridriver-test/src/interactive.rs` for raw stdin key handler (using `crossterm` for raw mode).
4. Add `run_watch()` method to `TestRunner` that loops: wait for change/key -> re-discover -> filter -> run.
5. Implement browser persistence: extract browser launch into a shared `Arc<Browser>` that survives across runs.
6. Add `--watch` flag to `TestArgs` and `BddArgs` in `cli.rs`.
7. Wire up CLI to call `run_watch()` when flag is set.
8. Add NAPI binding for `runWatch`.
9. Forward `--watch` in TS CLI.
10. Test: manual smoke test with file edits, verify only affected tests re-run.

## Key Files
| File | Action |
|---|---|
| `crates/ferridriver-test/src/watch.rs` | Create |
| `crates/ferridriver-test/src/interactive.rs` | Create |
| `crates/ferridriver-test/src/runner.rs` | Modify — add `run_watch()` |
| `crates/ferridriver-test/src/lib.rs` | Modify — add `mod watch; mod interactive;` |
| `crates/ferridriver-test/Cargo.toml` | Modify — add `notify`, `crossterm` deps |
| `crates/ferridriver-cli/src/cli.rs` | Modify — add `--watch` flags |
| `crates/ferridriver-cli/src/main.rs` | Modify — branch on watch mode |

## Verification
- Unit test: `Watcher` correctly classifies file changes as `TestFile` vs `SourceFile`.
- Integration test: create temp dir with test file, start watch, modify file, assert re-run triggered.
- Manual: run `ferridriver test --watch`, edit a test file, verify only that file's tests re-run.
- Manual: press `a` to run all, `f` to run failed, `q` to quit.
- Verify browser is NOT re-launched between watch cycles.
