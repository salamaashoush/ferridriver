# Test runner

Playwright-shaped runner for Rust. Parallel workers, auto-retrying
assertions, DAG-resolved fixtures, hooks, retries with flaky detection,
snapshots, traces, and a wide set of reporters. Runs the same execution
pipeline whether tests are written in Rust or as Gherkin features (with
Rust or JavaScript / TypeScript step bodies).

## Features

- **Parallel execution.** N workers, each with its own browser. MPMC
  work-stealing dispatch. Browsers launch concurrently (`tokio::join!`),
  saving 80–100 ms per extra worker on warm machines.
- **Serial suites.** `SuiteMode::Serial` runs tests in order and skips
  the rest on first failure.
- **Auto-retrying assertions.** `expect()` polls on the Playwright
  schedule (`100, 250, 500, 1000, 1000, ...` ms).
- **Fixtures.** DAG-resolved, scoped (global / worker / test), injected
  via `FixturePool`.
- **Hooks.** `#[before_all]`, `#[after_all]`, `#[before_each]`,
  `#[after_each]`, per-suite per-worker tracking.
- **Retries with flaky detection.** Failed tests re-dispatched; final
  status is `Flaky` if a later attempt passes.
- **Reporters.** terminal, progress, dot, JSON, JUnit, HTML, blob,
  allure, GitHub annotations, rerun, Cucumber Messages, usage,
  cucumber-json, empty — multiplexed via an event bus.
- **Snapshots.** Text `.snap` files with unified diff, plus pixel-diff
  PNG snapshots.
- **Traces.** Playwright-compatible ZIP traces (viewable with
  `npx playwright show-trace`).
- **Filters.** `--grep`, `--grep-invert`, `--tag`, `--shard`,
  `--last-failed`, `--forbid-only`, `--project`.

## Entry points

- [Rust](/test-runner/rust) — `#[ferritest]`, `ferridriver_test::main!()`, `cargo test`-driven.
- [BDD](/bdd/overview) — Gherkin features with Rust or JS / TS step bodies via `ferridriver bdd`.
- [`expect` matchers](/test-runner/expect) — all 38 matchers with auto-retry.
- [Fixtures and hooks](/test-runner/fixtures-and-hooks) — wiring test state.
- [Configuration](/test-runner/config) — `ferridriver.{toml,yaml,json}`, env vars, CLI flags.
