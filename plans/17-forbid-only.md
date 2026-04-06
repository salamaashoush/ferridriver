# [DONE] Feature: --forbid-only

## Context
`test.only()` is essential during development but catastrophic in CI: it silently reduces test coverage to a single test while reporting "all tests passed." `--forbid-only` makes the test run fail immediately if any `.only()` marker is found, ensuring full suite coverage in CI. This is a simple but critical CI safety net.

Note: This feature overlaps with plan 02 (test.only). This plan covers the standalone `--forbid-only` enforcement in detail, including scanning behavior, error reporting, and config integration. Plan 02 covers the `Only` annotation itself.

## Design

### Architecture
| Crate/Package | Role |
|---|---|
| `ferridriver-test` | Scan `TestPlan` for `Only` annotations, fail with detailed error |
| `ferridriver-bdd` | Scan BDD plan for `@only` tags |
| `ferridriver-cli` | `--forbid-only` flag |
| `packages/ferridriver-test` | `forbidOnly` config option |

### Core Changes (ferridriver-test)
- The `forbid_only` field already exists in `TestConfig` (line 57 of `config.rs`).
- New function in `runner.rs` or `discovery.rs`:
  ```rust
  pub fn check_forbid_only(plan: &TestPlan) -> Result<(), ForbidOnlyError> {
    let only_tests: Vec<&TestId> = plan.suites.iter()
      .flat_map(|s| &s.tests)
      .filter(|t| t.annotations.iter().any(|a| matches!(a, TestAnnotation::Only)))
      .map(|t| &t.id)
      .collect();

    if only_tests.is_empty() {
      return Ok(());
    }

    Err(ForbidOnlyError { tests: only_tests })
  }
  ```
- `ForbidOnlyError`:
  ```rust
  pub struct ForbidOnlyError {
    pub tests: Vec<TestId>,  // all tests marked with .only()
  }
  impl fmt::Display for ForbidOnlyError {
    // "Error: test.only() found in 3 test(s):\n  file.rs > suite > test1\n  ..."
  }
  ```
- In `TestRunner::run()`, after plan construction but before dispatch:
  ```rust
  if self.config.forbid_only || self.overrides.forbid_only {
    if let Err(e) = check_forbid_only(&plan) {
      eprintln!("{e}");
      return 1;  // exit code 1
    }
  }
  ```
- Also check suite-level `Only` annotations (if a whole `describe.only()` is used).

### BDD Integration (ferridriver-bdd)
- Same check applies: scan BDD-generated `TestPlan` for `Only` annotations.
- `@only` tag on a Feature or Scenario -> `TestAnnotation::Only`.
- Error message includes `.feature` file path and scenario name.

### NAPI + TypeScript (ferridriver-napi, packages/ferridriver-test)
- Config: `forbidOnly: true` in `ferridriver.config.ts`.
- The TS `hasOnly` flag is already tracked in the registry.
- NAPI passes `forbid_only` config to Rust.
- Error message is printed by Rust and the process exits with code 1.

### CLI (ferridriver-cli)
- `--forbid-only` flag on `TestArgs` and `BddArgs`.
- Maps to `CliOverrides::forbid_only`.
- Recommended CI usage: `ferridriver test --forbid-only`.

### Component Testing (ferridriver-ct-*)
- No CT-specific changes. The check runs on the unified test plan.

## Implementation Steps
1. Add `Only` variant to `TestAnnotation` (if not done in plan 02).
2. Add `forbid_only: bool` to `CliOverrides`.
3. Implement `check_forbid_only()` function with detailed error reporting.
4. Call `check_forbid_only()` in `TestRunner::run()` before dispatch.
5. Add `--forbid-only` flag to `TestArgs` and `BddArgs` in `cli.rs`.
6. Map CLI flag to `CliOverrides::forbid_only`.
7. Pass `forbidOnly` config from NAPI to Rust.
8. Write tests.

## Key Files
| File | Action |
|---|---|
| `crates/ferridriver-test/src/model.rs` | Modify — add `Only` variant (if needed) |
| `crates/ferridriver-test/src/runner.rs` | Modify — add `check_forbid_only()` call |
| `crates/ferridriver-test/src/discovery.rs` | Modify — add `check_forbid_only()` function |
| `crates/ferridriver-test/src/config.rs` | Modify — add `forbid_only` to `CliOverrides` |
| `crates/ferridriver-cli/src/cli.rs` | Modify — `--forbid-only` flag |

## Verification
- Unit test: plan with no `Only` annotations + `forbid_only=true` -> Ok (runs normally).
- Unit test: plan with 2 `Only` annotations + `forbid_only=true` -> error listing both tests.
- Unit test: plan with `Only` + `forbid_only=false` -> Ok (only filter applied, no error).
- Integration test: `ferridriver test --forbid-only` with a `.only()` test -> exit code 1, error printed.
- BDD test: `ferridriver bdd --forbid-only` with `@only` scenario -> exit code 1.
- CI verification: add `--forbid-only` to CI config, push a branch with `.only()`, verify CI fails.
