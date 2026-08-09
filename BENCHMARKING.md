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

## Whole-suite A/B, 2026-08-08 — after context pre-creation

Same machine, same contract as the run above (M3 Pro 12-core, 36GB,
`release-fast` on cdp-pipe vs Playwright 1.62.1, both on
`chrome-headless-shell-1234`, workers=4, `fullyParallel` on both sides,
n=5 interleaved after a warmup). The workloads are generated by
`scripts/bench-workloads.sh`, which builds them so that each differs
from the next by exactly one ingredient — that is what makes the
subtraction below legitimate.

Pre-creation is OFF by default (see "What the default is and why"); the
"after" column is with `contextPrewarm = 2`.

| workload | before | after | Playwright | ratio before | ratio after |
|---|---|---|---|---|---|
| F. 96 tests, no fixtures | 57ms | 61ms | 1008ms | 16.9x | **16.5x** |
| E. 96 tests, `{ page }`, empty body | 1512ms | 1400ms | 2692ms | 1.69x | **1.92x** |
| D. 96 hermetic DOM tests | 2448ms | 1982ms | 3815ms | 1.60x | **1.92x** |
| C. 24 todomvc specs, network | 2549ms | 2513ms | 3717ms | 1.43x | **1.48x** |

Medians; the means track them within 2% on every row (raw samples are
printed by the script). Workload D's ferridriver samples were
[1924, 1949, 1982, 2013, 2026] against Playwright's
[3809, 3811, 3815, 3844, 3866].

### What the per-test cost is actually made of

Two measurements, both reproducible, explain every number above.

**A context plus its first page costs ~42ms, and ~40ms of that is one
renderer process becoming responsive.** A CDP round-trip census of the
`{ page }` fixture path shows 13 commands: `Target.createBrowserContext`
(0.50ms), `Target.createTarget` (1.12ms), then one concurrent flight of
ten session-setup commands whose FIRST response lands 37.4ms later. The
Rust and QuickJS side of that whole path is ~1ms. A lead-time sweep
isolates it: create the target, wait L ms, then time a single
`Page.enable` — L=0 gives 44.6ms, L=25 gives 21.8ms, L=50 gives 1.19ms.
Playwright measures 45.9ms for the same `newPage` on the same binary,
because it is the same renderer.

**The first `setContent` in a renderer costs ~34ms, and it is Blink's
first layout, not ours.** The same call measures 39.35ms, then 1.17ms,
then 0.84ms within one test. Warm the renderer with a throwaway
`<h1>a</h1><input>` first and the real one costs 1.90ms. Split the
hermetic body up and 88% of it is that one-time layout; the remaining
4.68ms is driver-bound, and there ferridriver is 2.6-7x faster than
Playwright — at workers=1, the same body is 7.80ms against Playwright's
22.40ms (**2.87x**).

So the suite ratio is set by two costs that belong to the browser and
are charged to whichever driver touches it first. Both are latency, not
throughput: they can be moved off the critical path, but not removed.

### What was done

`[test] contextPrewarm` has each worker pre-create contexts for its
upcoming tests, so the renderer spawn overlaps the running test.
Isolation is unchanged — every test still gets a context and page no
other test has touched, built from that test's own options — and a
pooled context is unlisted until it is handed over, so a running test
never sees the next test's container in `browser.contexts()`.

Pooled pages are additionally warmed with one throwaway layout, but only
as a background promotion of a spare entry, never the one a waiting test
is about to take. That ordering matters and was measured: warming
eagerly inside pre-creation buys workload D another 180ms (1982 → 1801ms,
**2.19x**) but costs workload E 360ms (1400 → 1761ms, 1.49x), because a
layout is ~18ms of real Blink CPU that a test with an empty body would
never have paid, and spending it anyway starves the pool that test is
waiting on. Promotion keeps the win where a suite renders without
charging the suites that do not.

Pool depth trades memory for wall clock. On workload D at 4 workers:

| contextPrewarm | wall | peak RSS |
|---|---|---|
| 0 | 2452ms | 4.1GB |
| 2 | ~2000ms | — |
| 4 | 1998ms | 5.0GB |
| 12 | 1855ms | 6.6GB |
| 16 | 2288ms | — (thrashes) |

### What the default is and why: OFF

A renderer spawn is CPU-bound, and pre-creating one doubles the live
renderer count. That is affordable only when workers leave cores idle.
The benchmark above runs 4 workers on 12 cores, so it does. This repo's
own suite runs 6 workers over 4 backend projects on the same 12 cores,
and it does not — measured on `ferridriver test`, all 1384 tests:

| contextPrewarm | wall | result |
|---|---|---|
| 0 | 63.8s, 63.7s | 1362 passed, twice |
| 1 | 63.7s, 64.1s | 1362 passed, twice |
| 2 | 61.5s, 63.9s | 1362 passed, then 1361 + 1 failed |
| 4 | 62.2s, 64.5s | 1360 + 2 failed, 1359 + 3 failed |

There is no wall-clock gain to buy the instability with. Isolating
`tests/e2e/events.test.ts` shows what it costs: 10.4/10.5/10.5s and 60/60
three times at `0`, against 14.5-16.7s and 59/60, 57/60, 60/60 at `2`.
Those tests wait on context events with 5s deadlines, and the extra
renderers push delivery past them.

So the default is `0`. `contextPrewarm` is worth setting when a suite
runs fewer workers than the machine has cores to spare, which is the
common case for a single-project suite on a developer machine, and is
exactly the shape of the benchmark workloads above. It is not worth
setting on a box that is already saturated, and the numbers to cite for
a default-configuration run are the "before" column.

### What may be claimed

**~1.9x on a hermetic browser-bound suite at 4 workers with
`contextPrewarm = 2` on a machine with idle cores**; 1.6x on that suite
in the default configuration; 1.5x on a network-bound one; 16.5x on
runner dispatch alone. Not 3x. The gap is
not driver overhead: the runner is 16.5x, the driver-bound part of a
test body is 2.9x, and the Rust/QuickJS cost of acquiring a page is ~1ms
out of 42ms. What is left is a renderer spawn and a first layout, and
Playwright pays both at the same price.

Reaching 3x on this workload requires not spawning a renderer per test
at all — i.e. reusing a page across tests, which trades the isolation
Playwright's model guarantees. Playwright ships that internally
(`browserContext.resetForReuse`, used by its UI mode and VS Code
extension, gated behind a hash of the options that may NOT change) and a
reset measures 3.2ms against 42ms for a fresh context+page. It is not
implemented here, and until it is, 3x is not available without changing
what a test can assume about its own isolation.

### Chromium density flags: a memory lever, not a speed lever

`--in-process-gpu` plus `--enable-features=NetworkServiceInProcess2` folds
the GPU and network services into the browser process. Playwright sets
neither — grepping all of `playwright-core/src/` for them returns nothing —
so this is a deliberate divergence, taken for density rather than fidelity.

Per browser, subtracting this machine's idle baseline (~20 Chrome processes,
3113MB) so only the launched browser is counted:

| | processes | RSS |
|---|---|---|
| Playwright's switch set | 4 | 182MB |
| + both flags | 2 | 110MB |

**-2 processes and -72MB per browser**, about 40% of its footprint. At a
thousand browser-per-session hosts that is 182GB against 110GB.

Fidelity is unchanged, checked through ferridriver's own API rather than
assumed: `page.route` intercepts and fulfils identically (1 hit, status 200,
routed body rendered), WebGL reports the same
`WebGL 1.0 (OpenGL ES 2.0 Chromium) | WebKit WebGL`, and the screenshot is
byte-identical at 6646 bytes. Network interception was the one most likely
to break, since moving the network service in-process is exactly where the
`Fetch.requestPaused` plumbing could shift. It does not.

**It does not make a suite faster.** Measured on this 12-core box:

| workload | baseline | with flags |
|---|---|---|
| 96 hermetic DOM tests, 4 workers, `contextPrewarm = 0` | 2392ms | 2378ms |
| 96 hermetic DOM tests, 4 workers, `contextPrewarm = 2` | 2051ms | 1992ms |
| repo suite, 6 workers, 4 backend projects | 65964ms | 64214ms |

Medians of five for the first two, single run for the suite. Every delta is
inside run-to-run noise, and the suite's peak RSS actually measured *higher*
with the flags (6592MB vs 6719MB) because only the two Chromium projects
carry them and the peak falls elsewhere. The reason is simple: freed memory
only buys wall clock when memory is the binding constraint, and a developer
machine with 36GB running one suite is not memory-bound. A cloud host packing
browsers until it runs out of RAM is.

So this belongs in a deployment's config, not in ferridriver's defaults, and
it needs no new API — `[test.browser] args` (or `launch({ args })`) already
carries it:

```toml
[test.browser]
args = ["--in-process-gpu", "--enable-features=NetworkServiceInProcess2"]
```

The trade is crash isolation: a GPU or network-service crash now takes the
browser process with it instead of being contained. That is acceptable under
browser-per-session, where the browser is already the blast radius, and only
because a dead browser is now evicted and relaunched rather than poisoning
its instance name — before that fix this flag would have converted a
recoverable GPU crash into a permanently dead instance.

### Measured negative results

Recorded so they are not re-investigated:

- **Detaching context teardown is not worth it.** `ctx.close()` measures
  1.4ms and `Target.disposeBrowserContext` 0.88ms. BENCHMARKING.md
  previously suggested teardown might be most of the 15.1ms; it is not.
- **The QuickJS↔Rust boundary is not a cost.** `page.locator()` is
  0.26µs, `page.url()` 0.11µs, an awaited round-trip ~100µs — three to
  four orders of magnitude below one CDP round trip. Optimizing the JS
  engine cannot move the suite number.
- **No fixed delay precedes the first `expect` poll**, and none precedes
  the first actionability attempt (`RETRY_BACKOFFS_MS` starts `[0, 0,…]`).
  Both were suspected; both are clean.
- **Our Chromium switches already match Playwright's `chromiumSwitches`
  exactly**, so launch flags are not a lever (and diverging would make
  the comparison unfair).

### Known-but-not-taken

Both measured, both real, both left alone deliberately:

- `page.setContent` uses 3 CDP round trips where Playwright uses 1 (a
  `Runtime.evaluate('document.readyState')` and an unconditional
  `Page.getFrameTree`). Worth ~0.80ms per call. Not taken because the
  `readyState` probe is what lets an already-complete document pass with
  a tiny timeout, and the `getFrameTree` seeding exists for a recorded
  bug where `FrameAttached` is missed for iframes inserted by
  `setContent` on a never-navigated page; getting either wrong converts
  a fast path into a 30s hang.
- `locator.fill` uses 3 round trips per attempt where Playwright uses 2
  (actionability check and DOM write are separate `callFunctionOn`
  calls). Worth ~0.66ms per fill.

### Where the remaining headroom is

Prewarming is implemented (`[test] contextPrewarm`, above) and takes the
hermetic suite from 1.60x to 1.92x where there are idle cores to run it
on. It cannot take much more:
pre-creation is bounded by how fast the browser will spawn renderers, and
that supply is already the limit. One browser process sustains 46.7ms per
page serially, 13.7ms at 4 concurrent, and 7.8ms at 12 — so a deeper pool
buys throughput until it thrashes, which is the curve in the depth table
and why the default sits at 4.

What is left:

- **Page reuse across tests**, which Playwright ships internally but does
  not expose. It removes the renderer spawn rather than hiding it (3.2ms
  against 42ms) and is the only remaining path to 3x on this workload. It
  trades the isolation guarantee, so it must be opt-in and must mirror
  Playwright's `resetForReuse` semantics exactly — including its rule
  that only `colorScheme`, `forcedColors`, `reducedMotion`, `contrast`,
  `screen`, `userAgent`, `viewport` and `testIdAttributeName` may differ
  between reuses; anything else forces a new context.
- The two round-trip reductions under "Known-but-not-taken", worth
  ~1.5ms per test between them.

Until page reuse lands, "way faster than Playwright" remains true of the
runner (16.5x) and of driver-bound operations (2.9x on a real test body),
and a browser-bound suite may be cited at **1.6x by default, 1.9x with
`contextPrewarm` set on a machine with cores to spare**.
