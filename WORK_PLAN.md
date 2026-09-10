# Gate and automation work

## Delivery order

1. Finish the headless gate: reproduce and fix parallel screenshot failures,
   measure worker allocations, preserve process cleanup and failure reporting.
2. Complete integration, E2E and MCP migrations to the native JS/TS runner.
   Preserve assertions and addon ABI coverage; remove readiness sleeps through
   observable lifecycle signals. Strengthen real application BDD scenarios.
3. Verify the final gate, dependency reproducibility and publication checks.
   Commit whole files in logical groups and push verified changes.
4. Audit protocol transports and architecture against their implementations.
   Measure latency, throughput, memory and cancellation before optimizing.
5. Extend existing scripting capabilities for performance analysis, WebMCP and
   browser extensions. Keep protocol details behind the established API.
6. Add browser automation on iOS simulators and Android emulators through
   Safari, WebDriver and Appium, including device startup and owned cleanup.
   Verify on available devices and record any missing platform access.
7. Benchmark equivalent workloads against Playwright, Chrome DevTools tooling
   and agent-browser before making comparative performance claims.

## Current evidence

- Verified headless gate at committed checkpoint `63641057`: 157 checks, zero failed
  or blocked, 107.68 seconds in the gate and 107.813 seconds wall time. This is
  a warm build with 32 browser slots on the 32-CPU Linux host. Logs:
  `target/gate/1789013667-1715415`.
- That run passed 2,219 native E2E tests, 637 BDD scenarios and 464 native
  integration tests, plus Rust and addon checks. The 33 E2E and 19 BDD skips
  predate this work and remain to be audited.
- Last fully passing gate: 159 checks in 129.41 seconds, before subsequent
  concurrency changes. This is historical evidence, not final verification.
- Historical diagnostic gate: 159 checks in 113.45 seconds with one failure,
  CDP screenshot capture under parallel load. Logs remain under
  `target/gate/1788966313-3016625`.
- That run passed 452 native integration tests, 2,211 E2E tests and 637 BDD
  scenarios. Existing skips still require an audit.
- 68 Rust integration test files and the Bun addon suites remain; migration is
  incomplete. BDD lifecycle coverage now runs 15 native JS cases in 97 ms with
  eight workers, preserving fixture, attachment, timeout and skip assertions.
  The probe uses the production runner bridge; no browser is requested.
- Ferrijs passed its full workspace gate and was pushed as
  `144f8ef04dbfe9ca83f0e669c5eb033c05133945`. Cargo pins that Git revision.
- UI reporter events and replies now share one FIFO. The original UI test
  failed in 3 of 16 repetitions before the fix and passed 32 afterwards.
- Page screenshot rendering synchronization passed 128 concurrent addon test
  runs. Element capture bypassed it; all three CDP capture paths now share the
  same implementation, verified by the full gate and eight native first-capture
  cases covering both page and locator pixels across all four backends.
- CI invokes the headless gate and rejects failed, cancelled or skipped required
  jobs. Previously its conclusion shell command could return success on failure.

## Remaining gate bottlenecks

- The addon build uses a separate Cargo target directory and repeats shared
  compilation.
- The Rust UI integration invokes Cargo with a narrower package selection,
  recompiling dependencies already built by the workspace gate.
- Compare warm runs with different worker budgets after rendering fixes pass.
  The pinned dependency's first build is not a warm timing.
- A 64-slot experiment took 132.036 seconds wall time and failed three suite
  checks with timeouts and event-ordering errors. It is not the default. One
  sample used about 10.5 GiB proportional memory with ample host memory free;
  investigate protocol and runtime coordination before raising concurrency.

## Verification requirements

No weakened assertions, new skips or success caches. The full gate must run
headlessly and return zero on the final source state. Report build state,
worker allocation, test counts and wall time with each benchmark. Capability
completion requires observable behavior through the public scripting API.

## In-progress verification

- BDD fixture and attachment lifecycle migrations replace two Rust targets with
  15 native JS cases. Their standalone run passed in 97 ms with eight workers.
  Clean committed originals were copied and byte-verified before removal at
  `/tmp/ferridriver-bdd-lifecycle-backup-pjpa84kn`.
- The first subsequent gate exposed an ephemeral-port reuse race in a config
  unit test. Three dead-endpoint fixtures now retain a bound, non-listening
  socket; their rejection assertions are unchanged.
- CDP frame state now updates in wire order before navigation completion. New
  coverage checks 256 navigations across four backends, including fragments.
  The existing navigation-listener test exposed queued events reaching listeners
  registered after the originating event. Emitters now preserve an event's
  registration boundary, including page-to-context forwarding. Both ordering
  tests pass on all four backends, and the transport/emitter unit regressions
  pass.
- The completed event-order gate logs are `target/gate/1789015448-2276000`, console at
  `/tmp/ferridriver-event-order-ready.log`. It passed 479 native integrations,
  637 BDD scenarios and all four URL/listener ordering cases, but E2E reported
  one Firefox `context_set_http_credentials` failure: expected 200, received
  401. This is unresolved; do not claim the gate is green.
- That authentication failure did not recur in 24 isolated original-test runs,
  128 concurrent-context authentications, or a full Firefox project run with
  556 passing tests and eight existing skips. Diagnostic logs are under
  `/tmp/ferridriver-bidi-auth-repro`, `/tmp/ferridriver-auth-stress.log` and
  `/tmp/ferridriver-bidi-project-auth.log`.
- Rebuilding after transport changes took 165.08 seconds for workspace binaries;
  the Rust UI test rebuilt its narrower Cargo graph and took 167.90 seconds.
  The addon then rebuilds core separately. These are rebuild costs, not warm
  gate timings, and remain major productivity bottlenecks.
- Remote CI for `63641057` failed documentation links/HTML, spelling, and the
  `rsa` advisory RUSTSEC-2023-0071. Logs: `/tmp/ferridriver-ci-63641057.log`.
  Documentation and spelling failures are now corrected locally. The security
  advisory remains unresolved; no advisory or failing assertion was disabled.

- Ready now checks format, lint and strict Rustdoc before building binaries.
  Native gate tests cover lint/docs failure blocking builds, strict warnings,
  and transitive dependency failure propagation. The scheduler now propagates
  failed dependencies to a fixed point instead of misreporting a cycle.
- Strict workspace Rustdoc passed with `--keep-going` after correcting stale
  links and disabling the CLI binary documentation that collided with the core
  library output. The gate uses `--keep-going` to report all documentation
  failures in one pass. The full repository passed CI's typos 1.50.1 checker.
- Full gate `target/gate/1789017187-2522991` finished with exit 1:
  156 checks, one failed, 146.96 gate seconds / 147.168 wall seconds.
  Console: `/tmp/ferridriver-early-checks-ready6.log`. Integration passed in
  57.82 seconds, BDD 637 passed in 98.56 seconds, E2E 2222 passed and one
  WebKit navigation stress case timed out at 30 seconds. Firefox authentication
  passed on this run. Builds: workspace 16.85, Rust tests 53.72, addon 13.53
  seconds. This is a rebuild run with BiDi trace diagnostics, not a warm benchmark.
- Local NAPI CLI source confirms every build passes `--target <host-triple>`;
  workspace builds use the implicit host target. The separate Cargo target
  graph explains duplicate compilation. Rust UI also invokes a package-scoped
  Cargo build through the actual product path. Neither duplication is fixed yet.

- WebKit navigation stress passed alone in 1.5 seconds and in 12 unchanged
  runs across four concurrent processes (768 navigations). Logs:
  `/tmp/ferridriver-webkit-navigation-repro.log` and
  `/tmp/ferridriver-webkit-navigation-stress`. The full-gate timeout is not
  reproduced deterministically yet.
- Inspection found a lost-notification window in WebKit's lifecycle wait loop:
  state was checked before constructing `Notify::notified()`. The future now
  precedes the state checks, consistent with Tokio's guarantee that a created
  future receives `notify_waiters()` even before polling. This fixes a real
  race, but causation of the observed timeout is not yet established.
- Verification after the WebKit wakeup fix passed: 156 checks, zero failures,
  163.52 gate / 163.644 wall seconds, without diagnostic tracing. Logs:
  `target/gate/1789017415-2744486`, `/tmp/ferridriver-webkit-wakeup-ready.log`.
  Counts: 2223 E2E, 637 BDD, 482 native integration tests passed, existing skips
  unchanged. WebKit navigation stress passed in 2.5 seconds.
- Verified local commits: `699d0165` preserves navigation/event ordering and
  fixes the WebKit wait race; `12894ad1` migrates BDD lifecycle coverage and
  fixes dead-endpoint fixture port ownership. No push yet.
- The following warm gate failed one existing assertion: Firefox
  `to_pass_timeout_and_intervals` made four attempts in 400 ms, while the
  assertion requires at least five. Other counts: 2222 E2E passed, 637 BDD,
  482 integration. Total 114.33 gate / 114.427 wall seconds. Logs:
  `target/gate/1789017611-2952824`, `/tmp/ferridriver-warm-order-ready.log`.
  WebKit navigation stress passed in 2.3 seconds. The 114.427-second run is
  not a passing benchmark, and the retry-count assertion is unchanged.
- The native browser-free 256-case scheduling reproduction passed with
  32 workers in 3.2 seconds; `/tmp/ferridriver-to-pass-stress.log`.
  Source: `/tmp/ferridriver-to-pass-stress.test.ts`. It records attempt
  timestamps on failure. The broader gate load is needed to reproduce the
  observed failure; no scheduling behavior has been changed speculatively.
- The 16-browser-slot warm gate passed all 156 checks in 117.18 gate /
  117.312 wall seconds. Logs: `target/gate/1789017838-3136616`, console
  `/tmp/ferridriver-warm-order-ready16.log`. Integration took 63.65 seconds,
  BDD 89.62, E2E 97.24. This is the current passing warm benchmark. The
  default remains 32; the failed 32-slot timing does not prove reliable speed.
- The early-check gate and documentation fixes have passed complete gates at
  both 32 slots (with rebuilds) and 16 slots (warm), plus spelling and leak
  checks. They are ready for the verified checkpoint.
- Browser-budget audit found two unnecessary global barriers:
  `parallel_projects` has empty fixture requests throughout and launches no
  browser; `screenshot_diff` runs serialized and launches one browser at a
  time. The gate currently reserves the entire budget for each because both
  belong to `ferridriver-test`. Correcting these reservations is next.
- Revalidated inventory: 68 top-level Rust integration targets and 50 addon
  suites still require migration. Protocol and scripting capability work remains
  open. Fetch confirmed `origin/main` had not advanced beyond `63641057`.

- Pushed verified checkpoint commits `699d0165`, `12894ad1`, and `a1a75ea2`
  to `origin/main`. The last includes early lint/docs checks and their native
  gate regressions. Security advisory remediation and remaining migration work
  are still open.
- Browser accounting now reserves zero slots for `parallel_projects` and one
  for the serialized `screenshot_diff` target. All other reservations retain
  their previous bounds. These are scheduling changes, with no test/assertion
  changes. Full default 32-slot verification passed all 156 checks in 103.58
  gate seconds; logs `target/gate/1789018015-3312688`, console
  `/tmp/ferridriver-budget-ready32.log`. The scheduler target passed
  concurrently in 1.88 seconds. This improves the passing 16-slot warm run
  by approximately 11.6%; both runs executed the same test coverage.
