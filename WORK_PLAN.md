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

- Verified headless gate on the current implementation: 157 checks, zero failed
  or blocked, 107.68 seconds in the gate and 107.813 seconds wall time. This is
  a warm build with 32 browser slots on the 32-CPU Linux host. Logs:
  `target/gate/1789013667-1715415`.
- That run passed 2,219 native E2E tests, 637 BDD scenarios and 464 native
  integration tests, plus Rust and addon checks. The 33 E2E and 19 BDD skips
  predate this work and remain to be audited.
- Last fully passing gate: 159 checks in 129.41 seconds, before subsequent
  concurrency changes. This is historical evidence, not final verification.
- Latest recorded gate: 159 checks in 113.45 seconds with one failure,
  CDP screenshot capture under parallel load. Logs remain under
  `target/gate/1788966313-3016625`.
- That run passed 452 native integration tests, 2,211 E2E tests and 637 BDD
  scenarios. Existing skips still require an audit.
- 70 Rust integration test files and the Bun addon suites remain; migration is
  incomplete. The latest BDD binding and entry-host migrations pass 12 native
  JS cases.
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
