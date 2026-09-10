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

- Latest direct-session migration headless gate passed: 119 checks, zero
  failures or blocks, 115.06s gate time. All 772 native integration cases
  passed in 59.6s. Logs: target/gate/1789041208-1164580 and
  /tmp/ferridriver-session-semantics-ready-final.log. This is an incremental
  observation, not a controlled before/after benchmark. Remaining top-level
  Rust test files: 30; session.rs retains sixteen extension cases. Addon
  migration and capability implementation remain open.
- Verified headless gate at committed checkpoint `63641057`: 157 checks, zero failed
  or blocked, 107.68 seconds in the gate and 107.813 seconds wall time. This is
  a warm build with 32 browser slots on the 32-CPU Linux host. Logs:
  `target/gate/1789013667-1715415`.
- That run passed 2,219 native E2E tests, 637 BDD scenarios and 464 native
  integration tests, plus Rust and addon checks. The 33 E2E and 19 BDD skips
  predate this work and remain to be audited.
- Earlier passing gate: 159 checks in 129.41 seconds, before subsequent
  concurrency changes. This is historical evidence, not final verification.
- Historical diagnostic gate: 159 checks in 113.45 seconds with one failure,
  CDP screenshot capture under parallel load. Logs remain under
  `target/gate/1788966313-3016625`.
- That run passed 452 native integration tests, 2,211 E2E tests and 637 BDD
  scenarios. Existing skips still require an audit.
- The initial inventory had 68 Rust integration test files plus Bun addon
  suites. BDD lifecycle coverage runs 15 native JS cases in 97 ms with
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

- The addon build now reuses host-target workspace artifacts through the
  pinned NAPI CLI source patch; the latest gate's addon build took 8.55s.
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

- Pushed `059f59d1` with the verified browser reservation correction. The
  passing 32-slot run used 103.820 wall seconds (103.58 gate seconds).
- The three Rust `parallel_projects` tests are now native JS cases in the
  existing worker-budget suite. Original timing bounds are preserved; new
  interval assertions also prove all independent projects overlap, capped
  projects serialize, and dependencies finish before dependents start.
  Seven native cases passed in 1.9 seconds with four workers.
- The removed Rust file was unchanged from HEAD (5702 bytes); a byte-verified
  backup remains at `/tmp/ferridriver-project-scheduler-backup-77x37vql`.
  Its now-unused gate reservation exception was removed. Remaining top-level
  Rust integration targets: 67; addon migration remains open.
- Full default gate for this migration finished with one E2E failure in
  103.913 wall seconds (103.66 gate): the recurring Firefox authentication
  case returned 401 instead of 200. All 485 native integrations passed,
  including the three scheduler migrations. Logs:
  `target/gate/1789018215-3497369`, `/tmp/ferridriver-native-scheduler-ready.log`.
  The migration is not committed yet. No assertion was weakened.
- Authentication event-order capture is running in exec session 80824,
  logs `target/gate/1789018375-3681782`, console
  `/tmp/ferridriver-auth-order-ready.log`. It enables BiDi transport metadata
  tracing and auth-action diagnostics to distinguish response ordering from
  credential dispatch; no credentials are logged.

- The protocol-traced gate passed all 155 checks in 100.627 wall seconds
  (100.53 gate). All 485 native integrations passed, including scheduler
  migration. Logs: `target/gate/1789018375-3681782`.
- Trace evidence: Firefox emits responseStarted 401, authRequired, a second
  beforeRequestSent with the same request ID, responseStarted 200, completed
  200, then navigationCommitted/DOMContentLoaded/load. The transport dispatches
  command replies independently from the page's queued event consumer.
  `BidiPage::goto` currently reads NavRequestSlot immediately after the command
  reply, so it can observe the first response before the consumer processes
  the retry. A navigation-specific consumer barrier is the next correction
  to investigate and verify, rather than sleeps or status-based retries.

- BiDi navigation now waits for an acknowledgement on its existing ordered
  event tap before reading the navigation response. `goto`, reload and history
  traversal use the original navigation deadline for the wait. The marker is
  acknowledged when the consumer asks for its next event, after processing
  earlier events; it does not retry HTTP statuses or use a readiness sleep.
- New transport unit cases pin response ordering (401 then 200) and consumer
  closure. Exposed bindings now run in tracked tasks instead of blocking the
  event consumer, preventing reentrant navigation from waiting on itself.
  Finished task handles are pruned when tracking another callback.
- A new four-backend native case navigates from an exposed binding and checks
  URL and rendered content. Its old-binary diagnostic passed in 1.4 seconds
  (`/tmp/ferridriver-binding-navigation-before.log`), before the new binary
  build. It protects against a deadlock introduced by queue synchronization;
  it is not evidence reproducing the original authentication failure.
- The first verification run was intentionally interrupted during lint after
  identifying the binding deadlock risk (94.260 wall seconds, not a benchmark).
  Current full verification: exec session 64649, logs
  `target/gate/1789018885-3865455`, `/tmp/ferridriver-bidi-barrier-ready2.log`.
  Format, type checks and lint passed; strict documentation is running.
  These changes are not committed yet.

- The first barrier build passed lint/docs but the full suite caught reordered
  binding invocations after callbacks were independently spawned. Its E2E run
  also reproduced the pre-existing `toPass` attempt-count failure under load.
  That gate was intentionally interrupted after confirmed failures, at 355.895
  wall seconds, while an addon rebuild was running. It is not a benchmark.
- Binding callbacks now run on one ordered queue per page, separate from the
  protocol event consumer. This preserves existing invocation order while
  allowing callbacks to navigate and await the event acknowledgement. The
  worker is tracked for page teardown; no per-call task handles accumulate.
- Before that correction, authentication and navigation-from-binding passed on
  BiDi in the full suite (301 ms and 315 ms). This is partial evidence only.
  A standalone data-document navigation with a 200 ms deadline also passed,
  so no speculative protocol-scheme special case was added.
- Current full gate: exec session 20851, logs
  `target/gate/1789019241-4023547`, console
  `/tmp/ferridriver-bidi-ordered-barrier-ready.log`. Format, types, lint, docs
  and the workspace build passed; E2E, BDD and integration are running.
  The barrier and ordered binding changes remain uncommitted.

- The ordered BiDi queue passed the existing call-order case, authentication,
  and navigation-from-binding. The cross-backend test exposed WebKit awaiting
  its user callback inline, which deadlocks when that callback awaits navigation
  lifecycle events. The same run also repeated the unchanged `toPass` count
  failure. It was interrupted after known failures at 288.976 wall seconds.
- WebKit binding calls now enter a separate ordered future queue after their
  source frame and callback are resolved. A JoinSet aborts the worker when the
  owning event loop exits, retaining teardown ownership. Invocation order and
  the target-session swap behavior are preserved.
- Another 256-case native retry scheduling run, this time with attempt-timing
  diagnostics on failure, passed in 3.2 seconds with 32 workers:
  `/tmp/ferridriver-to-pass-timing.log`. No retry timing semantics or existing
  assertions have changed.
- Combined verification is running at 16 browser slots in exec session 31374,
  logs `target/gate/1789019554-3204`, console
  `/tmp/ferridriver-binding-queues-ready16.log`. Format, types and lint passed.
  The default worker budget remains unchanged. None of the queue fixes are
  committed yet.

- Combined binding-queue and BiDi acknowledgement verification passed all 155
  gate checks: 424.853 wall / 424.75 gate seconds at 16 browser slots. This
  was a rebuild run: Rust test compilation took 198.59 seconds and the Rust UI
  test rebuilt its package-specific graph in 164.96 seconds. It is not a warm
  benchmark. E2E: 2227 passed, 33 existing skips; BDD: 637 passed, 19 existing
  skips; integrations: 485 passed. Both transport barrier unit tests passed.
- Next migration is prepared in `test-registry.test.mjs` and a private registry
  observation probe. Seven native cases passed in 42 ms, preserving fixture
  inference, modifier/annotation metadata, suite membership, source-map
  locations, custom fixture chains, off-host availability and named diagnostics.
  Its original Rust target remains until backup and parity verification finish.

- Pushed `1e704840`, the verified BiDi event acknowledgement and ordered
  BiDi/WebKit binding queues. The broad protocol/mobile capability audit is
  still open; this checkpoint does not claim parity or benchmark superiority.
- The original `test_registry.rs` target (14384 bytes, unchanged from HEAD)
  was copied and byte-verified at
  `/tmp/ferridriver-test-registry-backup-a5i_b22x/test_registry.rs` before
  removal. Seven native cases replace its five Rust cases, splitting the
  original three-host loop into separate native cases. Remaining top-level
  Rust integration targets: 66.
- Registry migration's first gate rejected a 105-line operation dispatcher in
  1.672 seconds before building binaries. The collection operation is now a
  separate helper. Current full verification runs at 16 browser slots in exec
  session 12026, console `/tmp/ferridriver-registry-migration-ready2.log`.
  The private probe exposes observations; assertions live in native JS.
  Migration files are not committed yet.

- Registry migration full gate passed all 154 checks in 111.916 wall seconds
  (111.79 gate), at 16 browser slots. Logs:
  `target/gate/1789020131-209777`, `/tmp/ferridriver-registry-migration-ready2.log`.
  All 492 native integrations passed. This validates removal of the original
  Rust registry target while retaining its assertions.

- Pushed `712bd48b`, the verified native registry migration.
- Three native extension reload cases passed in 46 ms. They compile each
  extension twice in the same process, verifying standalone/package helper
  edits invalidate cached manifests and an unchanged tree retains its manifest.
  The existing probe now returns extracted manifests as observations.
- Removed `extension_reload.rs` only after confirming its 3773 bytes matched
  HEAD and its backup at
  `/tmp/ferridriver-extension-reload-backup-i80ckfdd/extension_reload.rs`.
  Remaining top-level Rust integration targets: 65.
- Full reload migration verification is running at 16 browser slots in exec
  session 70931, console `/tmp/ferridriver-extension-reload-ready.log`.
  Migration files are not committed yet.

- Reload migration gate exited 1 after 110.975 wall seconds (110.87 gate).
  Six existing command cases rejected the added manifest response field;
  browser suites passed. The probe now exposes manifests only when requested
  through `includeManifests`, preserving the existing exact response contract
  and every command assertion. Rebuilding the probe in exec session 40949
  before checking both native suites together.

- The corrected probe passed all nine native reload/command cases together
  in 193 ms, with existing exact command assertions unchanged. Full gate
  session 15489 is running at 16 slots, console
  `/tmp/ferridriver-extension-reload-ready2.log`. The focused package rebuild
  took 2m43s because Cargo rebuilt the narrower dependency graph; future
  probe builds should use the workspace graph already used by the gate.

- Reload migration full gate passed all 153 checks in 111.691 wall seconds
  (111.61 gate), with 495 native integrations passing at 16 browser slots.
  Logs: `target/gate/1789020949-574666` and
  `/tmp/ferridriver-extension-reload-ready2.log`. No assertions were weakened.
  Remaining migration and capability milestones are still open.

- Pushed `60ef0b32`, the verified reload migration. Migrated six more
  extension host/source-map cases to native JS. Existing host tests remain
  intact; all 18 combined cases pass in 2.1s. The new probe uses production
  load_bindings and Session APIs, exposing observations without Rust assertions.
  Cold and warm source-map cases load in one process with separate sessions.
  Backed-up, unchanged Rust originals removed from the gate: /tmp/ferridriver-host-maps-backup-30154jmf
  (extension_host_matrix.rs 8191 bytes; extension_source_maps.rs 6659 bytes).
  The workspace probe build took 1.42s versus the prior narrower graph 2m43s.

- Host/source-map migration first gate stopped at lint in 1.670s: the
  dispatcher reached 101 lines and the extension operation future needed
  boxing. Extracted cache observation serialization without behavior changes
  and boxed the new operation. Full verification now runs in session 55842,
  `/tmp/ferridriver-host-maps-ready2.log`, at 16 browser slots.
  Remaining top-level Rust integration targets: 63.

- Host/source-map migration full gate passed in 111.690 wall seconds
  (111.59 gate) at 16 browser slots. Full logs:
  `target/gate/1789021330-757483`, `/tmp/ferridriver-host-maps-ready2.log`.
  All 501 native integration cases passed.

- Pushed `3c2cb6ab`, verified host/source-map migration. Migrated all
  12 extension fixture cases to native JS, including actual runner fixture
  composition, reversed order, automatic fixtures, missing-package failure,
  imported APIs, and independent extraction/session policy enforcement.
  Native cases pass in 121 ms. Probe workspace rebuild took 1.38s.
  Read ferrijs SANDBOX.md before exposing separate policy observations;
  production permission behavior is unchanged. Original Rust target
  (17515 bytes, matched HEAD) backed up at /tmp/ferridriver-extension-fixtures-backup-mq_r1y39/extension_fixtures.rs
  before removal. Remaining top-level Rust integration targets: 62.

- Extension fixture migration full gate passed all 150 checks in
  111.035 wall seconds (110.95 gate) at 16 browser slots, with all 513 native
  integrations passing. Logs: `target/gate/1789021652-938692`,
  `/tmp/ferridriver-extension-fixtures-ready.log`. Remote CI for the previous
  commit remains in progress; local verification does not establish CI success.

- Pushed `c0b382e9`, verified fixture migration. Migrated six extraction
  context contracts and five provided-module contracts to native JS.
  Direct compile observations preserve separate batches and append their
  bytecode into one session; cache-hit/cold-consumer ordering remains tested.
  Loader observations cover identity, aliases, plain-script imports, shared
  helpers, and refusal plus non-resolution of late claims. All 11 cases
  pass in 111 ms. Unchanged Rust originals backed up at /tmp/ferridriver-extraction-provided-backup-_3sh83go
  (extraction_context.rs 13802 bytes; provided_modules.rs 11202 bytes).
  Remaining top-level Rust integration targets: 60.

- Extraction/provider migration full gate passed all 148 checks in
  108.574 wall seconds (108.49 gate) at 16 browser slots; all 524 native
  integrations passed. Logs: `target/gate/1789022089-1121400`,
  `/tmp/ferridriver-extraction-provided-ready.log`. This individual timing
  does not establish a repeatable performance improvement.

- Pushed `697f2c59`, verified extraction/provider migration. Found that
  `just test-integration` failed before testing: ferridriver-gate is absent
  from Cargo default members, so its explicit bin selector was rejected.
  Both focused integration and backend recipes now use the gate's workspace
  build command. The repaired integration recipe passed 11 native cases in
  1.453s wall time (112ms tests), `/tmp/ferridriver-focused-recipe-fixed.log`.
  Full default-parallelism gate running in session 60302, console
  `/tmp/ferridriver-justfile-ready-default.log`.

- Default 32-slot gate exited 1 after 102.353s (102.27 gate), solely
  on the unresolved WebKit toPass custom-interval test: fewer than five
  attempts in 400ms. No assertion changed. Added per-attempt timestamps
  to the assertion message to distinguish callback delay from timer drift
  during the actual full workload. Justfile fix remains uncommitted until
  the gate is verified.

- Diagnostic default gate is running in session 30853,
  `/tmp/ferridriver-retry-timing-ready.log`. The toPass failure message now
  includes attempt timestamps; its minimum-attempt and deadline assertions
  remain unchanged. Poll this handle before launching another gate.

- Diagnostic default gate passed all 148 checks in 101.527 wall
  seconds (101.42 gate), 32 jobs/browser slots and default two Tokio
  threads per child. Logs: `target/gate/1789022441-1486434`. All four
  toPass cases passed, so this run did not capture a failing timestamp.
- Compared TOKIO_WORKER_THREADS=4 on the same final worktree and 32-slot
  budget: gate passed in 101.544 wall seconds. Logs:
  `target/gate/1789022549-1668397`, `/tmp/ferridriver-runtime4-ready.log`.
  No speed improvement was demonstrated, so the default remains two.
  The intermittent retry-count failure remains unresolved; timestamps are
  retained in its assertion message for the next reproduction. The focused
  justfile command fix is verified independently and by the full gate.

- Pushed `e96c46ee`, verified focused recipe repair and retry timing
  diagnostics. Migrated all ten extension policy contracts to native JS.
  The private probe optionally installs the production HttpClient just as
  the original harness did. Network ceilings, command refusals/admission,
  timeout AbortSignal, microtask restrictions, and metadata assertions remain.
  Native cases pass in 275 ms. Original Rust target (14405 bytes, matched
  HEAD) backed up at /tmp/ferridriver-extension-policy-backup-sraevrb8/extension_policy.rs before removal.
  Remaining top-level Rust integration targets: 59.

- Extension policy migration full default gate passed all 147 checks
  in 104.742 wall seconds (104.66 gate), 32 jobs/browser slots, with all
  534 native integrations passing. Logs: `target/gate/1789022824-1854314`,
  `/tmp/ferridriver-extension-policy-ready.log`. All four retry timing cases
  passed this run; the previously observed intermittent failure remains open.

- Pushed `f08edbcc`, verified policy migration. Migrated both launch
  proxy targets to four native cases using the existing observable fixture
  proxy. Chromium pipe/WebSocket, Firefox, and standalone-script routing
  retain destination assertions; standalone VM health remains explicit.
  Fixture readiness is output-driven and owned servers stop in finally.
  All four cases pass in 1.0s. Unchanged originals backed up at /tmp/ferridriver-launch-proxy-backup-yefikh5_
  (core 4357 bytes; script 3222 bytes) before removal.
  Remaining top-level Rust integration targets: 57.

- Launch proxy migration full default gate passed all 145 checks in
  105.253 wall seconds (105.17 gate), 32 jobs/browser slots. All 538 native
  integrations passed. Logs: `target/gate/1789023115-2041327`,
  `/tmp/ferridriver-launch-proxy-ready.log`. No retry timing failure this run.

- Pushed `4627b20a`, verified launch-proxy migration. Inspecting remaining
  HTTP tests found missing JS/NAPI redirect modes and response redirect flags.
  Added four native regression cases; all failed on the prior binary. The
  Request overload uses a page-network Request, not WHATWG Request, so the
  initial speculative Request-constructor case was corrected to the actual
  per-call options contract after reading both binding implementations.
  Shared core parsing now validates follow/manual/error before I/O; native
  and NAPI bindings expose modes and redirected/unfollowedRedirect methods.
  Native declaration source updated, no generated declarations edited.
  All four native cases now pass. The original HTTP Rust targets remain
  until their complete assertion set is migrated.

- HTTP redirect binding full verification is live in session 33983,
  `/tmp/ferridriver-http-redirect-ready.log`, logs
  `target/gate/1789023545-2233498`. Lint/types/strict docs passed. Rust test
  graph is rebuilding after core/binding changes, alongside browser suites.
  Poll the same handle; do not start another full gate while it is live.
  Need runtime verification of the NAPI additions after addon build, plus
  final leak/style checks and commit/push. No HTTP originals removed yet.

- HTTP redirect binding gate passed all 145 checks in 135.332 wall
  seconds (135.25 gate) at default 32 slots, including rebuilds. All four
  retry timing cases passed. The built NAPI addon was additionally exercised
  through the native runner (22ms): /tmp/ferridriver-napi-redirect-verification.test.mjs
  and /tmp/ferridriver-napi-redirect-verification.log. Bun only loaded the
  .node application fixture and returned observations; assertions ran in
  ferridriver. Generated NAPI declarations include the new option and methods.
  The addon check should join a permanent native-addon gate group when
  migrating that suite; it currently remains a focused verification artifact.

- Pushed `18fa82ea`, verified redirect bindings. Migrated all eight
  HTTP redirect/cookie/multipart contracts to native JS. The fixture server
  retains original response bodies and headers under /fx/http-client routes;
  per-request limits, redirect flags, cookie persistence, multipart headers,
  bytes, and boundaries retain their assertions. Shared output-driven server
  lifecycle helper now serves both native HTTP suites. All 12 combined cases
  pass in 26 ms. Rust original (9801 bytes, matched HEAD) backed up at
  /tmp/ferridriver-http-redirects-backup-5pd90cml/http_client_redirects.rs before removal.
  Remaining top-level Rust integration targets: 56.

- HTTP migration gate failed only on the known WebKit retry-count
  case: attempts at 0, 61, and 495ms, proving a 434ms interruption.
  Gate exited 1 after 106.002s; logs target/gate/1789023926-2440013.
  Added absolute start time to the existing failure diagnostics so Linux
  scheduler samples can distinguish runnable-thread starvation from VM
  wake delays. Sampler /tmp/ferridriver-sample-gate.py records only E2E
  thread scheduler counters, not command arguments or environment.

- Sampled default gate passed all checks in 101.543s; no failing
  retry window was captured. Logs target/gate/1789024161-2626874 and
  /tmp/ferridriver-sampled-ready.log; scheduler counters retained in
  /tmp/ferridriver-e2e-scheduling.jsonl. Original failed window remains
  0/61/495ms. Exploring a controlled Tokio clock for the interval-selection
  contract while retaining its assertions and real-time timeout coverage.
  HTTP migration and absolute retry-start diagnostic remain uncommitted.

- Added controlled Tokio clock execution to the private current-thread
  runtime probe using test-util, after reading Tokio clock implementation.
  Session initialization precedes clock pause; clock resumes before errors
  are returned. Native retry-clock cases preserve >=5 attempts, last error,
  and <3000ms real-time bounds, and distinguish default intervals. Observed
  custom schedule: 9 attempts; default: 4; both 401ms virtual elapsed.
  Two tests pass in 32ms. Existing real-time E2E assertion remains intact.
  This confirms interval selection but does not explain the 434ms stall.

- Final HTTP migration and controlled-clock gate passed all 144 checks with
  32 jobs and 32 browser slots in 197.804s wall time (197.72s gate).
  Logs: target/gate/1789024512-2810834. Documentation rebuilt in 95.04s;
  E2E took 98.15s, BDD 91.90s, integration 49.34s. This is a rebuild
  measurement, not a warm-run speedup. Real-time retry coverage passed
  this run; the earlier intermittent 434ms gap remains unexplained.

- Pushed HTTP migration 3d279a92 and controlled-clock coverage 5e42bf88.
  Migrated all five core HTTP network-guard cases to native JS, preserving
  the no-guard fast path, numeric-host allowlist, and metadata preflight
  and redirect enforcement. The private probe constructs the original
  production client and Permissions policy; assertions live in JS. Added
  a specific blocked-address assertion for metadata redirects. Five cases
  pass in 26ms without a browser. Original backed up at /tmp/ferridriver-net-guard-backup-dh_2421v/http_client_net_guard.rs
  (5371 bytes, verified against HEAD). Remaining Rust targets: 55.

- Protocol audit note from source: core Page.start_tracing/stop_tracing
  and NAPI wrappers exist, but native ScriptPage lacks both methods.
  CDP stop_tracing installs a lossless tap before Tracing.end, then ignores
  a 30-second drain timeout and returns collected events as success.
  An interrupted trace can therefore look complete. Address this with
  observable native JS coverage when extending performance capabilities.
  Read Playwright crBrowser.ts tracing implementation for comparison: it
  waits for tracingComplete and consumes ReturnAsStream. No speed claim.

- Network-guard final gate exited 0: 143 checks, zero failures or blocks,
  107.347s wall time (107.26s gate), 32 jobs and 32 browser slots.
  Logs: target/gate/1789025159-3000698 and /tmp/ferridriver-net-guard-ready.log.
  Native integration: 557 passed in 49.2s; BDD: 91.95s. No benchmark
  claim of a speedup over the prior 101.543s warm run.

- Migrated event_bus.rs (nine cases) and reporter_api.rs (five cases)
  to native JS using private observations from production EventBus,
  ReporterDriver, RunPreamble, and reporter factory implementations.
  Coverage retains fan-out, clone lifetime, channel closure, immediate
  availability, concurrent drain ordering, finalization, suite nesting,
  stable IDs, shard deduplication, terminal fallback, JSON round trips,
  and the no-float wire contract. JS inspects raw number tokens so an
  integral float such as 30000.0 cannot disappear during JSON.parse.
  Added a direct nonempty returned-reporter-set assertion.
  Fourteen cases pass in 27ms; Rust originals backed up in /tmp/ferridriver-reporter-tests-backup-9bxa18wm
  (10968 and 8492 bytes, matched HEAD). Remaining Rust targets: 53.

- Reporter migration final gate passed: 141 checks, zero failures or
  blocks, 106.78s gate time; logs target/gate/1789025740-3191887.
  571 native integration tests passed in 49.67s, 2227 E2E cases in
  97.7s, and 637 BDD scenarios in 91.8s. Existing skips unchanged.
  Initial lint caught an inline 4KB ProjectConfig variant and a 101-line
  dispatcher: boxed the configuration and extracted runtime-contract
  observation. No lint allowances or assertion weakening.

- Built-in reporter migration covers eleven cases through production
  reporters and the blob reader. Native JS reads generated JSON, JUnit,
  CTRF, and Markdown, retaining all original content checks. Dot/GitHub
  smoke cases now inspect captured glyphs, escaped annotations, and
  delegation. First focused run: ten pass, outputFile absence check fails
  because native fs.stat exceptions lack code=ENOENT.
- Four native fs-stat regressions reproduce missing error metadata in
  stat/lstat and their synchronous forms. Fixed upstream ferrijs using
  existing node::system_error for metadata calls; permissions unchanged.
  Ferrijs full gate exited 0 (/tmp/ferrijs-stat-errors-ready-fixed.log);
  eight direct runtime observations verify ENOENT and ENOTDIR metadata.
  Pushed ferrijs a9904851f3d86c096c87451665f420883949ba5b. Ferridriver
  now pins all six patches to that revision; consumer rebuild pending.

- Consumer build with pinned ferrijs fix completed in 3m03s. All fifteen
  focused reporter-output and fs-stat cases now pass in 24ms, including
  the original output-path absence assertion. Removed Rust reporters.rs
  after backup to /tmp/ferridriver-reporter-output-backup-grvq3tdu/reporters.rs
  (20491 bytes, matched HEAD). Remaining Rust targets: 52.

- Measured a cold dependency-update gate bottleneck: lint 95.61s,
  docs 95.77s, rust-build 125.95s. Native suites finished while Rust
  tests compiled (integration 61.34s, BDD 95.77s, E2E 101.88s).
  NAPI build subsequently recompiles ferridriver and ferrijs-fetch.
  Read installed @napi-rs/cli src/api/build.ts: setTarget always adds
  --target with the host triple; copyArtifact always reads the target
  subdirectory. This duplicates the workspace host build. Next build
  optimization should preserve native addon/type generation while sharing
  the workspace Cargo graph, with actual ABI tests and timing evidence.

- Final reporter-output/runtime-update gate exited 0: 140 checks,
  zero failures or blocks, 660.574s wall time (660.46s gate). Logs:
  target/gate/1789026521-3396958 and /tmp/ferridriver-reporter-output-ready.log.
  This is a dependency-rebuild measurement, not comparable to the prior
  106.877s warm run. NAPI build duplicated core compilation for 163.38s;
  Rust UI test then compiled the rust-e2e-example package graph and took
  174.93s. Its package selection is intentional (only selected harnesses
  should run); preserve that behavior when eliminating duplicate builds.
  Next priority: measured Cargo graph/artifact reuse, retaining real addon
  loading, generated declarations, and Rust UI execution/stop/trace checks.

- Debug addon builds now invoke the NAPI builder's shipped TypeScript source
  with a pinned Bun source patch that supports Cargo's implicit host target.
  Workspace library selection unifies dependency features with the gate build;
  NAPI still runs Cargo and generates declarations and the platform loader.
  Release builds retain the upstream CLI path. The first focused build compiled
  only ferridriver-node and exited 0 in 3.819s; its full-gate build took 8.72s
  under concurrent browser load. These are reuse measurements, not cold-build
  comparisons. Frozen-lockfile installation accepts the source patch.
- Cargo 1.98 still gates workspace feature-unification behind nightly.
  Read cargo-hakari's design and publishing constraints from a source
  checkout at /tmp/ferridriver-guppy-Gl50Ke. No workspace dependency expansion
  adopted yet; the Rust UI package graph still needs a rebuild after shared
  dependency updates. All 52 remaining Rust targets and the addon-test
  migrations remain in scope, followed by the protocol capability work.
- Host addon build full gate exited 0: 140 checks, zero failures or blocks,
  108.590s wall time. Logs: target/gate/1789027711-3605308 and
  /tmp/ferridriver-addon-host-ready.log. E2E took 98.24s, BDD 92.20s,
  native integration 50.02s. This warm result is similar to the prior 106.877s;
  the optimization removes the duplicate addon dependency graph, without
  demonstrating a warm full-gate speedup. Rust UI passed in 1.52s with its
  existing cached package graph.

- Migrated context teardown and out-of-band target recovery to three native
  JS cases. The private lifecycle probe delegates browser actions and returns
  all twelve registry sizes plus page/active-page state; JS owns assertions.
  Each of five context cycles now proves options were registered and every
  registry returns to baseline. Recovery still destroys the target through
  window.close(), checks closed-state pruning, then evaluates on a replacement.
  A synchronously armed close-event waiter replaces the original 25ms polling
  sleeps. The isolated native run passed all three cases in 315ms.
  Clean Rust originals (2990 and 3823 bytes) were backed up and verified in
  /tmp/ferridriver-lifecycle-backup-itt794jc before removal. Remaining Rust
  integration targets: 50. Initial full-gate lint found two ambiguous default
  argument constructions; both now name SerializedArgument explicitly.
- Lifecycle full gate reached 589 passing native integrations, then failed the
  existing real-time retry assertion: attempts at 1, 357, and 401ms, expected
  at least five. No assertion was changed. The migration commit was held.
- A minimal three-realm reproduction isolates executor starvation: two realms
  execute synchronous JS for 350ms after the retry callback signals readiness.
  With the gate's forced two Tokio threads, the callback attempts land at
  0/352/401ms and 1/351/402ms. Four threads pass twice with nine attempts.
  Temporary callback/error-conversion timing probes measured microseconds and
  have been removed. Diagnostic logs: /tmp/ferridriver-runtime-starvation-red.log
  and /tmp/ferridriver-retry-contention-4-jdan7qyl.log.
- Gate runtime thread defaults now follow each job's concurrency reservation
  with a floor of four; an explicit TOKIO_WORKER_THREADS still takes precedence.
  This gives E2E sixteen executor threads and shared native suites eight at
  the default 32-slot gate. A native regression runs the three-realm workload
  through the child CLI, inheriting the gate's actual executor setting.
  It fails with the old setting; full-gate verification is in progress.
  The four-thread job minimum retains executor capacity for nested concurrent
  runner tests on small CI hosts. The shared HTTP fixture server keeps two.
- The instrumented diagnostic test run passed all 2227 E2E cases but failed
  two proc-macro executables because the diagnostic invoked the gate binary
  directly without Cargo's shared-library environment. Use cargo gate or just
  ready for subsequent measurements; this diagnostic is not a green full gate.
- First capacity-adjusted full gate passed: 138 checks, zero failures or
  blocks, 130.521s wall time, including 590 native integration cases and all
  original real-time retry assertions. Logs: target/gate/1789028555-4175390.
  This run rebuilt script consumers after diagnostic cleanup. A final run
  also verifies the four-thread minimum for smaller jobs.
- Final full gate passed: 138 checks, zero failures or blocks, 108.213s wall
  time (107.96s gate). Logs: target/gate/1789028691-178722 and
  /tmp/ferridriver-runtime-capacity-final.log. The contention regression passed
  under the gate's inherited executor setting; the original 400ms retry test
  passed on all four backend projects. Warm runtime is similar to 108.590s
  before this change; the demonstrated improvement is retry responsiveness
  under synchronous JS contention, not an overall throughput speedup.

- Migrated the fifteen Rust page_api.rs groups into eighteen native integration
  cases, splitting the large basic-API group across selectors/actions, state
  accessors, waiters, and screenshots/focus/filters/viewport. Other groups retain
  AI snapshot metadata/depth/incremental/ref-map checks, init-script disposal,
  dialog defaults/listeners, injection, lifecycle states, element evaluation,
  checkbox/selection/tap, storage, close idempotence, locator set operations,
  routing/disposal/abort, browser state, and the 3000-message console storm.
  Rust callback and page-level storage APIs use the existing private lifecycle
  probe so the exact core paths stay exercised; assertions live in native JS.
  All eighteen cases pass in 969ms with eight native workers.
- Native waitForLoadState accepted omission but tried converting an explicit
  undefined argument into String. The migrated lifecycle test caught this.
  Read rquickjs FromParam<Opt<T>> and Playwright page.ts: Opt handles missing
  arguments only. The binding now uses Opt<Option<String>> and forwards the
  flattened optional state. The Rust core and NAPI already accept None.
  A native E2E regression verifies omission, undefined, and undefined plus
  timeout options on all four backend projects.
- Corrected Page.addInitScript's native TypeScript declaration from void to
  Disposable, matching its existing native/NAPI result and Playwright's source.
  A typed E2E case verifies injection, repeated disposal, and absence after
  navigation on all four backends. The existing multi-page E2E now navigates
  three data URLs concurrently, retaining the original input/button/list and
  screenshot size/difference checks. Twelve focused backend cases passed.
- Removed clean page_api.rs (42046 bytes) and parallel.rs (2604 bytes) after
  copying and byte-verifying them against HEAD in
  /tmp/ferridriver-core-page-backup-0e9v7tw4. Remaining Rust targets: 48.
  The native console storm additionally verifies message order and a working
  browser command afterward. Full-gate verification is in progress.
- Page-contract rebuild gate passed: 136 checks, zero failures or blocks,
  124.937s wall time (124.85s gate); logs target/gate/1789029491-379030.
  Native integrations: 608 passed. Retained the console storm's original
  20-second limit as a native test timeout before final verification, so the
  event-driven waiter cannot silently broaden the allowed delivery time.
- The warm follow-up failed context_options_proxy on cdp-raw: page content
  proved proxy traversal, but the expected HTTP request line was absent.
  Every backend called DELETE on the same shared proxy log, so concurrent
  tests could erase one another's observations. A native 21ms reproduction
  recorded two scopes, deleted one, and observed both disappear.
- Added exact query-key cleanup to the fixture log while preserving unscoped
  DELETE for callers with a private server. The E2E test uses a fresh UUID in
  its requested URL, asserts that exact URL was observed, and only clears its
  own records afterward. The native regression includes prefix-overlapping
  keys to prevent substring deletion of another observer. It passes in 20ms;
  forty repeated E2E executions across all four backends passed in 3.6s.
  The runner reports four unique tests for those forty executions; review
  repeat-index reporting separately before claiming full runner parity.
  Lint required extracting the fixture-log handler after it crossed 100 lines;
  no suppression or assertion weakening. Final full gate is running.
- Final page-contract/proxy gate passed: 136 checks, zero failures or blocks,
  107.972s wall time (107.88s gate); logs target/gate/1789030015-765676 and
  /tmp/ferridriver-core-page-proxy-final.log. Both proxy isolation and original
  retry timing assertions passed across all four backends. Native integration
  count is 609; E2E count is 2235. Warm gate time remains about 108s, so this
  checkpoint improves coverage and removes shared-observer flakiness without
  claiming a material whole-gate speedup. Remaining Rust targets: 48.
- Repetition audit reproduced a false green: one failed execution followed by a
  pass was reported as one flaky test with exit zero. Both executions exposed
  repeatEachIndex zero and shared an artifact directory. Native regressions
  now preserve independent failure accounting, retry histories, artifact paths,
  reporter IDs and planned cases. A barrier regression requires both repetitions
  to be active on different workers and checks their suite-hook metadata.
- Repetitions expand in the execution plan before worker allocation and reporter
  boundaries, with a marker preventing cloned/filtered plans from expanding
  twice. Core IDs carry the repeat index; index zero retains existing stable
  IDs. Blob deserialization defaults older records to index zero. Suite hooks
  use the worker's actual TestInfo rather than anonymous metadata. Live UI and
  trace keys distinguish repetitions without changing their displayed titles.
- Focused native regressions: four passed in 142ms. Full headless gate passed:
  136 checks, zero failures or blocks, 159.439s wall (159.32s gate), including
  runner/dependent test rebuilds. Logs: target/gate/1789031020-964394 and
  /tmp/ferridriver-repeat-ready.log. Integrations: 613 passed; E2E: 2235 passed,
  33 existing skips; BDD: 637 passed, 19 existing skips. The previous warm
  107.972s result is not a comparable rebuild measurement.
- Original proxy stress now reports all 40 executions, using 16 workers,
  and passes in 2.9s across four backends. This is one observation, not a
  comparative performance benchmark. Log: /tmp/ferridriver-repeat-proxy-stress.log.
  Remaining repeat audit: JS sessions are keyed by worker index; verify module
  and worker-fixture isolation between repetitions. Remaining migrations and
  protocol/capability milestones are unchanged; this checkpoint does not claim
  complete repeatEach parity or completion of the broader goal.
- Repetition isolation audit reproduced module and worker-fixture leakage with
  one worker: the second repetition saw executions=2 and state.used=true.
  BDD also delayed both AfterAll hooks and worker-fixture teardown until both
  repetitions had run. Reproductions: /tmp/ferridriver-repeat-isolation-red2.log
  and /tmp/ferridriver-repeat-bdd-red.log.
- Worker slots now finish the previous logical worker before switching repeat
  indices: suite hooks, fixture scopes, queued contexts, and browser shutdown.
  The replacement receives a fresh worker ID while retaining its parallel slot.
  Native JS and BDD sessions register cleanup with the existing worker fixture
  scope, removing their cached VM when that scope ends. This avoids retaining
  one VM per completed repetition and keeps cleanup ahead of browser shutdown.
- Eight native repetition regressions passed in 371ms, including a real browser
  fixture whose cleanup reads the page title and closes its context. BDD pins
  setup/step/AfterAll/teardown ordering before the next repetition starts.
  Full headless gate verification is in progress; migration and broader
  protocol/capability milestones remain open.
- Full lifecycle gate passed: 136 checks, zero failures or blocks, 136.090s
  wall (136.01s gate), including changed-source lint/docs/test rebuilds.
  Logs: target/gate/1789031793-1177186 and
  /tmp/ferridriver-repeat-lifecycle-ready.log. Native integrations: 617 passed;
  E2E: 2235 passed with 33 existing skips; BDD: 637 passed with 19 existing skips.
- Repeated proxy stress passed all 40 executions on 16 worker slots in 4.3s:
  /tmp/ferridriver-repeat-lifecycle-stress.log. The earlier 2.9s observation
  reused workers and browsers across repetitions; resetting their state costs
  additional launches. These single observations do not establish a stable
  performance delta. Ordinary repeatEach=1 runs keep reusing each worker.
- Migrated connect_select.rs to a native CDP reconnect contract. The fixture
  still calls CdpBrowser<WsTransport>::connect, new_page, pages, url and title,
  then drops the connection and reconnects. Assertions live in native JS and
  now require both tabs' exact URLs/titles before and after reconnect, plus
  a browser process that survives disconnect. The original silently accepted
  URL/title errors and three-second timeouts; those now fail the observation.
- The private fixture uses the existing headless WebSocket launcher and owned
  process-group cleanup, including continuous stderr draining. The original
  manual launcher stopped reading stderr as soon as it found the endpoint.
  No readiness sleeps or new test suppressions were added. Native check passed
  in 446ms: /tmp/ferridriver-cdp-migration-native.log.
- Removed clean connect_select.rs (4193 bytes) after copying and verifying it
  against HEAD at /tmp/ferridriver-cdp-migration-backup-8_jj2mgg/connect_select.rs.
  Remaining top-level Rust test targets: 47. Lint identified two large backend
  futures in the fixture; boxing those calls fixed the containing future sizes
  without suppressions. Full headless gate verification is in progress.
- The first complete migration gate failed the existing core assertion that
  launching a process writes its record: 135 checks, one failure, 108.390s
  wall; logs target/gate/1789032267-1383947. Record files were truncated and
  rewritten while concurrent sweepers deleted anything they could not parse.
  A native regression observed 9964 malformed snapshots during 64 updates,
  with zero read errors and a live child: /tmp/ferridriver-process-records-red.log.
- Process records now publish a complete temporary file through atomic
  replacement in the same directory. Initial registration and owned-directory
  updates share that writer. Existing process-test setup uses it too, with
  every assertion retained. The native observer uses a private child waiting
  on stdin, a thread barrier, and concurrent reads; it adds no readiness sleep.
  Fixed observation: 6433 complete reads, zero malformed records or I/O errors.
  Both native cases passed in 443ms: /tmp/ferridriver-record-publication-native.log.
- Adding the fixture operations crossed the dispatcher function's line limit;
  extracted its existing bundler configuration operation without suppressions.
  The final full gate is running. Its core unit-test job, including the launch
  record assertion that previously failed, has passed.
- The next gate exposed a context weberror subscription race: the test triggered
  the error before registering its waiter. It now arms the existing filtered
  waiter first, preserving every assertion. Twenty executions across four
  backends and 16 slots passed in 2.6s; log:
  /tmp/ferridriver-context-weberror-stress.log.
- The same run exposed an intermittent retry-timer gap: timestamps
  [0,52,102,628] for a 400ms budget. Temporary instrumentation in a subsequent
  passing gate measured callback bodies below 1ms, waits of 28–64ms, and 16
  executor threads. This did not reproduce or explain the 526ms gap; the
  investigation remains open and the original timing assertions are unchanged.
  Diagnostic log: /tmp/ferridriver-poll-diagnostic-ready.log. Probes were removed
  before final verification, and the assertion builder matches HEAD exactly.
- Final uninstrumented headless gate passed: 135 checks, zero failures or blocks,
  143.65s gate time. Logs: target/gate/1789033742-1989082 and
  /tmp/ferridriver-cdp-records-events-ready.log. This is a single full-gate
  observation, not a comparative speedup claim. Remaining migrations and
  broader scripting capabilities are still open.
- Migrated all seven persistent-profile cases to native JS assertions over
  the actual BrowserState instance override and resolver paths. Coverage keeps
  Chromium/Firefox/WebKit profile retention, saved and adopted viewport sizes,
  and the maximized-window precondition before resizing. The temporary-profile
  case originally proved launch/shutdown only; its new name says that rather
  than claiming directory deletion was observed. Seven native cases passed in
  1.3s: /tmp/ferridriver-persistent-native.log. External Chromium uses the existing
  headless launcher with continuous stderr draining and owned process cleanup.
- Removed the unchanged persistent_profile.rs (10860 bytes) after a verified
  backup at /tmp/ferridriver-persistent-profile-backup-_zwjubwu/persistent_profile.rs.
  The full gate for this migration is pending; remaining top-level Rust test
  targets: 46. Broader migration and capability milestones remain open.
- Persistent-profile final headless gate exited zero: 134 checks, zero failures
  or blocks, 109.52s gate time. Logs: target/gate/1789034379-2194241 and
  /tmp/ferridriver-persistent-ready.log. This warm run is not a controlled
  comparison against the prior 143.65s run.
- Migrated the five WebKit smoke cases to native integration coverage, keeping
  backend navigation/evaluation, dynamic locale on cross-site navigation and
  fresh pages, mobile feature detection/layout, desktop orientation, and public
  launch proxy routing. Browser launch and navigation errors now fail instead
  of returning success. The private backend fixture always closes its browser.
- The migrated proxy case failed with unknown --proxy-bypass-list on Linux.
  Primary Playwright source (server/webkit/webkit.ts) uses separate --ignore-host
  arguments on Linux and --proxy-bypass-list on macOS. The launcher now follows
  that mapping. The same five native cases passed in 1.0s after the correction:
  /tmp/ferridriver-webkit-native-fixed.log; red log:
  /tmp/ferridriver-webkit-native.log. No assertions were weakened.
- Removed unchanged webkit_smoke.rs (12625 bytes) after byte-verifying its backup
  at /tmp/ferridriver-webkit-smoke-backup-p7s3a8n7/webkit_smoke.rs. Remaining
  top-level Rust test targets: 45. Full-gate verification is pending.
- WebKit migration gate passed with exit zero: 133 checks, zero failures or
  blocks, 140.80s gate time, including the changed core rebuild. Logs:
  target/gate/1789034718-2392068 and
  /tmp/ferridriver-webkit-migration-ready-fixed.log. The initial lint run caught
  a large locale-application future; boxing that call reduced its containing
  futures without suppressions. No temporary diagnostic instrumentation added.
- Migrated all three screenshot-diff integration cases to native JS assertions
  over the same Rust LocatorSnapshotMatchers::to_have_screenshot path. Coverage
  retains baseline creation and byte size, identical-content matching, changed
  pixel errors and attached screenshots, actual/diff files, and size mismatch.
  Snapshot environment settings are supplied at child startup; separate probe
  processes avoid unsafe runtime environment mutation and the old global mutex.
- Three cases passed concurrently in 4.7s, retaining the matcher retry budget:
  /tmp/ferridriver-screenshot-native.log. Removed unchanged screenshot_diff.rs
  (6748 bytes) after verifying its backup at
  /tmp/ferridriver-screenshot-backup-bsfzknvr/screenshot_diff.rs. Remaining
  top-level Rust test targets: 44. Full-gate verification is pending.
- Screenshot migration final headless gate exited zero: 132 checks, zero
  failures or blocks, 110.28s gate time. Logs: target/gate/1789035078-2597867
  and /tmp/ferridriver-screenshot-migration-ready-final.log. Extracted the
  existing bundle handlers after the new operation exceeded the dispatcher's
  line limit; corrected their PathBuf signature before this successful gate.
- Migrated fixture routes and TLS coverage into ten native JS cases. All
  redirect locations, landing status/body, API/header echoes, duplicate cookie
  headers, auth challenges, CSP/download/iframe content, WebSocket text/binary
  frames, proxy observations/reset, and static-file assertions remain covered.
  TLS still uses strict and explicitly lax reqwest clients to independently
  validate the self-signed fixture. Static precedence now supplies a conflicting
  static file so it cannot pass merely because no fallback file exists.
- Ten native cases passed without browsers in 41ms:
  /tmp/ferridriver-fixture-routes-native.log. Removed unchanged routes.rs
  (9653 bytes) and tls_probe.rs (1120 bytes) after verified backups under
  /tmp/ferridriver-fixture-routes-backup-jovlwbqk/. Their client dependencies
  moved from dev dependencies to the private runtime fixture. Remaining
  top-level Rust test targets: 42. Full-gate verification is pending.
- Fixture-route final headless gate exited zero: 130 checks, zero failures or
  blocks, 110.84s gate time. Logs: target/gate/1789035408-2791238 and
  /tmp/ferridriver-fixture-routes-ready-fixed.log. Boxed the combined fixture
  observer after lint identified its large future, without suppressions.
- Configuration migration is in progress. A private observation operation
  drives the actual layer resolver with explicit cwd, config directories,
  environment, module contributions, and extension defaults. It returns typed
  configuration, effective runner/project settings, warnings and provenance.
  Twenty-nine native cases passed in 80ms without browsers:
  /tmp/ferridriver-config-layers-expanded.log. The original layering.rs remains
  intact until device/use options, schema contract, cache, and recording-policy
  assertions also have native counterparts. No full-gate completion claimed
  for this unfinished migration; current changes are not committed.
- Completed the configuration-layer assertion migration with 45 native cases,
  including device expansion/overrides, project engine and headless inheritance,
  runner use options, every recording-policy row and deprecated spelling,
  schema/type key sets, and startup-cache consistency. The observation records
  viewport presence separately because absent and Disabled serialize to the
  same JSON null; native assertions retain that typed distinction.
- All 45 cases passed in 119ms without browsers:
  /tmp/ferridriver-config-complete-native.log. Removed unchanged layering.rs
  (42388 bytes) after verifying its backup at
  /tmp/ferridriver-config-layering-backup-bj243vl7/layering.rs. Remaining
  top-level Rust test targets: 41. Full-gate verification is pending.
- Configuration migration final headless gate exited zero: 129 checks, zero
  failures or blocks, 111.17s gate time. Logs: target/gate/1789036061-2987269
  and /tmp/ferridriver-config-migration-ready-fixed.log. The initial lint pass
  required an explicit LayerCache type at initialization; corrected without
  suppressions. All migration assertions now run through the native JS suite.
- Migrated CLI source-location and trace-command coverage to nine native
  cases. The real test/merge commands preserve step coordinates, skip status
  and annotations, blob schema, and merged HTML. Legacy wire coordinates still
  pass through WireStepLocation::into_runtime. Trace show/ls preserve text and
  JSON assertions; trace view serves assets/archives and rejects outside paths
  under --no-open, with bounded readiness and owned child teardown.
- Nine native cases passed in 168ms:
  /tmp/ferridriver-cli-report-trace-native.log. The viewer waits for a complete
  output line before parsing its URL, avoiding a partial-write readiness race.
  Trace fixture entries are supplied by JS to a private create-new ZIP writer.
- Removed unchanged test_step_location.rs (7268 bytes) and trace_command.rs
  (10743 bytes) after verified backups under
  /tmp/ferridriver-cli-report-trace-backup-keouyoo3/. Remaining top-level Rust
  test targets: 39. Full-gate verification is pending.
- CLI reporting/trace migration final headless gate exited zero: 127 checks,
  zero failures or blocks, 110.92s gate time. Logs:
  target/gate/1789036454-3183825 and /tmp/ferridriver-cli-report-trace-ready.log.
- Migrated all six test-server UI integration cases into native JS protocol
  clients: listing/selection, result IDs, failures, live traces, project IDs and
  filters, unsupported-option errors, per-run worker/failure/timeout/recording/
  snapshot/reporter options, and discovery diagnostics. A private subprocess
  forwards WebSocket text frames unchanged; protocol decisions and assertions
  remain in JS. The runtime still lacks a public WebSocket binding.
- Replaced the live trace's timed sleeps and filesystem polling with the
  existing fixture held/release barrier. The test reads the actual live trace
  before releasing navigation. Timeout coverage uses an unresolved promise.
  UI readiness reads complete lines until a URL and then continues draining;
  the first attempt exposed a banner-before-URL race, fixed in this helper.
- Six native cases passed in 2.1s: /tmp/ferridriver-ui-native-fixed.log.
  Removed unchanged test_server_ui.rs (24887 bytes) after verified backup at
  /tmp/ferridriver-test-server-backup-gbqjokyq/test_server_ui.rs. Remaining
  top-level Rust test targets: 38. Full-gate verification is pending.
- Test-server migration final headless gate exited zero: 127 checks, zero
  failures or blocks, 112.08s gate time. Logs: target/gate/1789037008-3382228
  and /tmp/ferridriver-test-server-migration-ready.log. One Rust test target
  was removed and the transport fixture adds a binary target, so the gate's
  job count stayed unchanged despite moving six cases to the native runner.
- Migrated BDD UI end-to-end coverage to native JS over the existing private
  WebSocket transport. Assertions retain scenario discovery, embedded UI/assets,
  trace attachment type and file-route restrictions, v8 headers, ordered step
  spans, nested protocol calls, before/after DOM snapshot references, JS/feature
  source stacks, and the source entry named by the SHA-1 of its path.
- Cancellation now holds a real navigation at the fixture barrier, waits for
  the Stop acknowledgement, releases the navigation, checks the test end and
  run reply, and runs another scenario successfully. This replaces the old
  six-second delay without weakening lifecycle assertions. The BDD case passed
  in 945ms; all seven BDD/test-server cases passed in 2.1s:
  /tmp/ferridriver-bdd-ui-native.log.
- Removed unchanged ui_mode.rs (17334 bytes) after verifying its backup at
  /tmp/ferridriver-bdd-ui-backup-nyh8l36b/ui_mode.rs. Remaining top-level Rust
  test targets: 37. Full-gate verification is pending.
- BDD UI migration final headless gate exited zero: 126 checks, zero failures
  or blocks, 110.92s gate time. Logs: target/gate/1789037342-3575943 and
  /tmp/ferridriver-bdd-ui-ready.log.
- Migrated the Rust-harness UI end-to-end test to native JS assertions while
  retaining the actual rust-test --ui command and rust-e2e-example harness.
  Coverage keeps sidebar/idle readiness, single-test selection, run boundaries
  and totals, absolute cross-process live-trace URLs, served v8 trace artifacts,
  action events, cancellation without runFinished, and server survival.
- Native case passed in 1.2s: /tmp/ferridriver-rust-ui-native.log. Readiness and
  cancellation use stream events, with existing owned command teardown instead
  of the old fifty-millisecond child-exit polling. The generous test budget
  permits a cold Cargo list cycle without changing any result assertions.
- Removed unchanged test_ui_mode.rs (11370 bytes) after verified backup at
  /tmp/ferridriver-rust-ui-backup-g898xs9x/test_ui_mode.rs. Remaining top-level
  Rust test targets: 36. Full-gate verification is pending.
- Rust-harness UI migration final headless gate exited zero: 125 checks, zero
  failures or blocks, 110.12s gate time. Logs: target/gate/1789037606-3767329
  and /tmp/ferridriver-rust-ui-ready.log.
- Migrated all five CLI debugger cases to native JS: stop-before-call,
  stepOver and source breakpoints, BDD step/scenario locations, suspended
  test/step deadlines, resumed hangs timing out, failure-page inspection,
  preserved failure verdicts, and passing runs without published sessions.
  Existing command output and exit notifications replace CLI readiness and
  child-exit polling. Deliberate four/seven-second deadline holds remain.
- Native debugger cases passed in 7.2s with four workers:
  /tmp/ferridriver-debug-native-fixed.log. Removed unchanged test_debug.rs
  (17983 bytes), byte-verified against its committed source and backup at
  /tmp/ferridriver-debug-backup-_i5c6sv9/test_debug.rs. Remaining top-level
  Rust test targets: 35. Full-gate verification is pending.
- Debugger migration final headless gate exited zero: 124 checks, zero
  failures or blocks, 110.20s gate time. Logs: target/gate/1789038218-3962275
  and /tmp/ferridriver-debug-ready.log.
- Migrated scripting browser integration to two native JS cases, preserving
  ScriptEngine execution, bound arguments, cross-execution host variables,
  navigation, selectors, input and visibility assertions. A private observation
  fixture supplies a real headless Chromium page and explicitly closes its
  browser. Added the actual page.close assertion missing from the old test.
- Migrated four TypeScript plan cases through the real CLI/core runner, using
  original embedded TypeScript fixtures with added type declarations. Coverage retains
  hooks, custom fixtures, steps, attachments, runtime modifiers, snapshot path
  layout, baseline writes/matches and mismatch failures. The failure-location
  case now also verifies the original TypeScript line instead of only exit code.
- Focused native results: browser-engine cases 352ms, plan cases 646ms, with
  two/four workers respectively. Logs: /tmp/ferridriver-script-browser-native.log
  and /tmp/ferridriver-native-plan.log. Removed unchanged integration.rs (9976
  bytes) and e2e_plan.rs (7919 bytes) after verified backups in
  /tmp/ferridriver-script-plan-backup-_4y5t8ov/. Remaining top-level Rust files:
  33. Full-gate verification is pending.
- The first gate caught implicit global and custom-fixture types in the
  extracted TypeScript suite, which was previously invisible to typechecking.
  Added the global state declaration and fixture generic without changing
  runtime assertions. Final headless gate exited zero: 122 checks, zero
  failures or blocks, 109.54s; 717 native integration cases passed in 59.8s.
  Logs: target/gate/1789038641-4164735 and
  /tmp/ferridriver-script-plan-ready-fixed.log.
- Migrated all seventeen sidecar Rust transport/binding tests to native JS.
  Transport observations retain direct send/send_many, concurrent ID routing,
  per-call errors, empty batches and pushed events; scripting cases retain
  on/once/off behavior, unknown names, batch rejection and respawn after close.
- Sidecar closure now publishes retained watch state. wait_closed supports
  concurrent and late waiters without the old ten-millisecond child-death
  polling. Added explicit-close and repeated-wait assertions. The 22-case
  sidecar selection passed in 98ms with four workers and no browser:
  /tmp/ferridriver-sidecar-native.log.
- Removed unchanged sidecar.rs (13137 bytes), byte-verified against its
  committed source and /tmp/ferridriver-sidecar-backup-mzdxr8ay/sidecar.rs.
  Remaining top-level Rust test files: 32. Full-gate verification is pending.
- Sidecar migration gate exposed a real runtime-deadline bug: the Rust UI
  test requested 600000ms but the outer worker killed it at the configured
  30000ms. The short native reproducer failed all four timeout-extension,
  slow and zero-timeout cases at the original 100ms budget:
  /tmp/ferridriver-runtime-deadline-red.log.
- The worker now owns the shared modifiers used by JS/BDD bridges, and watches
  their deadline updates. Budget changes retain elapsed time and debugger
  pause accounting; zero disables the active deadline. Timeout reports use
  the updated budget. Primary comparison: local Playwright timeoutManager.ts
  setTimeout/_updateTimeout, which replace the total slot budget.
- Six native deadline regressions cover both setters, slow, disabled timeout,
  shortening after elapsed work and an extended unresolved body still timing
  out. Combined deadline/debugger/sidecar/Rust-UI selection passed 34 cases
  in 12.5s: /tmp/ferridriver-deadline-sidecar-native.log. The UI case included
  a fresh harness build and passed in 12.5s. Final gate is pending.
- After the worker-deadline fix, the full gate exposed the independent
  commands.read fixed 30-second limit during the Rust UI harness rebuild.
  Added its optional timeoutMs argument, consistent with command waits; the
  UI helper passes the existing 600000ms test budget through both streams.
- A native read-cancellation regression then proved partial output was lost:
  after reading head and timing out, the next call returned tail instead of
  headtail. Partial lines now live with the process reader across cancelled
  futures. Red: /tmp/ferridriver-command-read-red.log. Fixed command/deadline/
  Rust-UI selection: 13 cases passed in 11.1s at
  /tmp/ferridriver-command-read-fixed.log. Final gate remains pending.
- Final combined headless gate exited zero: 121 checks, no failures or
  blocks, 135.43s. All 742 native integration cases passed in 64.8s, including
  runtime deadline updates, partial command reads and the Rust UI harness.
  Documentation rebuilt in 20.47s and Rust test artifacts in 32.57s. Logs:
  target/gate/1789039826-582955 and
  /tmp/ferridriver-sidecar-deadline-command-ready.log.
- Migrated both long-session reliability tests to native JS, retaining the
  actual SessionTable/BrowserSession path. Coverage preserves forty live-page
  calls, 250 allocation-heavy executes, durable vars, timeout-triggered VM
  replacement, browser-epoch replacement, warm-VM capacity and idle record reap.
- Each churn return is now checked, and the epoch case seeds an existing
  global before changing the epoch, so it proves replacement rather than
  merely initializing a previously absent variable. The 250ms TTL wait retains
  the original 150ms expiry contract; it is not a readiness poll.
- Both native cases passed in 799ms with two workers:
  /tmp/ferridriver-session-reliability-native.log. Removed unchanged
  long_session_reliability.rs (11882 bytes) after verified backup at
  /tmp/ferridriver-session-reliability-backup-y8isgy2v/long_session_reliability.rs.
  Remaining top-level Rust test files: 31. Full-gate verification is pending.
- Session reliability migration final headless gate exited zero: 120 checks,
  zero failures or blocks, 109.81s gate time; 744 native integration cases
  passed in 59.5s. Logs: target/gate/1789040278-778573 and
  /tmp/ferridriver-session-reliability-ready.log.
- Migrated all eight JS-reporter Rust integration tests to native JS.
  Extended the existing event-replay fixture to load actual JS reporters,
  expose stdio/status answers and return preprocess edits. Native data builds
  the same event sequence and suite tree, using production-derived stable IDs.
- Assertions preserve all V1 hook counts and object shapes, V2 configuration,
  parent/project traversal, outcomes, step paths, stdout, attachment bytes,
  Date/number types, throwing-reporter isolation, stdio defaults, preprocessing
  exclusions/skip reasons/sharding, status override and missing-module errors.
  No browser is launched, and scratch roots are unique rather than reused.
- All eight native cases passed in 98ms with four workers:
  /tmp/ferridriver-js-reporter-native.log. Removed unchanged js_reporter.rs
  (16205 bytes) after verified backup at
  /tmp/ferridriver-js-reporter-backup-xjc2guhe/js_reporter.rs. Remaining top-level
  Rust test files: 30. Full-gate verification is pending.
- JS-reporter migration final headless gate exited zero: 119 checks, zero
  failures or blocks, 109.18s gate time; 752 native integration cases passed
  in 59.2s. Logs: target/gate/1789040715-972109 and
  /tmp/ferridriver-js-reporter-ready.log.
- Migrated twenty direct Session.execute cases from session.rs to native
  JS assertions: persistent globals, lexical scope, ordinary exceptions,
  timeout/native-await poison state, stale-deadline VM reentry, durable vars,
  timers, URL/query semantics, codecs/streams, assertion errors and console
  formatting. Script fixtures retain the original programs, with the console
  example using sashoush. The original extension cases remain in Rust pending
  their own equivalent migration; the top-level file count is still 30.
- Extended the existing execution probe with optional per-call timeout and
  owned VM-loop evaluation after an idle interval. All twenty cases passed
  in 1.2s: /tmp/ferridriver-session-semantics-native.log. Before removing their
  Rust counterparts, verified the complete original session.rs (65021 bytes)
  at /tmp/ferridriver-session-semantics-backup-q_gpbi94/session.rs.
  Full-gate verification is pending.
- Direct-session migration final headless gate exited zero: 119 checks,
  zero failures or blocks, 115.06s; 772 native integration cases passed in
  59.6s. Removed the unused Rust context helper/imports and extracted the
  existing cache-write handler after lint identified the migration leftovers.
  Logs: target/gate/1789041208-1164580 and
  /tmp/ferridriver-session-semantics-ready-final.log.
- Next gate scheduling investigation: jobs() makes build depend on docs,
  which depends on lint. Documentation took 13.02s before browser tests could
  start in this run. Cargo jobs are already serialized by cargo_busy, so
  requiring lint rather than docs for build may move documentation off the
  critical path while retaining it as a required gate check. Measure before
  making a speedup claim; no scheduler change has been applied yet.

- Measured the unchanged headless gate before changing scheduling: 119 checks,
  zero failures, 107.57s; 772 native integration cases in 59.3s. Documentation
  took 3.43s and build 3.42s before suites started. Logs:
  target/gate/1789041673-1356037 and /tmp/ferridriver-gate-scheduling-before.log.
- Build now requires lint instead of documentation. Pending prerequisites
  take priority over leaf checks, then retain longest-duration-first ordering.
  This lets builds release test work before documentation takes the Cargo
  slot; documentation remains required and its failure still fails the gate.
- Native gate regressions seeded slow documentation timing and failed on the
  old ordering for both successful and failed documentation. They now prove
  build/addon completion, documentation verdict preservation, and actual
  test/documentation overlap using a two-way FIFO handshake without sleeps.
  Existing lint fail-fast and documentation warning checks remain covered.
  Focused checks passed; full-gate timing on the changed scheduler is pending.
- Final headless gate passed: 119 checks, zero failures, 104.26s, versus
  the unchanged 107.57s baseline (3.31s, 3.1% lower in this paired run).
  Documentation rebuilt in 12.94s while tests ran, rather than blocking
  their startup. This is one paired measurement, not a stable speedup claim.
  All 774 native integration cases passed. Logs:
  target/gate/1789041870-1548625 and
  /tmp/ferridriver-gate-scheduling-after-final.log. The initial attempt stopped
  at formatting; the final run includes the formatter correction.

- Migrated the sixteen remaining Session extension integration cases to the
  native JS runner. Tests use the existing compiler/session probe, extended
  with native execute_tool, bytecode length and optional real HTTP client
  observations. Original script bodies remain in a JS fixture.
- Coverage retains namespace aliases, TypeScript local imports, compile-time
  name validation, persistent bytecode state, host branches, top-level await,
  broken-plugin isolation, ambient globals, JS/native deadlines, missing
  tools, handler errors/poison state and network attenuation across concurrent
  calls, global access and timer callbacks. Added successful local HTTP
  request/fetch checks and verified the broken tool is absent.
- All sixteen native cases passed without launching a browser. Removed
  unchanged session.rs (42984 bytes), after HEAD comparison and verified
  backup at /tmp/ferridriver-session-extensions-backup-zsreml3p/session.rs.
  Remaining top-level Rust test files: 29. Full-gate verification is pending.
- Session extension migration final headless gate exited zero: 118 checks,
  zero failures or blocks, 104.59s. All 790 native integration cases passed
  in 65.7s; the focused sixteen cases took 252ms. Logs:
  target/gate/1789042282-1744195 and
  /tmp/ferridriver-session-extensions-ready.log.

- Migrated web-server lifecycle integration assertions to native JS and
  replaced the Node trap process with a native HTTP fixture. SIGTERM/SIGINT
  markers and hard-kill absence are checked immediately after manager.stop,
  including actual HTTP response and server unreachability after shutdown.
  Removed the original marker polling and 200ms hard-kill sleep.
- Added real self-signed HTTPS readiness checks alongside both original
  plain HTTP modes: strict rejects the certificate and lenient accepts it.
- Found and reproduced unread stdout/stderr pipe blockage in spawn_command.
  A fixture writes 1 MiB to each pipe before serving; the native regression
  timed out under the old implementation. Both streams now drain throughout
  the child's life without accumulating output. The regression passed in
  118ms; all six focused matches passed in 148ms. Primary reference:
  /tmp/playwright/packages/playwright/src/plugins/webServerPlugin.ts, which
  attaches consumers to both process streams.
- Removed unchanged web_server.rs (6400 bytes) after verified backup at
  /tmp/ferridriver-web-server-backup-cxl7cff4/web_server.rs. Remaining top-level
  Rust test files: 28. Full-gate verification is pending.
- Additional server audit findings remain open: readiness builds an HTTP
  client on every poll, its backoff can exceed the configured deadline,
  failed startup does not explicitly stop the launched child, and reuse
  unnecessarily spawns a true placeholder. These need separate regressions.
- Web-server migration and pipe-drain fix final headless gate exited zero:
  117 checks, zero failures or blocks, 112.30s. All 795 native integration
  cases passed in 89.9s. Rust test artifacts rebuilt in 26.50s and docs
  took 53.49s; this is not a comparable warm-run speedup measurement.
  Logs: target/gate/1789042684-1950507 and
  /tmp/ferridriver-web-server-ready-final.log. The initial gate stopped at
  an explicit-default clippy error in the fixture; the final run fixes it.

- Added six native regressions for the remaining web-server startup audit:
  stalled response, unavailable status, later invalid configuration, later
  spawn failure, early process exit and HTTPS reuse with an empty PATH.
  All failed before the fix: the 400ms stalled probe took 5105ms; unavailable
  took 864ms; failed startup left servers reachable without shutdown markers;
  early exit became a timeout; reuse failed while trying to spawn true.
- Startup now reuses one HTTP client for its probes and bounds requests and
  backoff by one deadline. It races readiness against child exit, retaining
  the exit status. Failed startup stops the current child and previously
  started servers before returning. Child drop also requests termination.
  Reused servers have an explicit non-owning entry, with no placeholder
  process and no termination of the external service.
- All twelve focused matches passed in 541ms. The stalled/unavailable cases
  completed in 410/409ms including cleanup, early exit in 12ms, and HTTPS
  reuse in 22ms. No sleeps were added to the tests; the stalled response
  releases on shutdown through a watch channel. Primary startup/teardown
  reference remains Playwright's webServerPlugin.ts. Full gate is pending.
- Web-server startup fixes final headless gate exited zero: 117 checks,
  zero failures or blocks, 123.62s; all 801 native integration cases passed
  in 86.5s. Rust test artifacts rebuilt in 22.55s, addon build took 26.81s
  and docs 36.62s. This includes rebuild work and does not establish a
  warm-gate regression or speedup. Logs: target/gate/1789043121-2161277
  and /tmp/ferridriver-web-server-startup-ready.log.
- Audited the Chrome performance path against Playwright's
  `crBrowser.ts::startTracing/stopTracing`: CDP now uses `ReturnAsStream`,
  waits for `Tracing.tracingComplete`, consumes the handle with `IO.read`,
  closes it, and errors on missing completion or malformed trace data.
  The old `ReportEvents` implementation silently returned partial events
  after a 30-second timeout.
- Added `page.startTracing(categories?)` and `page.stopTracing()` to the
  native QuickJS scripting surface and its TypeScript declaration. Native
  Chromium coverage records a real navigation and script execution, checks
  hundreds of returned events, and checks the precondition error. Focused
  cases passed in 269ms and 163ms. Full-gate verification is pending.
- The first native metrics assertion exposed a real CDP defect: Chrome
  returned no counters until `Performance.enable` ran. `page.metrics()` now
  enables that idempotent domain at the read boundary, and the focused trace
  case verifies the `Timestamp` counter in 305ms. Full-gate verification of
  this follow-up is pending.
- Performance tracing final headless gate exited zero: 117 checks, zero
  failures or blocks, 148.31s. All 803 native integration cases passed in
  122.02s. Rust artifacts rebuilt in 59.94s, documentation took 52.12s and
  E2E took 101.36s, so this is a correctness checkpoint rather than a warm
  performance comparison. Logs: target/gate/1789043723-2371471 and
  /tmp/ferridriver-cdp-performance-ready.log.
- Metrics follow-up final headless gate exited zero: 117 checks, zero
  failures or blocks, 154.18s; all 803 native integration cases passed in
  126.44s. Rust artifacts rebuilt in 37.76s, addon build took 32.52s and
  docs 55.39s. Logs: target/gate/1789044003-2588176 and
  /tmp/ferridriver-cdp-metrics-ready-final.log.
- Replaced the manually assembled Rust runner E2E harness with a native JS
  CLI invocation. The migration preserves browser fixture discovery, two
  configured workers, click/fill interactions, retrying expectations and
  skipped-test accounting. The focused headless run passed one test containing
  four executed cases and one skip in about 400ms. The unchanged Rust harness
  was backed up at /tmp/ferridriver-runner-e2e-backup-3vjllI/runner_e2e.rs
  before removal; commit ea3f4549 is pushed.
- Audited Chromium's WebMCP protocol from Playwright's generated CDP schema
  and confirmed `WebMCP.enable` is available in the headless browser. Added a
  typed `page.webMcp` facade over the existing CDP session, keeping one target
  session per page so domain enablement survives separate property accesses.
  It exposes frame-aware enable, disable, invokeTool and cancelInvocation
  operations. A real headless test invokes a missing tool and checks the
  protocol error, proving command framing and session lifetime; the focused
  test passed in 157ms.
- WebMCP capability final headless gate exited zero: 116 checks, zero failures
  or blocks, 139.70s total. Integration completed in 96.28s, E2E in 104.22s,
  BDD in 95.85s, and all NAPI checks passed. Log directory:
  target/gate/1789045086-2811586.
- Replaced `features_e2e.rs` with a native JS runner workspace that executes
  the same browser contracts: flaky retry classification, the full locator
  matcher surface, `expect.poll`, `toPass` against a delayed DOM update, and
  page title/URL assertions. The focused headless run passed in 314ms. The
  unchanged Rust harness was backed up at
  /tmp/ferridriver-features-e2e-backup-olcRcT/features_e2e.rs before removal.
- Feature E2E migration final headless gate exited zero: 115 checks, zero
  failures or blocks, 107.94s total. Integration completed in 64.82s, E2E in
  102.41s, and BDD in 96.82s. Log directory:
  target/gate/1789045354-3011398.
- Migrated the opt-in React component E2E from Rust to native JS. With no
  `VITE_URL`, registration skips before requesting a browser fixture; when a
  URL is supplied, the test waits for the counter through `expect.poll` and
  verifies increment/decrement interactions without a fixed sleep. The
  no-server focused run confirmed one skip and no browser launch. The
  unchanged Rust file was backed up at
  /tmp/ferridriver-ct-react-live-backup-EBjtSU/ct_react_live.rs.
- Audited remote protocol selection and fixed Firefox `BrowserType.connect()`
  and instance-resolver connections to use the existing WebDriver BiDi
  transport instead of incorrectly forcing CDP. BiDi connections now accept a
  direct `ws://`/`wss://` endpoint, keep page adoption and lifecycle handling
  in the shared `BrowserState`, and pass targeted format and clippy checks.
  Safari/WebDriver HTTP and Appium classic sessions still need a separate
  adapter because they are not BiDi WebSocket endpoints.
- Remote BiDi plus component-test migration final headless gate exited zero:
  114 checks, zero failures or blocks, 106.72s total. Integration completed in
  63.14s, E2E in 101.46s, and BDD in 95.42s. The earlier isolated NAPI
  WebError failure did not reproduce in the clean rerun. Log directory:
  target/gate/1789045967-3417672.
- Replaced `new_features_e2e.rs` with native JS coverage for worker fixtures,
  lifecycle hooks, serial failure propagation, expected failures, soft
  assertions, and HTML reporter output. The focused headless run passed four
  tests in 119ms. The unchanged Rust source was backed up at
  /tmp/ferridriver-new-features-backup-s2K7GW/new_features_e2e.rs before
  removal.
- New-features migration final headless gate exited zero: 113 checks, zero
  failures or blocks, 109.30s total. Integration completed in 64.73s, E2E in
  103.75s, and BDD in 98.07s. Log directory:
  target/gate/1789046340-3611900.
- Measured higher concurrency before changing the scheduler: isolated BDD fell
  from 98.07s at eight workers to 21.96s at sixteen, and isolated E2E fell to
  62.69s at thirty-two. Running E2E with all 32 browser slots nevertheless
  increased the full gate to 114.23s by starving NAPI and shared suites, so the
  existing split allocation remains the faster end-to-end schedule.
- Replaced the 952-line `ferridriver-script` binding-surface Rust E2E sweep
  with a native JS browser-engine test. It keeps a persistent script session
  and asserts navigation, locators, input, keyboard, handles, frames, routes,
  exposed functions, media and screenshots, host variables, waits, special
  values, and disposal. The focused headless case passed in 529ms. The
  unchanged Rust source was backed up at
  /tmp/ferridriver-binding-surface-backup-dwCUzT/binding_surface_e2e.rs.
- Binding-surface migration final headless gate exited zero: 112 checks, zero
  failures or blocks, 107.21s total. Integration completed in 65.31s, E2E in
  103.82s, and BDD in 97.83s. Log directory:
  target/gate/1789047132-32897.
- Classified `napi/api-response.test.ts` as browserless in the gate scheduler.
  It only exercises the standalone HTTP client, so it now consumes zero browser
  slots and can run immediately beside browser tests. The targeted gate passed
  four checks in 7.32s.
- Browserless-slot correction final headless gate exited zero: 112 checks,
  zero failures or blocks, 109.48s total. Integration completed in 64.90s,
  E2E in 103.77s, and BDD in 97.66s. The browserless NAPI case started before
  browser capacity was available; the full makespan varied by roughly two
  seconds from the prior checkpoint. Log directory:
  target/gate/1789047427-228787.
- Added WebDriver Classic HTTP negotiation to the existing BiDi scripting
  surface. Firefox `connect()` now requests a W3C session with `webSocketUrl`,
  attaches to the returned socket without issuing a second `session.new`, and
  accepts optional W3C capabilities through the JS and NAPI connect options.
  Invalid endpoints, URL joining, and session-path preservation are covered by
  three focused unit tests. Classic-only servers still return a typed
  unsupported error because they cannot carry the BiDi command surface.
- The first post-change gate exposed stale target artifacts built by rustc
  1.98.0 after the toolchain moved to 1.98.1. Clearing generated `target`
  output and rerunning produced a clean gate with 112 checks, zero failures or
  blocks, in 767.50s; the 219.67s clean build accounts for most of that cold
  time. The immediately repeated warm gate passed the same 112 checks in
  112.51s, with integration at 69.02s, E2E at 107.66s, and BDD at 101.64s.
  Logs: `target/gate/1789048928-575875` and
  `target/gate/1789049701-839153`.
- Migrated the remaining browser-facing component E2E coverage from
  `ct_e2e.rs` to `tests/integration/ct-e2e.test.mjs`. The native JS tests use
  the fixture server, exercise real browser clicks and mount replacement, and
  verify serialized component props. Dev-server preset assertions remain as a
  library unit test, so the Rust integration target no longer launches a
  browser.
- Component E2E migration gate passed headlessly with 111 checks, zero failures
  or blocks, in 127.16s. Integration completed in 70.74s, E2E in 111.58s,
  and BDD in 102.48s. The count is one lower because the Rust `ct_e2e` target
  was removed; its two browser cases now run inside the JS integration job.
  Log: `target/gate/1789050176-1070775`.
- Extended `BrowserType.connect()` HTTP handling across Chromium, Firefox, and
  WebKit. Each product now negotiates a W3C `WebDriver` session onto the
  existing BiDi transport (`chrome`, `firefox`, or `safari`) and forwards the
  caller's W3C capabilities. Direct WebSocket connections retain their native
  backend behavior; Classic-only servers still fail clearly when no BiDi
  socket is returned. All-target Clippy passes for the expanded path.
- Final protocol-expansion gate passed headlessly with 111 checks, zero
  failures or blocks, in 140.21s. Integration completed in 84.66s, E2E in
  119.20s, and BDD in 113.02s. Log:
  `target/gate/1789050462-1314504`.
- Added focused capability-construction coverage for mobile WebDriver
  sessions. The unit test verifies `platformName` and nested `appium:options`
  survive merging while `webSocketUrl` negotiation remains enabled; four URL
  and capability tests pass, and all-target Clippy remains clean.
- Capability-test final gate passed headlessly with 111 checks, zero failures
  or blocks, in 140.75s. Integration completed in 83.47s, E2E in 110.91s,
  and BDD in 100.01s. Log: `target/gate/1789050816-1555898`.
- Added a Chromium-only `launch({ extensions: [path] })` option to the native
  scripting and NAPI surfaces. Launch-plan lowering emits one
  `--load-extension` switch and removes only the conflicting default
  `--disable-extensions`, retaining headless, automation, and sandbox policy.
  The flag unit test passes, and Firefox/WebKit behavior remains unchanged.
- Extension-launch gate passed headlessly with 111 checks, zero failures or
  blocks, in 135.58s. Integration completed in 77.48s, E2E in 110.76s, and
  BDD in 104.31s. Log: `target/gate/1789051254-1770196`.
- Measured the scheduler's E2E ceiling after the extension change. An isolated
  E2E run with 32 workers completed in 64.49s, but consumed all 64 requested
  browser slots and therefore cannot overlap the integration and BDD suites.
  The shared 16/8/8 allocation remains the faster full-gate shape; increasing
  one suite in isolation would lengthen the complete critical path.
- Fixed WebDriver HTTP connection options being discarded before session
  negotiation. Custom headers now reach `/session`, and `timeout` bounds the
  negotiation request, including typed errors for malformed header input.
  Workspace check and ferridriver Clippy pass; the focused URL/capability
  tests remain green.
- Repeated the full headless gate after the connection-path fix: 108 checks,
  zero failures or blocks, in 146.99s. Integration took 86.91s, E2E 143.03s,
  and BDD 135.76s. A cold run reached 468.70s because compiler work and
  browser suites contended for CPU; the warm result confirms correctness but
  does not establish a speed improvement.
- Added a local HTTP contract test for WebDriver negotiation. It captures the
  `/session` request, verifies authorization headers, Appium platform options,
  and `webSocketUrl: true`, then exercises the real error response path. The
  following warm full gate passed 108 checks with zero failures in 122.19s:
  integration 73.77s, E2E 110.98s, and BDD 104.76s.
- The pinned repository gate (`mise exec just@1.58.0 -- just ready`) passed
  111 checks with zero failures in 206.44s. This includes format, lint, docs,
  doc tests, native JS integration, NAPI, E2E, BDD, acceptance, and Rust
  targets; the plain `just` shim was unavailable in the shell and did not run.
- The full ready scheduler now overlaps the independent executable build and
  clippy verdict while keeping lint as a prerequisite for focused `--only`
  builds. The headless gate passed all 111 checks with zero failures or blocks
  in 105.03s, down from 206.44s (49% faster). Integration took 63.06s, E2E
  101.78s, and BDD 95.59s. The focused gate contract suite passes all 13
  tests. Log: `target/gate/1789054175-3315487`.
- Added `just test-integration-gated` for unfiltered integration work. It uses
  the dependency-aware gate so the workspace build and type check overlap,
  while the existing argument-preserving `test-integration` recipe remains
  available for filtered exploratory runs.
- Replaced the MCP trace test's 5-second polling loop with the native
  `waitForEvent('console', { predicate, timeout })` path. All eight trace
  scenarios pass across CDP, BiDi, and WebKit; the test no longer sleeps while
  waiting for a protocol event.
- Final post-change ready verification passed 111 checks with zero failures or
  blocks in 105.35s. Integration took 63.69s, E2E 101.87s, and BDD 95.83s.
  Log: `target/gate/1789054471-3511311`.
- Extended the WebMCP scripting facade with `page.webMcp.listTools()`. It
  listens for the protocol's `WebMCP.toolsAdded` event before enabling the
  domain, returns live metadata when tools exist, and returns an empty list for
  a page with no registrations after a 100ms collection window. The focused
  headless contract passes in 265ms.
