# [DONE] Feature: --last-failed Rerun

## Context
After a test run with failures, developers want to re-run only the failed tests to iterate quickly on fixes. The `RerunReporter` already writes `@rerun.txt` with failed test locations. This feature reads that file and filters the test plan accordingly, closing the loop.

## Design

### Architecture
| Crate/Package | Role |
|---|---|
| `ferridriver-test` | Read `@rerun.txt`, filter `TestPlan` by matching test IDs |
| `ferridriver-bdd` | Filter BDD scenarios by `file:line` from rerun file |
| `ferridriver-cli` | `--last-failed` flag |
| `packages/ferridriver-test` | `--last-failed` flag forwarded to Rust |

### Core Changes (ferridriver-test)
- New function in `discovery.rs`: `filter_by_rerun(plan: &mut TestPlan, rerun_path: &Path)`.
  - Read `@rerun.txt` (one `file:line` or `file > suite > name` per line).
  - Parse into a `HashSet<String>` of file locations.
  - Remove all tests from the plan whose `TestId::file_location()` is not in the set.
  - If file doesn't exist or is empty, log a warning and run all tests (no-op filter).
- Add `last_failed: bool` to `CliOverrides`.
- In `TestRunner::run()`, apply `filter_by_rerun` when `overrides.last_failed` is true.
- Default rerun file path: `{output_dir}/@rerun.txt` (matches `RerunReporter` output).
- Ensure `RerunReporter` is always enabled (or enabled by default) so the rerun file is available.

### BDD Integration (ferridriver-bdd)
- BDD scenarios already have `TestId` with `file` and `line` fields.
- The rerun file contains `features/login.feature:15` — matches `TestId::file_location()`.
- No BDD-specific code needed; the core `filter_by_rerun` works for both E2E and BDD.

### NAPI + TypeScript (ferridriver-node, packages/ferridriver-test)
- NAPI: accept `lastFailed: boolean` in config/overrides, pass to Rust `CliOverrides`.
- TS CLI: `--last-failed` flag, forwarded through.

### CLI (ferridriver-cli)
- Add `--last-failed` flag to `TestArgs` and `BddArgs`.
- Map to `CliOverrides::last_failed`.

### Component Testing (ferridriver-ct-*)
- No CT-specific changes. Works the same — CT tests have `TestId` with file locations.

## Implementation Steps
1. Add `filter_by_rerun(plan: &mut TestPlan, rerun_path: &Path)` in `discovery.rs`.
2. Add `last_failed: bool` to `CliOverrides` in `config.rs`.
3. In `TestRunner::run()`, call `filter_by_rerun` when `last_failed` is set.
4. Add `--last-failed` to `TestArgs` and `BddArgs` in `cli.rs`.
5. Ensure `RerunReporter` is added to the default reporter set (if not already).
6. Add NAPI binding for `lastFailed` option.
7. Forward `--last-failed` in TS CLI.
8. Write tests.

## Key Files
| File | Action |
|---|---|
| `crates/ferridriver-test/src/discovery.rs` | Modify — add `filter_by_rerun` |
| `crates/ferridriver-test/src/config.rs` | Modify — add `last_failed` to `CliOverrides` |
| `crates/ferridriver-test/src/runner.rs` | Modify — apply rerun filter |
| `crates/ferridriver-cli/src/cli.rs` | Modify — `--last-failed` flag |
| `crates/ferridriver-test/src/reporter/rerun.rs` | Verify — ensure always enabled |

## Verification
- Unit test: write a mock `@rerun.txt` with 2 entries, plan with 5 tests, verify only 2 survive filtering.
- Unit test: missing `@rerun.txt` logs warning and runs all tests.
- Integration test: run tests with a failing test, then run with `--last-failed`, verify only the failed test runs.
- BDD: run features, one scenario fails, `--last-failed` re-runs only that scenario.
