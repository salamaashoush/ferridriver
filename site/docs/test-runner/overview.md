# Test runner

Playwright Test-compatible runner with parallel workers, auto-retrying assertions, fixtures, hooks, and reporters. Runs the same execution pipeline whether tests are written in Rust, TypeScript, or Gherkin.

## Features

- **Parallel execution** — N workers, each with its own browser. MPMC work-stealing dispatch.
- **Serial suites** — `SuiteMode::Serial` runs tests in order and skips remaining tests on failure.
- **Auto-retrying assertions** — `expect()` polls on Playwright's interval schedule (100, 250, 500, 1000 ms).
- **Fixtures** — DAG-resolved, scoped (global / worker / test), dependency-injected via `FixturePool`.
- **Hooks** — `before_all` / `after_all` / `before_each` / `after_each`, per-suite, per-worker tracking.
- **Retries with flaky detection** — failed tests re-dispatched; final status is `Flaky` if a retry passes.
- **Reporters** — terminal, HTML, JSON, JUnit XML, multiplexed via an event bus.
- **Snapshots** — text `.snap` files with unified diff, plus pixel-diff PNG snapshots.
- **Traces** — Playwright-compatible ZIP traces (viewable with `npx playwright show-trace`).
- **Filters** — `--grep`, `--tag`, `--shard`, `--last-failed`, `--forbid-only`.

## Pick a binding

- [Rust](/test-runner/rust) — `#[ferritest]`, `ferridriver_test::main!()`, `cargo test`-driven
- [TypeScript](/test-runner/typescript) — `test()`, `describe()`, `expect()`, Playwright-compatible
- [`expect` matchers](/test-runner/expect) — all 38 matchers with auto-retry
- [Fixtures and hooks](/test-runner/fixtures-and-hooks) — how to wire test state
- [Configuration](/test-runner/config) — `ferridriver.config.*`, env vars, CLI flags
