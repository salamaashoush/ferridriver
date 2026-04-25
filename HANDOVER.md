# Handover — next Playwright-parity session

Read-first for any session continuing work. Overwrite this file with a
fresh summary at the end of each block.

## Cross-device setup

1. `CLAUDE.md` — rules + lessons.
2. `PLAYWRIGHT_COMPAT.md` — gap tracker. Tier 1 done. Tier 2.x and 4.x
   incremental wins through §2.15 BrowserType. Tier 7 (test runner)
   underway: §7.2 / §7.5 / §7.6 / §7.8 / §7.9 / §7.27 / §7.28 shipped
   (Cluster 1).
3. This file — block summary below.
4. `docs/NEXT_SESSION.md` — next-cluster brief + prompt.

`git clone https://github.com/microsoft/playwright /tmp/playwright` if missing.

## Landed this session — Cluster 1 (CLI flag surfacing)

Single commit. Mechanical CLI surface for the §7.2/§7.5/§7.6/§7.8/§7.9/
§7.27/§7.28 group plus a real fix for the underlying `--max-failures`
behavior (workers used to drain the queue past the threshold).

### New top-level config fields

`crates/ferridriver-test/src/config.rs::TestConfig` gained:

| field | type | Playwright equivalent |
|---|---|---|
| `global_timeout` | `u64` (ms, 0 = unlimited) | `globalTimeout` / `--global-timeout` |
| `ignore_snapshots` | `bool` | `ignoreSnapshots` / `--ignore-snapshots` |
| `pass_with_no_tests` | `bool` | `--pass-with-no-tests` |
| `tsconfig` | `Option<String>` | top-level `tsconfig` / `--tsconfig` |
| `name` | `Option<String>` | top-level `name` |

`CliOverrides` mirror plus `max_failures`, `repeat_each`, `fail_fast`,
`fully_parallel`, `update_snapshots`. `parse_common_cli_args` now
recognises `--max-failures`, `--repeat-each`, `--global-timeout`, `-x`,
`--pass-with-no-tests`, `--ignore-snapshots`, `--tsconfig`,
`--fully-parallel`, and `-u [all|changed|missing|none]` (with the
optional value matching Playwright's `preset = 'changed'`).

### Runtime effects

- `global_timeout` is enforced inside `runner::TestRunner::run` via
  `tokio::time::timeout`. The whole project / single-project pipeline
  is wrapped; on expiry the runner logs and returns exit code 1.
- `ignore_snapshots` propagates to `model::TestInfo::ignore_snapshots`
  (set by the worker) and short-circuits the text path
  `crate::snapshot::assert_snapshot`. The screenshot path
  (`compare_screenshot_png`) lands with §7.17 since the matcher needs
  `TestInfo` plumbing as part of the `toHaveScreenshot` rewrite.
- `dispatcher::Dispatcher` gained an `Arc<AtomicBool>` `stopped` flag
  and a new `stop()` method; the worker loop checks the flag after
  `recv()` and breaks before processing dropped items, plus calls
  `tokio::task::yield_now()` after each result-send so the runner can
  observe the result and trip the flag before the worker races to the
  next item. `runner` now uses `dispatcher.stop()` (not `close()`)
  when `--max-failures`/`-x` fires.

### NAPI surface

`crates/ferridriver-node/src/test_runner.rs::TestRunnerConfig`
gained: `maxFailures`, `repeatEach`, `failFast`, `globalTimeout`,
`ignoreSnapshots`, `passWithNoTests`, `tsconfig`, `name`,
`fullyParallel`, `updateSnapshots` (string union mode). Accessors:
`get_name`, `get_tsconfig`, `get_ignore_snapshots`,
`get_pass_with_no_tests`, `get_global_timeout`, `get_max_failures`,
`get_repeat_each`, `get_fail_fast`. Generated `index.d.ts` matches
Playwright's `PlaywrightTestConfig` field names.

### TS surface

`packages/ferridriver-test/src/cli.ts` exposes:
`--max-failures <N>`, `--repeat-each <N>`, `-x`, `--pass-with-no-tests`,
`--ignore-snapshots`, `--tsconfig <PATH>`, `--global-timeout <MS>`,
`--name <NAME>`, `--fully-parallel`, plus `-u`/`--update-snapshots`
upgraded to accept the optional `[mode]` value. `mergeConfig` reads the
matching fields from the config file. The TS loader rebuilds `jiti`
with the user-supplied tsconfig in Node mode; under Bun the runtime
reads its own `tsconfig.json` and the loader prints a one-time warning
when the override is set (no programmatic Bun override exists).

`config.ts`: `FerridriverTestConfig` gained `tsconfig?: string`,
`ignoreSnapshots?: boolean`, `passWithNoTests?: boolean`. `name?` and
the rest already existed.

### Pass-with-no-tests semantics

Both no-test exit paths in `cli.ts` (`testFiles.length === 0` and the
post-discovery `tests.length === 0`) now exit `1` by default and `0`
when `passWithNoTests` is set. Previously they exited `0`
unconditionally — a parity bug that meant `forbid-only` style CI
gates couldn't distinguish "no tests selected" from "all tests
passed".

### Tests (Rule 9)

`crates/ferridriver-node/test/cli-flags.test.ts` — 11 cases driven via
the `TestRunner` NAPI surface, asserting page-visible / runner-visible
effects:

- `maxFailures: 2` over 4 failing tests → exactly 2 failures recorded.
- `failFast` over 1 failing + 2 passing → exactly 1 result.
- `repeatEach: 3` over 1 test → callback invoked 3×.
- `globalTimeout: 100` against a 1500ms test body → returns under 1s
  with exit code 1.
- Config getters reflect each input value (`getName`, `getTsconfig`,
  `getIgnoreSnapshots`, `getPassWithNoTests`, `getGlobalTimeout`,
  `getMaxFailures`, `getRepeatEach`, `getFailFast`).

### Baseline (must stay green)

```
cargo clippy --workspace --all-targets -- -D warnings            # clean
cargo test -p ferridriver --lib                                   # 125 pass
cargo test -p ferridriver-test --lib                              # 11 pass
cargo test -p ferridriver-script --lib                            # 13 pass
cargo test -p ferridriver-mcp --lib                               # 38 pass
cargo test -p ferridriver-test --test new_features_e2e            # 14 pass
cd crates/ferridriver-node && bun run build:debug
cd <repo root> && bun test                                        # 883 pass
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1
# previously 164 cdp-pipe / 164 cdp-raw / 159 bidi / 161 webkit
```

## Open clusters (in order)

| # | scope | status |
|---|---|---|
| 1 | CLI flag surfacing (§7.2/§7.5/§7.6/§7.8/§7.9/§7.27/§7.28) | DONE |
| 2 | Built-in fixtures (`browserName`, `browserVersion`, `playwright`, `request`); `auto: true` enforcement (§7.18 / §7.19) | next |
| 3 | TestInfo helpers (§7.10) | pending |
| 4 | Generic + asymmetric matchers, `.resolves` / `.rejects`, `.soft` / `.poll`, `expect.extend`, `toBeOK` (§7.11 – §7.16) | pending |
| 5 | Locator matcher advanced options (§7.17, includes `compare_screenshot_png` ignore_snapshots wiring) | pending |
| 6 | Reporters (`dot`, `github`, `blob`, `null`) + `merge-reports` + TS Reporter interface (§7.20 – §7.22) | pending |
| 7 | Project DAG + git-aware filters + WebServer polish + git metadata (§7.1 / §7.3 / §7.4 / §7.25 / §7.26) | pending |

`§7.7 --ui mode`, `§7.23` and `§7.24` (CT adapters / mount API) plus
the Tier 8 CLI subcommands are deferred — they ride alongside their
companion subsystem work (HAR / Tracing / CT bring-up).

## Carried-forward backend gaps (real protocol limits)

- **BiDi**: response body unavailable for non-intercepted responses;
  multi-`Set-Cookie` collapses; `request.postData()` null for
  fetch-with-body; `Download.cancel` typed `Unsupported`; spurious
  page-init `"Permission denied"` cross-origin error; `userAgent`,
  media overrides, geolocation+permissions, `setNetworkConditions`
  shape — Firefox BiDi protocol gaps.
- **WebKit** (stock `WKWebView`): no public API for main-doc
  Response, redirect chain, response body bytes, browser-set request
  headers, `Set-Cookie`, WebSocket frames, dialog intercept,
  download intercept, console args+location, WebError stack frames,
  screencast, multiple browser contexts.

## Key source locations (Cluster 1)

| area | path |
|---|---|
| `TestConfig` field additions | `crates/ferridriver-test/src/config.rs` |
| `CliOverrides` + `parse_common_cli_args` | `crates/ferridriver-test/src/config.rs` |
| `runner::TestRunner::run` global timeout | `crates/ferridriver-test/src/runner.rs` |
| `Dispatcher::stop()` + `stop_flag()` | `crates/ferridriver-test/src/dispatcher.rs` |
| Worker stop check + yield | `crates/ferridriver-test/src/worker.rs` |
| `TestInfo::ignore_snapshots` propagation | `crates/ferridriver-test/src/model.rs`, `worker.rs`, `expect/locator.rs`, `tests/new_features_e2e.rs` |
| `assert_snapshot` ignore short-circuit | `crates/ferridriver-test/src/snapshot.rs` |
| NAPI `TestRunnerConfig` + getters | `crates/ferridriver-node/src/test_runner.rs` |
| TS CLI flags + `_configureTsLoader` + pass-with-no-tests | `packages/ferridriver-test/src/cli.ts` |
| TS config types | `packages/ferridriver-test/src/config.ts` |
| Rule 9 tests | `crates/ferridriver-node/test/cli-flags.test.ts` |
| Compat tracker updates | `PLAYWRIGHT_COMPAT.md` (§7.2/§7.5/§7.6/§7.8/§7.9/§7.27/§7.28) |
