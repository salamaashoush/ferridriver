# Feature: Project Dependencies

## Context
Real test suites often need ordered project execution: a "setup" project runs first (e.g., login and save auth state), then "chromium-tests" and "firefox-tests" run in parallel, both depending on setup. Playwright's `dependencies` field in project config enables this DAG-based execution. Without it, users resort to fragile global setup scripts.

## Design

### Architecture
| Crate/Package | Role |
|---|---|
| `ferridriver-test` | Topological sort of projects, sequential/parallel execution phases |
| `ferridriver-cli` | No new flags (config-driven) |
| `packages/ferridriver-test` | `dependencies` in project config |

### Core Changes (ferridriver-test)
- Extend `ProjectConfig` in `config.rs`:
  ```rust
  pub struct ProjectConfig {
    pub name: String,
    pub dependencies: Vec<String>,   // NEW: names of projects that must run first
    pub test_match: Option<Vec<String>>,
    pub browser: Option<BrowserConfig>,
    pub retries: Option<u32>,
    pub timeout: Option<u64>,
    pub storage_state: Option<String>,
    pub teardown: Option<String>,     // NEW: project name to run as teardown
  }
  ```
- New module `crates/ferridriver-test/src/project.rs`:
  - `resolve_project_order(projects: &[ProjectConfig]) -> Result<Vec<Vec<&ProjectConfig>>, CycleError>`:
    - Topological sort using Kahn's algorithm.
    - Returns layers: `[[setup], [chromium, firefox], [teardown]]`.
    - Each layer runs in parallel; layers run sequentially.
    - Detects cycles and reports clear error.
  - `ProjectRunner`:
    - Takes the sorted layers.
    - For each layer: create `TestRunner` per project, run in parallel with `tokio::join!`.
    - Pass artifacts (storage state files) between projects via the output directory.
    - If any dependency project fails, skip all dependents.

- In `TestRunner::run()`:
  - If `config.projects` is non-empty and any has `dependencies`:
    - Delegate to `ProjectRunner` instead of flat execution.
  - If no dependencies, keep current behavior (all projects run in parallel).

### BDD Integration (ferridriver-bdd)
- BDD projects work the same way. A "setup" project can be a BDD feature that logs in.
- Project config can mix BDD and E2E: `setup` project has `features: [...]`, test projects have `test_match: [...]`.

### NAPI + TypeScript (ferridriver-napi, packages/ferridriver-test)
- Config:
  ```ts
  projects: [
    { name: 'setup', testMatch: 'setup.ts' },
    { name: 'chromium', dependencies: ['setup'], use: { storageState: 'auth.json' } },
    { name: 'teardown', testMatch: 'teardown.ts' },
  ]
  ```
- No API changes; purely config-driven.

### CLI (ferridriver-cli)
- `--project <name>` flag: run only a specific project (and its dependencies).
- When specified, filter to the named project + all transitive dependencies.

### Component Testing (ferridriver-ct-*)
- CT projects can depend on a setup project that builds the dev server or seeds data.

## Implementation Steps
1. Add `dependencies: Vec<String>` and `teardown: Option<String>` to `ProjectConfig`.
2. Create `crates/ferridriver-test/src/project.rs` with topological sort.
3. Implement `ProjectRunner` with layer-based parallel execution.
4. Wire `ProjectRunner` into `TestRunner::run()` when dependencies exist.
5. Implement `--project <name>` filter in CLI.
6. Handle dependency failure: skip dependents with clear error message.
7. Add cycle detection with readable error (list the cycle).
8. Test with multi-project configs.

## Key Files
| File | Action |
|---|---|
| `crates/ferridriver-test/src/project.rs` | Create |
| `crates/ferridriver-test/src/config.rs` | Modify — extend `ProjectConfig` |
| `crates/ferridriver-test/src/runner.rs` | Modify — delegate to `ProjectRunner` |
| `crates/ferridriver-test/src/lib.rs` | Modify — add `mod project;` |
| `crates/ferridriver-cli/src/cli.rs` | Modify — `--project` flag |

## Verification
- Unit test: topological sort of `[A -> B, A -> C, B -> D, C -> D]` -> `[[A], [B, C], [D]]`.
- Unit test: cycle detection for `[A -> B, B -> A]` -> error listing cycle.
- Integration test: setup project saves `auth.json`, test project loads it, verify auth works.
- Integration test: setup project fails -> dependent projects are skipped with correct status.
- Verify `--project tests` also runs `setup` dependency automatically.
