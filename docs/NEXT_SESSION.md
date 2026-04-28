# Next session — Cluster 7 (project DAG + git-aware filters + WebServer + git metadata)

Cluster 6 (built-in reporters + merge-reports) is shipped. Cluster 7
is the final test-runner cluster: wire the existing `ProjectConfig[]`
DAG into the runner, surface CLI flags for project selection /
dependencies / teardown / `-x` semantics, add git-aware filtering
(`--only-changed`), `--fail-on-flaky-tests`, polish `WebServerConfig`
with `ignore_https_errors` / `graceful_shutdown` / `name`, and add
`captureGitInfo` so test results carry git metadata.

The user prompt described cluster 7 as "one large but cohesive
commit — all surface CLI overrides + filters that compose."

A separate follow-up (6b) covers the TS-authored Reporter interface
bridge (§7.22) — left out of cluster 6 because it spans multiple
layers (NAPI shim, TS helper, lifecycle plumbing) and benefits from
its own focused commit.

## Read-first

1. `CLAUDE.md` — rules + lessons.
2. `PLAYWRIGHT_COMPAT.md` — §7.1 / §7.3 / §7.4 / §7.25 / §7.26 / §7.22.
3. `HANDOVER.md` — Cluster 6 recap.
4. `/tmp/playwright/packages/playwright/src/runner/projectUtils.ts`
   and `loaderHost.ts` — canonical project DAG resolution.
5. `crates/ferridriver-test/src/runner.rs::run_projects` — current
   project handling. `ProjectConfig::dependencies` and `teardown`
   already exist on the struct (cluster 1 plumbing).
6. `crates/ferridriver-test/src/config.rs::WebServerConfig` for the
   §7.25 polish targets.

## Cluster scope

### §7.1 — `--project` DAG

- Surface `--project NAME` as a multi-flag in the TS CLI; map to
  `CliOverrides::project_filter: Vec<String>`.
- Surface `--no-deps` and `--teardown` as well.
- Wire `dependencies` → topological sort already in `run_projects`;
  verify the `--no-deps` flag short-circuits the dependency walk.
- Verify `teardown` projects are deferred until the parent + all
  dependents complete (already implemented; add an integration test).
- `-x` semantics: stop the whole DAG at the first failure (currently
  scoped to a single run via the dispatcher stop flag).

### §7.3 — `--only-changed [ref]`

- Spawn `git diff --name-only HEAD <ref>` (or `git status`-equivalent
  for uncommitted changes when `[ref]` is omitted) and intersect with
  the discovered test files.
- New CLI flag in `cli.ts`; new `CliOverrides::only_changed:
  Option<String>` (None = uncommitted, Some(ref) = compare to ref).
- Skip if outside a git repo with a clear error.

### §7.4 — `--fail-on-flaky-tests`

- New `CliOverrides::fail_on_flaky_tests: bool`. The runner's
  RunSummary already tracks `flaky` count; just bump exit code to
  non-zero if `failOnFlakyTests && flaky > 0`.

### §7.25 — WebServer polish

`WebServerConfig` already exists; add three fields:

- `ignore_https_errors: bool` — passes through to the wait-for-URL
  health check.
- `graceful_shutdown: { signal: 'SIGINT'|'SIGTERM', timeout: number }`
  — replaces the current hard-kill on cleanup.
- `name: Option<String>` — display name in reporter output for the
  spawned server.

### §7.26 — `captureGitInfo`

- New top-level `TestConfig::capture_git_info: { commit?, branch?,
  diff?, trigger?: 'push'|'pull-request' }` mirroring Playwright.
- Populate by spawning `git rev-parse HEAD`, `git symbolic-ref
  --short HEAD`, etc. at runner start.
- Surface on every `TestResult` via `metadata.git = { ... }`.

### §7.22 (carry-over) — TS Reporter interface

If time permits, ship the bridge in this cluster:

- NAPI `TestRunner.registerJsReporter(reporter)` accepting an
  object whose methods match Playwright's `Reporter` interface.
- Wrap the JS callbacks as a Rust `Reporter` impl.
- TS `defineReporter(impl)` helper for type-safety.
- Bun test that registers a JS reporter, drives the runner, and
  asserts every lifecycle method fired with the right shape.

## Tests (Rule 9)

- `--project foo`: register two projects, run with `--project foo`,
  assert only `foo`'s tests ran.
- `dependencies`: project `B` depends on `A`; if `A` fails, `B` is
  skipped.
- `teardown`: project `T` declared as `teardown` for `B`; runs
  after `B` even if `B` fails.
- `--no-deps`: with the same setup, `A` is skipped when only `B`
  is requested.
- `-x` cross-project: failure in `A` stops the DAG.
- `--only-changed`: write a fixture git repo, change one file,
  assert the other's tests are filtered out.
- `--fail-on-flaky-tests`: register a test that passes on retry,
  assert exit code is 1 with the flag and 0 without.
- WebServer `graceful_shutdown`: spawn a server, close, assert the
  process received the configured signal first.
- `captureGitInfo`: assert the run summary contains commit / branch
  metadata when invoked from a git repo.

## Ground rules (CLAUDE.md)

- Rule 1: project DAG / git-info logic in Rust core; CLI only flags.
- Rule 2: `--project`, `--no-deps`, `--teardown`, `--only-changed`,
  `--fail-on-flaky-tests` match Playwright verbatim.
- Rule 7: rebuild NAPI; diff `index.d.ts` for any new fields on
  `TestRunnerConfig`.
- Rule 9: per-flag integration test (no per-backend matrix needed —
  these are runner-level concerns).

## Baseline (must stay green)

```
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p ferridriver --lib                                 # 125
cargo test -p ferridriver-test --lib                            # 11
cargo test -p ferridriver-script --lib                          # 13
cargo test -p ferridriver-mcp --lib                             # 38
cargo test -p ferridriver-test --test new_features_e2e          # 15
cargo test -p ferridriver-test --test reporters                 # 4
cd crates/ferridriver-node && bun run build:debug && bun test   # 935
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1
# cdp-pipe 175 / cdp-raw 175 / bidi 170 / webkit 171
```

## Prompt for the next session

> Continue ferridriver Playwright parity — Tier 7 cluster 7 (project
> DAG + git-aware filters + WebServer polish + git metadata, §7.1 /
> §7.3 / §7.4 / §7.25 / §7.26). Optionally also tackle §7.22 (TS
> Reporter interface bridge — a 6b follow-up). Read first, in order:
>
> 1. `CLAUDE.md` — rules + lessons.
> 2. `PLAYWRIGHT_COMPAT.md` — §7.1 / §7.3 / §7.4 / §7.22 / §7.25 / §7.26.
> 3. `HANDOVER.md` — Cluster 6 recap.
> 4. `docs/NEXT_SESSION.md` — this file.
> 5. `/tmp/playwright/packages/playwright/src/runner/projectUtils.ts`
>    and `loaderHost.ts`.
> 6. `crates/ferridriver-test/src/runner.rs::run_projects` (existing
>    project DAG implementation) and
>    `crates/ferridriver-test/src/config.rs::WebServerConfig`.
>
> Task: wire the existing project DAG into the CLI surface, add the
> git-aware filter, fail-on-flaky-tests, WebServer polish, and
> captureGitInfo. Optionally also ship the TS Reporter interface
> bridge.
>
> One large cohesive commit (or two if the TS Reporter bridge lands
> alongside).
>
> Rule 9 per flag — see the test list above.
>
> Baseline that must stay green is in HANDOVER.md.
>
> Non-negotiables: project DAG / git-info logic in Rust core; CLI
> only flags; signature shapes match Playwright; rebuild NAPI; no
> shortcuts; no emojis; no AI attribution.
