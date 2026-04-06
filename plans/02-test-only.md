# Feature: test.only()

## Context
During development, you need to focus on a single test without commenting out others. `test.only()` runs only marked tests and skips the rest. The counterpart `--forbid-only` prevents `.only()` from leaking into CI, which is a common source of silent test suite gaps.

## Design

### Architecture
| Crate/Package | Role |
|---|---|
| `ferridriver-test` | `TestAnnotation::Only`, plan filtering, `forbid_only` enforcement |
| `ferridriver-test-macros` | `#[only]` attribute for Rust tests |
| `ferridriver-bdd` | `@only` tag support on scenarios/features |
| `ferridriver-cli` | `--forbid-only` flag |
| `packages/ferridriver-test` | `test.only()` already exists in TS — wire to Rust |

### Core Changes (ferridriver-test)
- Add `Only` variant to `TestAnnotation` enum in `model.rs`.
- In `TestRunner::run()`, after all filtering, check if any test has `Only` annotation:
  - If yes, remove all tests without `Only` from the plan.
  - If `config.forbid_only` is true and any `Only` exists, print error listing the files/tests and return exit code 1 without running.
- In `discovery.rs`, `collect_rust_tests()`: map `#[only]` attribute to `TestAnnotation::Only`.
- The `forbid_only` field already exists in `TestConfig` — just need to enforce it.

### BDD Integration (ferridriver-bdd)
- In `filter.rs`: treat `@only` tag as the Only signal.
- When building the BDD `TestPlan`, if any scenario has `@only`, filter to only those scenarios.
- `@only` on a `Feature:` line applies to all scenarios in that feature.
- Combine with existing tag filter: `@only` is processed first, then other tag filters apply on top.

### NAPI + TypeScript (ferridriver-napi, packages/ferridriver-test)
- `test.only()` already exists in `packages/ferridriver-test/src/test.ts` — it sets `modifier: 'only'`.
- NAPI side: when receiving test metadata with `modifier: 'only'`, set `TestAnnotation::Only`.
- The `hasOnly` flag in TS registry triggers plan-level filtering on the Rust side.

### CLI (ferridriver-cli)
- Add `--forbid-only` flag to both `TestArgs` and `BddArgs`.
- Map to `CliOverrides::forbid_only` -> `TestConfig::forbid_only`.

### Component Testing (ferridriver-ct-*)
- No CT-specific changes. `test.only()` works the same way in CT mode since it uses the same test plan.

## Implementation Steps
1. Add `Only` to `TestAnnotation` enum in `crates/ferridriver-test/src/model.rs`.
2. Add `only` filtering logic in `TestRunner::run()` — after grep/shard filters, before dispatch.
3. Add `forbid_only` enforcement: scan plan, print errors, exit 1.
4. Update `#[ferritest]` proc macro to accept `#[only]` attribute.
5. In BDD `filter.rs`, add `@only` tag handling.
6. Add `--forbid-only` to `TestArgs` and `BddArgs` in CLI.
7. Wire NAPI `modifier: 'only'` -> `TestAnnotation::Only`.
8. Add tests for: only filtering, forbid-only rejection, BDD @only.

## Key Files
| File | Action |
|---|---|
| `crates/ferridriver-test/src/model.rs` | Modify — add `Only` variant |
| `crates/ferridriver-test/src/runner.rs` | Modify — add only filter + forbid check |
| `crates/ferridriver-test/src/discovery.rs` | Modify — handle `#[only]` attribute |
| `crates/ferridriver-test-macros/src/lib.rs` | Modify — parse `#[only]` |
| `crates/ferridriver-bdd/src/filter.rs` | Modify — `@only` tag handling |
| `crates/ferridriver-cli/src/cli.rs` | Modify — `--forbid-only` flag |

## Verification
- Unit test: plan with 5 tests, 1 marked `Only` -> only 1 runs.
- Unit test: plan with `Only` + `forbid_only=true` -> exit code 1, error message lists the file.
- BDD test: feature with `@only` on one scenario -> only that scenario runs.
- TS test: `test.only('focused', ...)` -> only that test runs.
- CI test: `ferridriver test --forbid-only` fails if any `.only()` found.
