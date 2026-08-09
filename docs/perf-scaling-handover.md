# Performance and scaling: what was done, and how to re-measure it on Linux

Every number below was measured on ONE machine — macOS 15, M3 Pro (12 core),
36GB — and several of them are machine-dependent by construction. Two are
known to be unmeasurable there at all, because the Chromium switch involved
is compiled under `#if BUILDFLAG(IS_LINUX)`.

`BENCHMARKING.md` is the contract: a number not produced on the machine doing
the comparison must not be cited. So treat this document as a set of claims to
re-test, not as results to trust. Where a claim is expected to move on Linux,
it says so and why.

## Re-measuring, in order

```bash
cargo build --profile release-fast --bin ferridriver   # benchmarks
cargo build --bin ferridriver                          # gates and probes
./scripts/bench-workloads.sh                           # generates the spec corpora

# 1. suite A/B against Playwright (needs npx + chrome-headless-shell)
npx playwright install chromium-headless-shell
./scripts/bench-vs-playwright.sh target/bench-src/no-fixture   5 4
./scripts/bench-vs-playwright.sh target/bench-src/page-fixture 5 4
./scripts/bench-vs-playwright.sh target/bench-src/dom          5 4

# 2. teardown: does closing a browser release its connection?
./scripts/probe-fd-leak.sh target/debug/ferridriver 1 5 9

# 3. Chromium flag sets, on the host that will run them
./scripts/bench-chrome-flags.sh target/bench-src/dom 5 4
```

The suite script aborts on a non-zero exit from either runner, so a suite that
silently stopped running cannot be reported as fast. Keep it that way.

Machine state matters more than it looks. Every measurement here was taken with
load average under 5; runs taken while the profiling agents were busy produced
Playwright samples spanning 3936-14163ms on a workload whose real spread is
under 100ms. Check `uptime` before believing anything.

## What landed

Seven commits, each gated (clippy `-D warnings`, workspace tests, cli serial,
`ferridriver test`, bdd, bun, tsc, `just compat`).

| commit | what |
|---|---|
| `f2b620f` | clippy green again — 4 pre-existing lints |
| `b44f471` | `contextPrewarm` context pre-creation, off by default |
| `d9f9a74` | `contexts()` listed closed contexts; `screenshotOnFailure` ignored |
| `e2b1b4c` | fd + task leak on every closed browser |
| `ae5d1ee` | twelve per-context registries never pruned |
| `2124aee` | crashed browser poisoned its instance name forever |
| `d2ed85d` | Chromium density flags recorded as a memory lever, not a speed one |

A note on the first one: `cargo clippy --workspace --all-targets -- -D warnings`
was red at HEAD and looked green, because the shell proxy in use summarises
clippy's output and returns 0. Check the exit code explicitly, not the tail of
the output.

## Claims to re-test, ranked by how likely Linux is to disagree

### 1. `--disable-dev-shm-usage` — unmeasured, Linux-only, possibly the biggest lever

Not tested at all. The switch is declared under `#if BUILDFLAG(IS_LINUX)`, so it
is inert on macOS.

Both Playwright (`chromiumSwitches.ts:66`) and ferridriver
(`crates/ferridriver/src/state.rs`, in `CHROMIUM_SWITCHES`) set it
unconditionally. On Linux it makes `GetShmemTempDir` fall through from
`/dev/shm` (tmpfs, RAM-backed) to `TMPDIR` — real disk. Every anonymous shared
memory region goes with it: renderer/GPU transfer buffers, and the CDP
screenshot and paint buffers.

Playwright sets it because Docker's default `/dev/shm` is 64MB and Chrome
crashes on that. Where `/dev/shm` is sized properly — a plain VM gives you half
of RAM, or `docker run --shm-size=1g` — dropping it should move all of that
back into RAM.

It is a boolean switch with no un-set form, so it cannot be A/B'd through
`args`. Testing it means deleting the line from `CHROMIUM_SWITCHES` and
rebuilding. Worth the rebuild: on a screenshot- or trace-heavy suite this is an
I/O win with no memory cost and no semantic change. Check `df -h /dev/shm`
first — dropping it on a 64MB `/dev/shm` crashes Chrome, which is a hard
failure, not a slow one.

### 2. `--in-process-gpu` + `NetworkServiceInProcess2` — no speedup here, may differ on CI

Measured on macOS, per browser, subtracting this box's idle baseline:

| | processes | RSS |
|---|---|---|
| Playwright's switch set | 4 | 182MB |
| + both flags | 2 | 110MB |

**-2 processes, -72MB per browser** — about 40% of its footprint. Fidelity
verified through ferridriver's own API, not assumed: `page.route` intercepts and
fulfils identically, WebGL reports the same
`WebGL 1.0 (OpenGL ES 2.0 Chromium) | WebKit WebGL`, screenshot byte-identical
at 6646 bytes, suite green at 1366 passed / 22 skipped.

And yet **no wall-clock win**: 2392 -> 2378ms (prewarm 0) and 2051 -> 1992ms
(prewarm 2) on 96 hermetic DOM tests at 4 workers, medians of five, all inside
noise; 65964 -> 64214ms on the repo suite at 6 workers.

The reason is specific and is exactly what should change on CI: freed memory
buys wall clock only when memory is the binding constraint, and a 36GB laptop
running one suite is not memory-bound. A 2-vCPU / 7GB runner is. But the
direction is genuinely uncertain — folding GPU and network into the browser
process puts that work on the same threads, and contention on 2 cores is much
sharper than on 12. Measure it; do not assume it helps.

Playwright sets neither flag anywhere in `playwright-core/src/`. This is a
deliberate divergence for density, and it costs crash isolation: a GPU or
network-service crash now takes the browser with it. That is acceptable under
browser-per-session, and only because `2124aee` means a dead browser is now
relaunched instead of poisoning its instance name.

### 3. `contextPrewarm` — off by default, and the default is a judgement call

Pre-creating the next test's context so the renderer spawn overlaps the running
test. On 96 hermetic DOM tests at 4 workers on 12 cores it took 2452ms to about
2000ms at depth 2 and 1855ms at depth 12, peak RSS 4.1GB to 6.6GB, thrashing
past 16.

It defaults to `0` because it only pays where cores are idle. This repo's own
suite runs 6 workers over 4 backend projects on the same 12 cores, and there it
bought nothing and turned flaky:

| contextPrewarm | wall | result |
|---|---|---|
| 0 | 63.8s, 63.7s | 1362 passed, twice |
| 2 | 61.5s, 63.9s | 1362 passed, then 1361 + 1 failed |
| 4 | 62.2s, 64.5s | 1360 + 2 failed, 1359 + 3 failed |

Isolating `tests/e2e/events.test.ts`: 10.4/10.5/10.5s and 60/60 three times at
`0`, against 14.5-16.7s and 59/60, 57/60, 60/60 at `2`. Those tests wait on
context events with 5s deadlines and the extra renderers push delivery past
them.

Re-derive the default on Linux. The cores-to-workers ratio is the whole
argument, so a machine with a different ratio may well want a different number.

### 4. The suite ratio vs Playwright

At 4 workers, n=5 interleaved, cdp-pipe vs Playwright 1.62.1 on the same
`chrome-headless-shell`:

| workload | before | after | Playwright | ratio after |
|---|---|---|---|---|
| 96 tests, no fixtures | 57ms | 61ms | 1008ms | 16.5x |
| 96 tests, `{ page }`, empty body | 1512ms | 1400ms | 2692ms | 1.92x |
| 96 hermetic DOM tests | 2448ms | 1982ms | 3815ms | 1.92x |
| 24 todomvc specs, network | 2549ms | 2513ms | 3717ms | 1.48x |

The "after" column is with `contextPrewarm = 2`; the default configuration is
the "before" column, i.e. 1.60x on the hermetic suite.

**Not the 3x that was aimed at.** The gap is not driver overhead — the runner is
16.5x and the driver-bound part of a test body is 2.9x. What is left belongs to
the browser and is charged to whichever driver touches it first:

- A context plus its first page costs ~42ms, of which ~40ms is one renderer
  becoming responsive. A lead-time sweep pins it: create the target, wait L ms,
  then time a single `Page.enable` — L=0 gives 44.6ms, L=25 gives 21.8ms, L=50
  gives 1.19ms. Playwright measures 45.9ms for the same `newPage` on the same
  binary.
- The first `setContent` in a renderer costs ~34ms and it is Blink's first
  layout. Same call: 39.35ms, then 1.17ms, then 0.84ms. Warm the renderer with a
  throwaway `<h1>a</h1><input>` and the real one costs 1.90ms.

Both are latency that can be hidden but not removed. Removing them needs page
reuse across tests, which trades the isolation Playwright's model guarantees.
Playwright ships that internally as `BrowserContext.resetForReuse` (used by its
UI mode and VS Code extension) and a reset measures 3.2ms against 42ms. Not
implemented here.

### 5. Teardown leaks — fixed, and the probe should stay flat on Linux

`./scripts/probe-fd-leak.sh` reported 9 / 13 / 17 descriptors for 1 / 5 / 9
closed browsers before, and 9 / 9 / 9 after. Slope 1.0 to 0. At the default 1024
soft limit the old slope was EMFILE after roughly a thousand sessions ever.

Two independent reference cycles pinned the transport and cutting either alone
changed nothing — the first attempt measured no improvement and was reverted
before the second was found:

1. Per-page listener tasks (~10 per page), each holding an `Arc` of the
   transport, parked on transport-wide taps. Released by a new `dispose_local()`
   that does the local half of teardown only — no new protocol round trips,
   because `disposeBrowserContext` already kills the pages browser-side.
2. The two attach-listener tasks, which hold the transport while parked on one
   of its own taps.

The absolute floor (9 here) is platform-specific; the slope is what matters.

## Measured negative results — do not re-investigate

- **Detaching context teardown is not worth it.** `ctx.close()` is 1.4ms and
  `Target.disposeBrowserContext` 0.88ms. An earlier version of BENCHMARKING.md
  suggested teardown might be most of the per-test cost. It is not.
- **The QuickJS/Rust boundary is not a cost.** `page.locator()` ctor 0.26us,
  `page.url()` 0.11us, an awaited round trip ~100us — three to four orders of
  magnitude below one CDP round trip. Optimising the JS engine cannot move the
  suite number.
- **No fixed delay precedes the first `expect` poll**, and none precedes the
  first actionability attempt (`RETRY_BACKOFFS_MS` starts `[0, 0, ...]`).
- **Our Chromium switches already match Playwright's `chromiumSwitches`
  exactly**, so launch flags are not a lever, and diverging makes the comparison
  unfair unless stated.
- **Dead flags**: `--memory-pressure-off` and `--lite-mode` do not exist in
  Chromium 151 — verified absent from the binary's string table while every
  other candidate switch was present. They are propagated by "optimal Chrome
  flags" blog posts.
- **`--disable-site-isolation-trials` is a no-op on chrome-headless-shell**:
  5 pages with cross-origin iframes gave 581.2MB with the flags against 581.4MB
  without, because `ShouldEnableStrictSiteIsolation()` already returns false in
  that binary. **Re-check on Linux** — this was measured on macOS and the
  headless shell's defaults are the reason, not the platform, but it is cheap to
  confirm.
- **`--no-zygote` is a regression on Linux and a no-op on macOS.** The zygote
  amortises dynamic-loader relocations and library init per process. This one is
  Linux-relevant and was reasoned, not measured here.
- **A warm/pre-booted browser pool**: browserless built it in v1 and deprecated
  `PREBOOT_CHROME` / `KEEP_ALIVE` / `CHROME_REFRESH_TIME` outright in v2.
- **Per-session processes**: the Rust process floor is 33.6MB against ~770KB per
  additional live context in one process.

## Still open, highest value first

1. **`Target.detachedFromTarget` / `targetDestroyed` unhandled.** Zero grep hits
   in the tree. A page that closes itself — an OAuth popup finishing its
   redirect — is never marked closed, so it is never reaped, which reintroduces
   the leak `e2b1b4c` fixed, once per popup. I attempted this and backed it out:
   a page-scoped listener on both events did not fire, and the next step is
   checking whether those events reach a browser-session tap at all
   (`tap_event_methods(..., None)` and `matches_session` in
   `backend/cdp/transport.rs`) given `setAutoAttach(flatten: true)`. The test I
   wrote reproduces the bug and is worth recovering from `git show`.
2. **The CDP and BiDi network trackers never delete finished requests** (~4KB
   each; WebKit already does it right). The one-line fix is wrong and provably
   breaks `network_headers`, `network_response_body` and `network_redirect_chain`:
   Chromium often delivers `responseReceivedExtraInfo` AFTER `loadingFinished`,
   so deleting there strands the payload in `pending_*_extra` where nothing
   consumes it. Needs Playwright's separate `_responseExtraInfoTracker`.
3. **`BrowserState`'s write lock is held across a full browser launch**
   (`context.rs`, `new_page` -> `ensure_instance`). One cold start blocks every
   other session's lifecycle operation process-wide. Needs `ensure_instance`
   split into snapshot / launch-off-lock / double-checked insert, with the
   "two concurrent cold starts must not both survive" race handled.
4. **BiDi and WebKit have no liveness signal.** `AnyBrowser::is_alive()` returns
   `true` for both, which is what every backend did before `2124aee`, so it never
   makes a live browser look dead — but it means only CDP recovers from a crash.
5. **Temp directories are not reclaimed on abnormal exit.** `AsyncTempDir::drop`
   defers to `spawn_blocking`, which never runs on SIGKILL/SIGTERM, and
   `run_mcp` installs no signal handler. This box accumulated 525 leftover
   `ferridriver-pipe-*` directories totalling 1.07GB.
6. **A second `--disable-features=` from user args silently replaces
   Playwright's entire 18-feature list**, because Chromium's
   `AppendSwitchNative` does a plain map assignment and the last occurrence
   wins.
7. **`routeFromHAR(update: true)` records a truncated HAR past 1000 requests**:
   `start_len` is an index into the context network log, and `push_capped`
   evicts from the front, shifting every index.

## Architecture, if this is going to run thousands of sessions

The research converged on **browser-per-session**, from three independent
directions:

- Prior art reversed away from pooling. browserless v2 deprecated its
  pre-booting knobs and launches one browser per session. Playwright's own reuse
  mode is `Semaphore(1)` and dev-only.
- Isolation is measured, not theoretical: on a shared browser, one page's
  console flood pushed a sibling page's evaluate p99 from 1ms to 28ms (max
  685ms); on a separate browser the same flood cost the victim nothing. For
  synthetic monitoring the timing IS the product, so a noisy neighbour does not
  slow a check, it corrupts the SLI.
- Every remaining registry and parked task is scoped to a browser's lifetime, so
  a browser that exits after one session bounds all of them to zero.

That shape was ferridriver's worst case until this session's teardown fixes, and
item 3 above is the last structural blocker: as long as a launch holds the
global write lock, sessions serialise on each other's cold starts.
