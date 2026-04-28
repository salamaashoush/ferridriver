# Next session — Cluster 6 (reporters, §7.20 – §7.22)

Cluster 5 (locator matcher advanced options) is shipped. Cluster 6
adds the `dot`, `github`, `blob`, and `null` reporters, the
`merge-reports` subcommand for blob shards, and bridges the
TS-authored `Reporter` interface into the Rust event bus.

The user prompt explicitly allows two commits for this cluster:
(a) Rust-side reporters + `merge-reports`; (b) TS Reporter interface
+ event-bus bridge.

## Why §7.20 – §7.22 next

The matcher core (Cluster 4) and locator matcher options (Cluster 5)
are in. Reporters are the natural follow-up because they consume the
matcher events that the runner emits — and the `blob` reporter
unlocks sharded test runs (Tier 7's last big feature) when paired
with `merge-reports`.

## Read-first

1. `CLAUDE.md` — rules + lessons.
2. `PLAYWRIGHT_COMPAT.md` — §7.20 / §7.21 / §7.22.
3. `HANDOVER.md` — Cluster 5 recap.
4. `/tmp/playwright/packages/playwright/src/reporters/` — canonical
   reporter implementations:
   - `dot.ts` — single-character status per test.
   - `github.ts` — wraps another reporter and adds `::error`
     annotations for GitHub Actions.
   - `blob.ts` — emits a `report.zip` per shard; consumed by
     `merge-reports`.
   - `empty.ts` (the `null` reporter — name overloaded to avoid the
     keyword).
   - `merge.ts` — read multiple `report.zip`s and produce a unified
     output.
5. `crates/ferridriver-test/src/reporter/mod.rs::create_reporters` —
   the existing factory; new reporters slot in here.
6. `crates/ferridriver-test/src/reporter/` — terminal, json, junit,
   html, allure, progress, rerun, bdd implementations as references
   for the trait shape (`Reporter::on_event` + `finalize`).

## Cluster scope

### §7.20 — built-in reporters

`dot`, `github`, `blob`, `null` (renamed `empty` internally to avoid
clashing with the JS keyword). All consume the existing
`ReporterEvent` enum.

- `dot`: print `.` for pass, `F` for fail, `S` for skip, `T` for
  timeout; line-wrap at 80.
- `github`: wraps the configured fallback reporter and additionally
  emits `::error file=...,line=...::message` for each failure when
  `process.env.GITHUB_ACTIONS` is set.
- `blob`: write a `report.zip` containing every event as JSON-lines
  plus attachment files. The shard index goes in the filename.
- `null` / `empty`: drop every event. Useful for soft-fail
  scenarios.

### §7.21 — `merge-reports`

New CLI subcommand `ferridriver-test merge-reports [dir]`. Takes a
directory of blob `*.zip` files, parses each into events, merges
them into a unified plan + outcome stream, and runs the configured
reporter against the merged stream.

Wires through the existing `EventBus` so the merge can drive any
reporter (terminal/json/html/etc.) — not just one fixed format.

### §7.22 — TS Reporter interface

Allow user-authored TS reporters with the Playwright signature:
`onBegin(config, suite)`, `onTestBegin`, `onStepBegin`, `onStepEnd`,
`onTestEnd`, `onEnd(result)`, `onError(error)`, `onStdOut(chunk,
test, result)`, `onStdErr(...)`, `onExit()`,
`printsToStdio(): boolean`.

Bridge approach: NAPI exposes a `registerJsReporter(reporter)`
method. Inside, the Rust side wraps the JS object as a
`ReporterDriver` adapter that translates each `ReporterEvent` into a
TS callback invocation. The TS surface in `expect.ts` /
`test-runner` exposes a `defineReporter(impl)` helper.

## Tests (Rule 9)

- `dot`: register a fake test, assert stdout matches the dot
  pattern.
- `github`: simulate `GITHUB_ACTIONS=1` and assert `::error` lines
  appear for failures.
- `blob`: run a small plan and assert the resulting zip contains
  the expected JSONL events.
- `null`: assert no output is produced.
- `merge-reports`: write two blob zips by hand, run merge, assert
  the unified output contains tests from both.
- TS Reporter interface: register a JS reporter that records every
  event into an array; run a small plan and assert
  `onTestBegin`/`onTestEnd` were invoked with the right tests.

## Ground rules (CLAUDE.md)

- Rule 1: each Rust reporter implements the `Reporter` trait; no
  reporter logic in TS / NAPI.
- Rule 2: TS-Reporter interface signatures match Playwright's
  `Reporter` type verbatim.
- Rule 7: rebuild NAPI and diff `index.d.ts` for the new
  `registerJsReporter` shape.
- Rule 9: per-reporter integration test asserting the output stream.

## Baseline (must stay green)

```
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p ferridriver --lib                                 # 125
cargo test -p ferridriver-test --lib                            # 11
cargo test -p ferridriver-script --lib                          # 13
cargo test -p ferridriver-mcp --lib                             # 38
cargo test -p ferridriver-test --test new_features_e2e          # 15
cd crates/ferridriver-node && bun run build:debug && bun test   # 933
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1
# cdp-pipe 175 / cdp-raw 175 / bidi 170 / webkit 171
```

## Prompt for the next session

> Continue ferridriver Playwright parity — Tier 7 cluster 6
> (reporters: dot, github, blob, null + merge-reports + TS Reporter
> interface, §7.20 – §7.22). Read first, in order:
>
> 1. `CLAUDE.md` — rules + lessons.
> 2. `PLAYWRIGHT_COMPAT.md` — §7.20 – §7.22.
> 3. `HANDOVER.md` — Cluster 5 recap.
> 4. `docs/NEXT_SESSION.md` — this file.
> 5. `/tmp/playwright/packages/playwright/src/reporters/` —
>    canonical Rust-side reporter implementations.
> 6. `crates/ferridriver-test/src/reporter/` for the trait shape and
>    `create_reporters` factory.
>
> Task: implement the four built-in reporters in
> `crates/ferridriver-test/src/reporter/`, add the `merge-reports`
> subcommand, and bridge a TS-authored Reporter interface through
> NAPI into the Rust event bus.
>
> Two commits acceptable: (a) Rust reporters + `merge-reports`;
> (b) TS Reporter interface + bridge.
>
> Rule 9 per reporter (output assertion) + per merge-reports run +
> per TS-bridge invocation.
>
> Baseline that must stay green is in HANDOVER.md.
>
> Non-negotiables: reporter logic in Rust core; TS bridge is a thin
> adapter; signature shapes match Playwright; rebuild NAPI and diff
> index.d.ts; no shortcuts; no emojis; no AI attribution.
