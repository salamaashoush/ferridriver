# Benchmarking ferridriver

This document is the single source of truth for how ferridriver is benchmarked,
what is actually measured, and what may and may not be claimed. It exists because
the project previously cited a "5x faster than Playwright" figure that could not
be reproduced: the number compared against a hardcoded, self-reported Playwright
baseline (~2200ms) that was never measured on the same machine, and one of the
per-operation comparisons (`click()`) was confounded by passing `force: true` to
Playwright but not to ferridriver.

If a number is not produced by one of the harnesses below, on the machine doing
the comparison, in the same run, it must not be cited.

## Harnesses

There are three independent harnesses measuring three different things.

### 1. Test-runner throughput and parallelism

`crates/ferridriver-test/tests/bench_runner.rs` (run with `cargo test -p
ferridriver-test --test bench_runner -- --ignored --nocapture`).

- Measures: end-to-end wall time of `TestRunner::run` for a synthetic workload
  of N tests (alternating navigation and click-interaction tests against
  `data:` URLs), across worker counts (1/2/4/6) and scales (20/50/100 tests).
- Reports: total time, ms/test, tests/sec, and worker-scaling speedups
  (1->2, 1->4) which are internal and fully reproducible.
- Playwright comparison: NOT asserted by default. The harness only prints a
  speedup ratio when `FERRIDRIVER_PW_BASELINE_MS` is set to a Playwright Test
  number you measured on the same machine with the same 50-test workload.
  Without that env var it prints `Playwright baseline: NOT MEASURED` and
  refuses to print a ratio.

### 2. Per-operation latency vs Playwright

`crates/ferridriver-node/test/benchmark.ts` (run with `bun run
test/benchmark.ts` from `crates/ferridriver-node`).

- Measures: median and mean latency of individual page/locator operations
  across ferridriver backends (`cdp-pipe`, `cdp-raw`, and `webkit` on macOS)
  and Playwright's `chromium`, in the same process, on the same machine.
- Both ferridriver and Playwright run the SAME operation with the SAME flags.
  Action ops (`fill`, `click`, `check`) now go through the locator API with
  `force: true` on BOTH sides so neither side is penalised by actionability
  waits the other side skipped. Mismatched flags here are how the bogus
  `14.42x` click figure was produced; do not reintroduce them.
- Output: a console table plus
  `crates/ferridriver-node/test/benchmark-results.csv` with both median and
  mean columns per backend, so the aggregation is auditable and no single
  statistic can be cherry-picked.

### 3. Whole-suite A/B on identical specs

`scripts/bench-vs-playwright.sh <spec-dir> [runs] [workers]`.

- Measures: wall-clock of a whole `ferridriver test` process against a
  whole `playwright test` process, over the SAME spec files. The Playwright
  compat work (`docs/playwright-compat.md`) is what makes this possible —
  ferridriver runs Playwright-authored specs unmodified, so the two runners
  can be pointed at one directory.
- Both sides are configured BY THE SCRIPT, never by a spec's own
  `playwright.config.ts`: same Chromium binary (Playwright's
  `chrome-headless-shell`, so neither side ships its own), same worker
  count, `fullyParallel` on both, reporters/video/trace off.
- Runs are interleaved after one discarded warmup per side, and a
  non-zero exit on either side aborts — a suite that quietly stopped
  running cannot be reported as fast.

The `fullyParallel` knob is the trap here. Playwright parallelises by
FILE unless it is set; ferridriver parallelises by test. Benchmarking a
single 96-test file without it pins Playwright to one worker and
manufactures a ~4x that evaporates the moment it is turned on. The script
sets it on both sides for that reason.

## Operations measured (per-operation harness)

Navigation: `goto` (network), `setContent`.
Content: `title`, `content`, `innerText('h1')`, `innerHTML('ul')`.
Evaluation: `evaluate('1+1')`, `evaluate` over 50 elements.
Locator: `textContent`, `count`, `isVisible`, `boundingBox`, `allTextContents`.
Actions (force:true, both sides): `fill`, `click`, `check`.
Screenshots: viewport `screenshot()`, `screenshot(fullPage)`.
Viewport: `setViewportSize`.

## Environment

Record these alongside any number you publish; results are meaningless without
them:

- OS and version, CPU model and core count, RAM.
- Chrome/Chromium build and Playwright version.
- ferridriver commit SHA and backend used.
- Whether the machine was otherwise idle (close other browsers; CI runners and
  laptops on battery skew latency badly).

## Aggregation

- Each op runs `WARMUP = 3` discarded iterations then `RUNS = 15` measured
  iterations.
- We report BOTH median (robust to GC/scheduler spikes) and mean (sensitive to
  tail latency). A claim that holds for the median but not the mean, or vice
  versa, must say which one it relies on.
- Iterations that throw are dropped; the recorded sample count is emitted so a
  partially-failing op cannot masquerade as a fast one.

## Pass criteria

- The per-operation harness is informational; it has no hard pass/fail gate.
  Treat a backend as "at parity" on an op when its median is within roughly
  +/-15% of Playwright's median, "faster" when meaningfully below, "slower"
  when meaningfully above. Report the direction honestly per op rather than
  collapsing to one headline multiplier.
- The throughput harness asserts only that all tests pass (`exit_code == 0`)
  and prints internal scaling numbers; it asserts no cross-tool ratio.

## Current honest numbers

These are the directionally-observed results from prior local runs. They are
machine-dependent and are NOT committed as authoritative figures; re-run on your
hardware before citing.

- Aggregate per-operation latency: ferridriver has been observed roughly 2-3x
  faster than Playwright on the content/locator/evaluate ops on a developer
  laptop. This is a range, not a single multiplier, and it varies by op.
- Screenshots: roughly at parity with Playwright (both are dominated by the
  browser's own capture/encode path, which ferridriver does not change).
- Navigation (`goto` over the network): ferridriver has been observed about
  0.74x of Playwright's speed, i.e. SLOWER, because real network time
  dominates and ferridriver's load-state handling adds overhead here. This is a
  known weak spot and must not be hidden behind an aggregate "Nx faster" claim.

## Latest measured run (2026-05-29, Linux, cdp-pipe vs Playwright 1.60 chromium)

Per-operation harness, 15 runs after 3 warmups, both sides `force:true` on
actions. Median latency, cdp-pipe column:

| Operation | Playwright | cdp-pipe | ratio |
|---|---|---|---|
| goto (network) | 20.2ms | 17.1ms | 1.2x |
| setContent | 1.0ms | 1.2ms | 0.9x |
| title() | 0.15ms | 0.07ms | 2.1x |
| innerText('h1') | 0.36ms | 0.12ms | 3.0x |
| evaluate('1+1') | 0.12ms | 0.09ms | 1.3x |
| loc.textContent() | 0.30ms | 0.09ms | 3.3x |
| loc.boundingBox() | 0.66ms | 0.11ms | 6.0x |
| loc.allTextContents() | 0.45ms | 0.13ms | 3.5x |
| fill (force) | 0.85ms | 0.34ms | 2.5x |
| click (force) | 14.5ms | 0.71ms | 20.5x |
| check (force) | 1.18ms | 0.74ms | 1.6x |
| screenshot() | 33.3ms | 33.2ms | 1.0x |
| screenshot(fullPage) | 33.2ms | 33.4ms | 1.0x |
| **TOTAL (sum of medians)** | **107.5ms** | **86.9ms** | **1.2x** |

Reading:

- **Aggregate ~1.2x** on this op mix. The total is dominated by the two 33ms
  screenshots (parity, browser-bound) and the 20ms network goto, so the
  headline multiple is small even though most ops are much faster.
- **Driver-bound DOM/locator reads: 2-6x** (boundingBox 6x, allTextContents
  3.5x, textContent 3.3x, innerText 3x) -- where ferridriver's lower per-call
  overhead shows.
- **click 20.5x** is now a fair comparison (both sides `force:true`); the gap is
  ferridriver's batched single-click fast path (press+release+move in one
  `try_join!`) vs Playwright's per-event dispatch. Not the old `force` confound.
- **Navigation is no longer slower**: this run shows goto at 1.2x faster,
  reversing the previously observed 0.74x. Treat as variance-sensitive; do not
  advertise a navigation speedup without re-confirming.
- **Screenshots at parity** (1.0x) -- Chrome does the encode; ferridriver cannot
  change that.

The strongest, separately-measured win is the test runner: independent projects
now run concurrently (wall-clock ~= slowest project, not the sum).

## What a defensible "5x or more" claim requires

Do not state "5x faster than Playwright" (or any single headline multiplier)
until ALL of the following hold:

1. A Playwright baseline measured on the SAME machine, in the SAME run, on the
   SAME workload (for the throughput harness, via `FERRIDRIVER_PW_BASELINE_MS`;
   for the per-op harness, the in-process Playwright column).
2. Identical flags and actionability behaviour on both sides for every op being
   compared (no force-only-on-one-side confounds).
3. The claimed multiplier reproduced across at least 5 independent runs with
   low variance, reported as median AND mean, with the environment recorded.
4. The claim scoped to the operations where it actually holds. The aggregate
   cannot be advertised as a flat multiplier while navigation is slower and
   screenshots are at parity; either scope the claim ("Nx faster on
   content/locator extraction") or report the per-op breakdown.

Absent those, the honest summary is: faster on most synchronous
content/locator/evaluate operations (~2-3x in local runs), at parity on
screenshots, and slower on network navigation. Ship that, not a round number.

## Whole-suite A/B, 2026-08-08 (macOS, M3 Pro 12-core, 36GB)

`scripts/bench-vs-playwright.sh`, ferridriver 0.5.0 `release-fast` on
cdp-pipe vs Playwright 1.62.1, both driving the same
`chrome-headless-shell-1234`, workers=4, `fullyParallel` on both sides,
n=5 interleaved runs after a warmup. Wall clock of the whole process.

| workload | ferridriver | Playwright | ratio (median) |
|---|---|---|---|
| A. 2 tests (1 no-fixture, 1 page) — startup-dominated | 184ms | 988ms | **5.4x** |
| B. 24 hermetic DOM tests (`setContent`, no network) | 974ms | 2006ms | **2.1x** |
| C. 24 todomvc specs, unmodified corpus (network) | 2549ms | 3639ms | **1.4x** |
| D. 96 hermetic DOM tests (startup amortized) | 3246ms | 4647ms | **1.4x** |
| E. 96 tests, `{ page }` fixture, EMPTY body | 1512ms | 2531ms | **1.7x** |
| F. 96 tests, NO fixtures (pure runner dispatch) | 61ms | 975ms | **16.0x** |

Subtracting the workloads decomposes where the time actually goes
(per test, at 4 workers):

| cost | ferridriver | Playwright | ratio |
|---|---|---|---|
| process startup (F total, ~0 test cost) | 61ms | 975ms | 16.0x |
| context + page per test (E − F) | 15.1ms | 16.2ms | 1.07x |
| the DOM work itself (D − E) | 18.1ms | 22.0ms | 1.22x |

**Read this honestly.** ferridriver's own runner — discovery, dispatch,
fixtures, assertions, reporting — is roughly **16x** faster, because it
is a Rust process with a QuickJS VM per worker rather than a Node process
per worker. Everything downstream of that is the browser, and the browser
does not care who is driving it: per-test context+page creation is at
parity (1.07x) and the DOM work is 1.22x. So the headline for a real
suite lands between 1.4x and 2.1x, and rises the shorter the suite is —
that is startup amortizing, not the driver getting faster.

Do NOT quote 16x as a suite figure, and do not quote the aggregate as a
driver figure.

### Worker scaling, workload D (96 hermetic DOM tests)

| workers | ferridriver | Playwright | ratio |
|---|---|---|---|
| 1 | 8366ms | 13563ms | 1.62x |
| 2 | 5206ms | 8456ms | 1.62x |
| 4 | 3315ms | 5921ms | 1.79x |
| 8 | 2497ms | 5186ms | 2.08x |
| 12 | 2227ms | 5391ms | 2.42x |

ferridriver takes 3.8x from 1 to 12 workers; Playwright takes 2.6x and
then REGRESSES at 12. A ferridriver worker is a thread plus a QuickJS VM;
a Playwright worker is a whole Node process, and on a 12-core box those
processes start competing. The advantage therefore grows with parallelism
— which also means a single-worker comparison understates it and a
many-worker comparison flatters it. State the worker count with the ratio.

### Where the remaining headroom is

Per-test context+page creation is ~15ms (≈45% of a hermetic test) and is
currently at parity with Playwright, because both pay the same CDP
round-trips. It is the only cost big enough to move the suite numbers:

- **Prewarm the next context while the current test runs.** Only valid
  when the next test's creation-time options match (`use` bags vary per
  test, and WebKit latches languages at target spawn), so it needs an
  options-keyed pool, not a blind queue.
- **Page reuse across tests**, which Playwright does not offer. Larger
  win, but it trades isolation for speed and must be opt-in.

Neither is implemented. Until one is, "way faster than Playwright" is
true of the runner and not of a browser-bound suite, and the numbers
above are what may be cited.
