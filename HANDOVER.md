# Mobile, WebDriver and protocol handover for Fable

## Current browser work, 2026-10-05

The Android package and native WebMCP milestone is verified on real emulators:

- Managed Android accepts `pkg`, `apkPath` and the existing instance `args`.
  APK identity is checked before creating an emulator; arguments retain quoted
  spaces and trailing backslashes. Wrong-package rejection and stock Chrome 124
  controls passed with exact device/ADB cleanup.
- Chromium 157's native navigation blur animation discarded CDP input after DOM
  load. Its field-trial configuration enables an input-suppression scope through
  the native animation. Disabling only that feature fixed the unchanged form
  workflow. This is now a launcher default. The ordinary configuration passes
  the original form and three more fresh-page forms without diagnostic flags.
- The full shared WebMCP suite passes 38 assertions on the configured Chromium
  157 AndroidDesktop APK: native results/errors, same/cross-origin frames,
  registration changes, lifecycle invalidation, pending close, trusted manual
  declarative form submission and autosubmit. This is the AndroidDesktop build,
  not the regular phone Chrome package.
- `session open` now waits for host readiness or exit; the host owns its
  configured provisioning deadline. A provider delayed 62.06 seconds succeeds,
  creates/deletes one session and leaves no host or registry entry.
- The broad gate exposed a WebKit frame-cache race. Its backend applied a
  navigation, then the queued common observer applied it again after a fresh
  frame-tree snapshot, detaching restored children. An actual-handler/observer
  regression failed with one frame instead of two before the fix and passes
  now. WebKit exclusively owns its existing frame-cache updates; common
  observation buffers still run.
- Targeted desktop navigation and native form-result tests passed 36 cases
  across four projects and three repetitions. Real macOS WebKit passed five
  history/reload cycles with exact preservation of the original environment.

Earlier continuation work also remains in this candidate: CDP context identity
updates in wire order; live context snapshots let late observers adopt existing
iframes; isolated WebMCP uses a 5,249-byte utility bootstrap instead of the
176,436-byte selector engine. Local 200-call medians fell from 766 to 111ms for
pipe, 794 to 124ms for WebSocket, and 2,113 to 481ms for Firefox. These are local
measurements, not cross-machine guarantees.

Full scoped gate `target/gate/1791151443-1871600` passed 57 checks, zero failures,
exit 0: 2,371 browser cases, 879 integration cases, 637 BDD scenarios and all
Rust/lint/type/doc checks. A final review caught user `--disable-features`
replacing the Android default; the launcher now merges and deduplicates lists
while preserving positional arguments after `--`. Its regression failed before
the fix and passes now. Final-state gate `target/gate/1791152094-2091326` passed
53 Rust/build/lint/format/type checks, zero failures, exit 0. The final merged
feature and stock-device controls both passed with exact cleanup. NAPI-specific
runtime and protected acceptance suites remain outside this requested scope.

Verified source checkpoint: `.git/ferridriver-work/browser-automation-verified-20261005.zip`.
Durable evidence: `.git/ferridriver-work/browser-automation-evidence-20261005.zip`.
All earlier archives are retained; nothing was committed or published.

Current evidence:

- Android package/default flags: `/tmp/ferridriver-android-final-managed157-k6j53pv_/verification.json`
- Android full WebMCP: `/tmp/ferridriver-android-shared-webmcp-_pf978c3/verification.json`
- Final merged feature list: `/tmp/ferridriver-android-disable-features-merge-ue9adk8q/verification.json`
- Final stock Android: `/tmp/ferridriver-android-stock-regression-gd_7v3zu/verification.json`
- Slow provider: `/tmp/ferridriver-session-late-provider-746fm8_s/verification.json`
- macOS WebKit: `/tmp/ferridriver-macos-webkit-navigation-pghyo89u/verification.json`

The full project goal is not complete. Physical devices and additional mobile
browser packages remain unverified; the repeated-same-data-URL Safari history
case also reproduces through raw Appium/native Back and remains unresolved.
The tracker is `.git/ferridriver-work/browser-completion-plan.json`.


Current priority is desktop/mobile browser automation and native WebMCP in
Rust core and native scripting. Cloud, NAPI expansion and unrelated fixture
lifecycle work are deferred. Preserve all existing work; there are no new
commits or published changes.

The current tracker is `.git/ferridriver-work/browser-completion-plan.json`.
Earlier plans and archives remain under `.git/ferridriver-work/`.
The verified browser checkpoint is `browser-webmcp-verified-20261004.zip`.
Earlier verified archives remain available, including `core-fixture-cleanup-verified.zip`.

The newer checkpoint is `browser-webkit-executable-verified-20261004.zip`.
WebKit accepts an explicit executable and exposes its installed launcher via
`executablePath()`. The real wrapper-launch and missing-path assertions pass
on Linux and macOS; Mac evidence is
`/tmp/ferridriver-macos-webkit-executable-4sb4iosz/verification.json`.
An arbitrary wrapper path has no revision metadata and reports
`webkit-playwright/unknown`.

Final scoped gate `target/gate/1791140219-700416` passed 55 checks, zero failures
or blocks, exit 0, in 137.72 seconds: 541 core tests, 2,367 browser e2e and
876 integration passes, plus formatting, workspace lint/build and types.
BDD and doc tests passed in the preceding full scoped checkpoint; they were
not rerun for the executable-path change. Existing skips remain unchanged.

Historical intermittent findings from that earlier checkpoint; see the current status above:

- Six Chromium WebMCP startup waits failed once at their unchanged 1,000ms
  deadline. Then 15, 36 and 168 targeted repetitions, a 2,367-case full suite,
  and all startup cases in a 32-worker run passed. No root cause is confirmed.
- The 32-worker run failed one CDP remote-frame evaluation with an unavailable
  execution-context ID. Ten targeted repeats and 100 same-browser navigation
  workflows passed. Asynchronous cache invalidation is a hypothesis only.
- One Android listener cancellation probe accepted a connection after close.
  It did not recur in 7,600 repetitions. Linux TCP self-connect was reproduced
  without a listener. The test now reserves a distinct source port before
  cancellation, retaining both original cleanup assertions. Its three network
  tests and the final core suite pass; the original failure remains unconfirmed.

See `browser-completion-plan.json` for the individual logs and evidence.

Verified browser changes:

- WebKit now owns its process, transport tasks, observers and unique download
  directory through cancellation and cleanup retry. Linux scoped gate
  `target/gate/1791128467-3743682` passed 57 checks, zero failures, exit 0.
  Real macOS WebKit passed trusted input, Promise evaluation, separate browser
  downloads and cleanup. Evidence:
  `/tmp/ferridriver-macos-webkit-live-b3_8o2ly/verification.json`.
- iOS pointer lookup used the same accessibility marker for the temporary tab
  title and the target element. The front-tab lookup therefore selected the
  entire web view. Separate marker identities fixed the actual geometry.
  Six unchanged tab workflows passed with 12 trusted button clicks, restored
  labels and exact device/process inventories:
  `/tmp/ferridriver-ios-tabs-marker-repeat-aakea7pf/verification.json`.
- Android Chrome passed forms, frames, network responses, handles, screenshots
  and new pages with owned-device cleanup:
  `/tmp/ferridriver-android-hosted-o79l7ipy/verification.json`.
- iOS passed keyboard, frames, HTTP history, new pages and the single data-URL
  navigation/back-history workflow. The latter evidence is
  `/tmp/ferridriver-ios-data-url-once-704zpp0m/verification.json`.
- Desktop Safari passed keyboard, frames, HTTP history and independent new-page
  creation/trusted input/closure. Classic target selection foregrounds a tab,
  so the hidden-background evaluation assertion still fails. SafariDriver
  also retains an additional WebContent process after session DELETE;
  ambiguous processes were not killed. New-page evidence:
  `/tmp/ferridriver-safari-new-page-m_qq0gm4/verification.json`.

WebMCP now supports concurrent pending Classic tools and browser input,
isolated CDP/BiDi discovery, native prototype lookup, result/error handling,
and trace actions. Ten repeated real ChromeDriver Classic/BiDi cases passed
in `webmcp-http-control-repeat.log`. Separate logical admission and transport
completion budgets prevent delayed setup from invoking a tool after timeout;
14 session tests passed in `webmcp-admission-units.log`. Chromium/Firefox native
and renderer/frame tests passed before this final admission refinement.

Remote connection auth headers now reach CDP/BiDi socket handshakes and CDP
HTTP discovery. Discovery uses the existing HTTP client for endpoint prefixes,
query strings and TLS; remote advertised socket authorities are preserved.
The same absolute connection budget covers handshake, initialization and
existing-page adoption. Seven native-script regressions pass, including zero
as an unlimited deadline and delays confined to adoption.

The scoped final gate `target/gate/1791137881-20923` passed 57 checks with
zero failures or blocks, exit 0, in 883.71 seconds: 541 core tests, 2,359 browser
e2e, 637 BDD and 876 integration passes, plus workspace lint/build, docs and
types. Existing skips and the ignored benchmark are unchanged. Dedicated NAPI
and protected acceptance gates remain excluded from this scope.

The real WebMCP HTTP integration fixture requires a matching ChromeDriver and
Chromium installation. It resolves ChromeDriver from PATH and Chromium from
common executable names, with ChromeDriver's own discovery as fallback.
`FERRIDRIVER_CHROMEDRIVER` and `FERRIDRIVER_WEBDRIVER_CHROME` override the pair.
The portable-path adjustment passed the integration-scoped gate
`target/gate/1791138814-240290`: five checks, zero failures, exit 0,
including all 876 integration cases.

Remaining limits include physical-device verification (no device/endpoint has
been provided), repeated-same-data-URL iOS navigation, Safari Classic background
visibility, and browser-version-dependent native WebMCP availability. Do not
claim full platform parity from compilation or the current workflow sample.

The paragraphs below retain earlier evidence and unfinished broader work.

Shared WebMCP deadlines now reach CDP, BiDi, WebKit and Classic WebDriver.
Cancellation removes pending response slots; Classic scripts retain serialized
selection and forward the remaining provider timeout. See
[`docs/protocol-deadlines.md`](docs/protocol-deadlines.md) for semantics and
Safari/XCUITest limits. The scoped gate at `target/gate/1791091119-1509158`
passed 57 checks with zero failures or blocks in 683.60 seconds, exit 0:
487 core tests, 2,331 e2e, 637 BDD and 851 integration passes, plus workspace
lint/build, doc tests and types. Existing suite skips and the ignored benchmark
were not changed. Dedicated NAPI runtime/build and protected acceptance checks
remain excluded. Native tool probes exceeded the old caps: Chromium pipe and
WebSocket each completed once after 31 seconds; Firefox BiDi completed once
after 61 seconds. Evidence:
`/tmp/ferridriver-webmcp-long-native-npnu6iq0/verification.json`.

Installation now retains the whole incoming browser before its future is
polled, and keeps adopted pages through cancellation and failed cleanup.
Failed WebDriver DELETE preserves its owned driver/device. iOS cleanup uses
a serialized worker that retains partial progress for explicit retries and
keeps diagnostics if final-drop cleanup fails.

The ownership gate at `target/gate/1791094710-1780832` passed 57 checks,
zero failed/blocked, 415.05 seconds, exit 0; core tests passed 498/0.
macOS passed 37 unique targeted tests and real Safari 26.6.1 form, managed
iOS 26.2 keyboard/form and Firefox 157 native WebMCP (61 seconds) workflows.
Original device/UI/process inventories were restored. Evidence:
`/tmp/ferridriver-macos-ownership-tests-8worrwtw/verification.json` and
`/tmp/ferridriver-macos-browser-smoke-sx42sxjc/verification.json`.

Temporary-profile removal now retains its path and reports failures through
CDP, BiDi and Android close. The blocking job owns its lock and completion
bookkeeping even when the caller is cancelled. Missing-child errors do not
count as successful root-directory removal. This narrower follow-up passed
workspace lint/build, 503 core tests, 164 desktop lifecycle/action tests,
33 native session/WebDriver integration tests, five macOS cleanup tests and
a real managed Android hosted workflow. Evidence:
`/tmp/ferridriver-macos-tempdir-tests-dytfd_rr/verification.json` and
`/tmp/ferridriver-android-hosted-d4kc7yky/verification.json`.
The preceding full scoped gate remains the ownership gate recorded above.

Allocation ownership is now registered before provider startup, with a detached
producer that survives caller cancellation. Shutdown waits for that producer
without holding a lock it needs to publish completion. Closing retires queued
launch permits, including per-instance close. Raw WebDriver sessions are
retained immediately after their ID is parsed; failed initialization and failed
DELETE leave the same owner available for explicit retry, without another POST.
Early Android, iOS and Safari driver resources share this allocation owner.
Raw sessions close before their dependent driver/device resources. Core tests
passed 510/0 and workspace lint/build passed. Fresh macOS Safari, managed iOS
and Firefox WebMCP workflows passed with original inventories restored:
`/tmp/ferridriver-macos-browser-smoke-51tb6h5s/verification.json`.
Logs use the `allocation-` prefix in `.git/ferridriver-work`.

Android live verification found a real readiness failure: two of twelve fresh
launches reported ready but could not navigate to the host fixture. The captured
device had Wi-Fi connected and `navigator.onLine === true` while its IPv4 routes
were absent for at least twelve seconds. Diagnostics and all cleanup checks are
in `/tmp/ferridriver-android-route-repair-batch-t6odwit3/diagnosis.json`.
The new `android/network.rs` waits for a bounded Chrome XHR to an owned local
HTTP endpoint instead of the online flag. Five fresh Android startup/form/close
cycles passed with full inventory restoration:
`/tmp/ferridriver-android-probe-gate-5rmwhd71/verification.json`.
macOS passed 33 targeted allocation/device/network/profile tests:
`/tmp/ferridriver-macos-allocation-network-tests-rmc9ice0/verification.json`.
The scoped gate at `target/gate/1791103809-2199531` passed 57 checks with zero
failures or blocks in 456.54 seconds, exit 0: 513 core, 2,331 e2e, 637 BDD and
851 integration passes. Existing skips and the ignored benchmark were unchanged.
The five Android runs did not encounter a prolonged missing-route state. A
route-creation race is suggested by passing-device netd logs, but the failing
device's exact cause and reconnect recovery are not established.

Final `Allocation::drop` remains terminal best-effort. A failed DELETE there
does not preserve dependent resources for another caller because the enclosing
state has already been discarded. Do not present that fallback as acknowledged
cleanup. Durable script ownership must keep the state available for explicit
close across factory errors, VM eviction and poison.

Attached CDP/BiDi close now terminates retained protocol handles and awaits
reader/writer completion, independently of the command queue. Terminal event
subscriptions close too, including after a forced handshake timeout; this
breaks the BiDi popup/session/downloads ownership cycle. The follow-up passed
522 core tests, workspace lint/build, 35 native integration tests, five repeats
of the two new close regressions, 72 four-backend browser tests and 15 macOS
transport tests. Evidence:
`/tmp/ferridriver-macos-transport-close-tests-vww9v9gz/verification.json` and
`.git/ferridriver-work/transport-close-native-integration.log`.
The previous full scoped gate remains the allocation/network gate above.

Current work adds core `BrowserResources` and installs durable browser/process
owners before native VM initialization. Browser ownership survives eviction
and poison. Script commands use retained process owners; stop awaits exit,
one-shot cancellation requests cleanup, and failed cleanup remains retryable.
`ChildGroup` observes exit without reaping, terminates descendants, and retires
PID authority before releasing the child. Directory removal has a shared owner
and a separate recovery record, including cancellation followed by owner drop.
The process changes passed 533 core and 64 scripting tests; see
`process-owner-recovery-core-script-tests.log`. Both process suites passed ten
additional repeats. The rebuilt CLI passed 34 native command/session tests.
Those tests exposed detached hosts inheriting the launcher's process group;
`session open` now assigns the host its own group, preserving its intended
lifetime while one-shot command cleanup terminates ordinary descendants.
The first native workspace build exposed E0275 in `ferridriver-testjs`:
`Session.browser_resources` expanded the recursive browser graph during
downstream `Send` checking. The current build verifies a private cleanup
interface inside `BrowserResources`; actual states and backend enum dispatch
remain unchanged. The native workspace build and lint passed after that fix.
The 533/64 counts precede the type-boundary change; macOS subsequently passed
all 16 process, four BrowserResources and one allocation tests on fresh code.
Darwin required distinguishing zombie-only groups from genuine kill permission
failures. The implementation checks an exited leader and a complete group
snapshot of zombies before accepting EPERM. Evidence is in
`/tmp/ferridriver-macos-process-owner-tests-0yoi04bf/verification.json`; original
registry/device/UI/process inventories are unchanged. Cross macOS lint passed.
The final scoped gate at `target/gate/1791116415-2887533` passed all 57 checks
with zero failures or blocks in 111.36 seconds, exit 0. An earlier run found a
WebMCP fixture assuming execution began before its 100 ms deadline. Timeout
assertions are unchanged; cleanup no longer depends on dispatch, and form
filling is separately synchronized. The corrected WebMCP matrix passed 100
repeated cases before the full gate. Existing skips and the ignored benchmark
remain unchanged; NAPI-specific runtime/build work remains deferred.

Logical-session removal, expiry, MCP close and `ScriptHost.close` now await
resource cleanup. `SessionResources` retains browser/process ownership across
VM generations and cancels active runs before teardown. Browser cleanup across
all selected script contexts precedes process cleanup: a command in one context
may host WebDriver for another. Failed deletion keeps those providers and
session entries available for explicit retry. `ChildGroup.shutdown` and
`BrowserState.shutdown` now return errors while retaining failed owners.
Standalone CLI eval and module execution explicitly await cleanup on success
and failure; four new native cases verify held DELETE acknowledgements and
command reaping.

This follow-up passed `target/gate/1791124775-3272814`: 57 checks, zero failures
or blocks, 153.34 seconds, exit 0. Targeted native verification also covers
active-wait cancellation and reopening on all four desktop backends, plus
cross-context provider retention after failed deletion. macOS passed 44
targeted tests and 250 process tests across 50 repeats, with original resources
preserved. Evidence:
`/tmp/ferridriver-macos-session-inexit-repeat-2myvcmip/verification.json`.
The first macOS run exposed a Darwin exit transition: group signaling can return
EPERM before the child becomes waitable. A complete same-group snapshot must
prove every member is a zombie or has `PROC_FLAG_INEXIT` before accepting that
error; normal exit observation and reaping still follow. The original overflow
regression passed 51 consecutive executions after this correction. macOS core
and scripting all-targets clippy also passed with warnings denied.

Rust fixture cleanup now retains the exact value and its pending teardown
future. Cancellation resumes that future; replacement cannot redirect cleanup
to another value. Fallible callbacks retain their owners for explicit retry,
and consumed one-shot callbacks cannot report success after a panic. The macro
passes the same owned Arc to registration and publication. Worker completion
now reports fixture cleanup errors. Fourteen fixture tests pass, including the
original cancellation regression and actual worker error propagation.

This is the cleanup primitive, not complete caller integration. Pool cleanup
currently stops on failure to preserve possible provider dependencies. Setup
admission, cross-pool dependency ownership, and invoking test-scoped fixture
cleanup at every test exit remain open. Test-worker and BDD session callbacks
still require their explicit resource-owner integration. The scoped gate at
`target/gate/1791126129-3501718` passed; see
`.git/ferridriver-work/fixture-owned-scoped-ready-fixed.log`.

Next, wire explicit cleanup through test-worker, BDD, reporter and config-module
callers; the audit is `session-resource-teardown-audit.json`. WebKit's separate
process wrapper and early native backend allocation ownership also remain.
Then publish session control before provisioning and distinguish starting from ready. Never
replay an ambiguous session-creation request. The startup plan and
`ownership-scope-plan.json` retain the broader gaps. The hard-crash watchdog's
PID identity limits and early native backend allocation ownership remain open.

Android SDK license acceptance and existing-work refactoring are authorized.
The macOS VM workspace is `/home/sashoush/Workspace/apple-vms`. Preserve its
original devices, UI sessions, profiles and authentication. Pushes, publishing,
paid cloud usage and the protected acceptance snapshot below remain outside
the current authorization. No commits while that acceptance failure is open.

## Historical handover, 2026-09-13

Updated 2026-09-13. The user paused implementation, requested this handover,
a global installation of the latest Linux binary, and shutdown of the macOS
test VM. Resume implementation here on **main**, as the sole writer. Do not
launch another successor or concurrent checkout writer. The older DevTools
handover below remains intact; its counts and status are historical.

## Objective and architecture

Users choose a browser or configured device target and run the same
Playwright-shaped script through MCP, the native test runner, QuickJS or NAPI.
They should not need backend, Appium or transport details in their scripts.
Rust core owns behavior; both bindings delegate. Native mobile browsers are
not desktop mobile emulation. Protocol limitations must produce honest typed
errors, not fake results, silently ignored options or synthetic trusted input.

Finish automatic browser/device selection, installation, launch, capabilities,
timeouts, cancellation, routing and cleanup across desktop, Android, iOS and
remote WebDriver providers. BrowserStack work must use private credentials and
needs explicit authorization before paid usage. No overall API parity or
performance superiority has been established.

## Preserve before resuming

- HEAD is `adb87814` (`perf: record reproducible MCP workload comparisons`),
  following `7df36f67` (`feat: run configured targets through shared browser
  scripts`). There is one worktree, on main, with substantial tracked and
  untracked implementation work. Recheck status and read the existing code;
  do not reset, stash, clean, rebase, blindly merge, or overwrite it.
- Preserve `tests/e2e/vm_websocket.test.ts` (2133 bytes, SHA-256
  `834e87b3b7706893077070d4f85561266983647cea86cad13a6c240376e36326`).
  It is now tracked, although earlier instructions described it as untracked.
  Preserve `crates/ferridriver-node/test/benchmark-results.csv` (1014 bytes,
  SHA-256 `9e4c41ca77a65bc897bad2d37b4c8265d34f414741a3094fd131ad96e166e9df`);
  do not stage that prior benchmark output incidentally.
- Inspect sibling `../ferrijs` before edits. It was clean at the last check;
  its runtime fixes and local Cargo overrides are required. Read
  `../ferrijs/docs/SANDBOX.md` before authority changes. Permissions remain
  narrowing-only regardless of agent execution mode.
- Read `CLAUDE.md`, applicable `AGENTS.md`, `docs/protocol-audit.md`, session
  protocol, core backends and bindings, `tests/integration/webdriver-connect.test.mjs`,
  `tests/integration/mcp-protocol.test.mjs`, and the mobile recipes first.
  Read exact signatures in the actual `/tmp/playwright` clone before API edits.
  Re-clone missing primary sources; a `.git` directory alone is not a clone.
- No commits while known tests fail. Stage whole files in logical units with
  mechanism-based attribution for prior work, without an AI signature.
  Do not push, publish, tag, release, or spend money without approval.

## What is verified, and what is not

The final handover gate **on the current source** passed:
`target/gate/1789296815-2011273`, **111 checks, zero failed/blocked,
694.51 seconds, exit 0**. Log: `/tmp/ferridriver-handover-ready-final.log`.
It includes 2287 e2e, 637 BDD, 827 integration and 53 NAPI test files, Rust
tests, lint/build, format, doc tests and types, using the scoped command below.
Acceptance remains excluded for the protected-baseline issue documented below.
All build/test jobs launched for this handover have finished; none is left
running. The current navigation change still needs real iOS verification.

The first handover gate failed lint only: `navigation_case` exceeded the
function-length limit. Its assertions were extracted into
`assert_navigation_requests` without changing them; the final gate above
passed. Failed-run evidence is retained at
`target/gate/1789296662-2009000` and `/tmp/ferridriver-handover-ready.log`.
This test-only extraction was the sole source edit during handover preparation;
the release build contains the current production implementation.

The latest Linux release binary is installed globally at
`/home/sashoush/.cargo/bin/ferridriver`: version
`0.5.0 (adb878141-dirty, stable, x86_64-unknown-linux-gnu)`, 69609896 bytes,
SHA-256 `d457ae5725e5e105ae2d5e7be8992c9935499dbd4ecca7896e7064cdf8e57450`.
`cargo build --release --bin ferridriver` completed in 9m 24s, exit 0;
log `/tmp/ferridriver-handover-release-build.log`. The old installed binary
is retained at
`/home/sashoush/.local/state/ferridriver-global-install-backup-d_w4r7se/ferridriver`;
`installation.json` beside it records both hashes and the installation.

The **installed** binary passed `portable-keyboard-workflow.js` on CDP pipe,
CDP WebSocket, Firefox BiDi and Playwright WebKit, all exit 0. Results,
exact commands, stdout/stderr and screenshots:
`/tmp/ferridriver-global-smoke-ycw8y19x/results.json`. These runs verify the
Linux installation; they do not verify the latest Safari navigation change.

The last full scoped gate **before the newest navigation edits** was
`target/gate/1789294740-1766374`: 111 checks, zero failed/blocked, 820.51 seconds,
exit 0. It covered 2287 e2e, 637 BDD, 827 integration and 53 NAPI test files,
plus format, workspace lint/build, doc tests and types. Log:
`/tmp/ferridriver-shared-selection-ready.log`.

```bash
rtk proxy /home/sashoush/.local/share/mise/installs/just/1.58.0/just ready \
  --only format --only rust-build --only doc-tests --only e2e --only bdd \
  --only integration --only napi --only types
```

This is not proof for the newest edits. Gate runs after core changes can take
13 minutes: the Rust harness UI integration test rebuilds examples. Do not
launch duplicate builds because that test appears quiet.

The interrupted targeted run was recovered during handover: session 66742
returned **exit 0**, and `/tmp/ferridriver-native-navigation-tests.log` records
24 passing WebDriver session tests, including four new native navigation
tests. Compilation overlapped the final source edits, so rerun on the final
state rather than treating this alone as final verification. The final
handover gate above subsequently rebuilt and tested the current source.

Actual managed iOS test-runner proof on the prior core build, **v16**:

- `/tmp/ferridriver-ios-safari-pjc54494`, log
  `/tmp/ferridriver-ios-shared-selection-v16.log`: iPhone 16, Safari 26.2,
  keyboard/form workflow passed, exit 0, wall 144410.9 ms. Result included
  `sashoush`, a trusted click, promise result 42 and rejection handling.
- Exact device UUID/name/state, Simulator UI and xcodebuild inventories were
  unchanged afterward; `cleanupVerified` was true.
- Earlier v15 also passed the two-new-tabs workflow and the identical keyboard
  workflow passed CDP pipe, CDP WebSocket, Firefox BiDi and Playwright WebKit.
  These are bounded workflows, not proof of the full Playwright API.

Permanent evidence lives in `docs/ios-*-verification-2026-09-13.json`,
`docs/managed-ios*.json`, `docs/managed-android-verification-2026-09-12.json`,
`docs/managed-ios.md`, `docs/managed-android.md`, and `docs/protocol-audit.md`.
Inspect each record for its actual binary, assertions and limitations.

## First unfinished work: Safari navigation and injected action completion

The latest uncommitted change is in:

- `backend/webdriver/session/native_input.rs`: `NativeOperation::Navigate`
  and `native_navigate` use Safari's native address field for top-level
  `data:` URLs. An old-document marker detects commit, including same-URL
  reload; a persisted `pageshow` listener cleans it after bfcache restoration.
  Selection remains serialized, caller cancellation cannot interrupt cleanup,
  and the original Appium context/window is restored.
- `backend/webdriver/page.rs`: this route applies only to XCUITest Safari
  and parsed `data:` URLs. Other URLs/drivers retain their existing path.
- `backend/webdriver/api.rs`: one caller timeout now bounds navigation plus
  lifecycle completion, instead of giving each phase a full timeout budget.

Paths above are relative to `crates/ferridriver/src/`. These files include
prior work: do not replace them wholesale with a reconstructed implementation.
Four protocol-server tests cover commit waiting, rejection, cancellation and
deadline restoration. **The current core implementation has not yet passed
the real iOS data-URL workflow.** No public signatures changed.

Complete these checks next:

1. Reuse the existing injected engine for DOM checks and action completion.
   The user's last technical question was why it was not reused here.
   Read `crates/ferridriver/src/injected/index.ts`, especially
   `setupHitInterceptor`, `finalizeHitInterceptor` and `clickPrep`, then the
   Rust action call sites. Upstream `setupHitTargetInterceptor().stop()` can
   return `done` even when no event occurred. It is not proof of event delivery.
   Observe real trusted events; do not dispatch a fake click or fake trust.
   Avoid searching generated `injected/dist/*.min.js`; edit source only.
2. Add public QuickJS and NAPI navigation regressions and a meaningful total
   goto-deadline test. Exercise same-URL reload, fragment navigation, bfcache
   restoration without leaked markers, and identical URLs in separate tabs.
3. Build a fresh macOS binary (next unused name is v17) and run
   `scripts/fixtures/portable-data-url-workflow.js` through the managed probe.
   Keep its iframe, detach-on-HTTP-navigation and back-restoration assertions.
4. Fix the separate delayed trusted-click problem without adding arbitrary
   sleeps, changing the page viewport to hide it, or weakening assertions.
   Account for disabled controls, navigation, detached targets and actions
   which legitimately produce no click.
5. Run the final scoped gate on the actual final source and record exit codes.

Measured reasons for native address entry:

- Inspector navigation and injected top-level `location` assignment failed
  to navigate Safari to `data:` in the VM.
- `mobile: deepLink` navigated the first tab but reused an already-open tab
  for an identical URL, even after explicit native activation. It cannot
  implement `page.goto()` while preserving target identity.
- The native address-field probe navigated both tabs to the identical data
  URL, retained their handles, verified title/text, closed the second tab,
  and returned to the first. Its later click assertion failed: the protocol
  returned before the trusted DOM click became visible.

Probe evidence: `/tmp/ferridriver-address-navigation.log`,
`/tmp/ferridriver-address-navigation-command.json`,
`/tmp/ferridriver-deeplink.log`, `/tmp/ferridriver-deeplink-focused.log`, and
`docs/ios-navigation-verification-2026-09-13.json`. All owned devices were
cleaned up and original device inventories were unchanged.

Do not enable experimental WebKit Automation as a shortcut. The independent
`scripts/probe-ios-automation-lifecycle.mjs` can create/navigate/close an
additional Automation tab while retaining its Appium context anchor, but:

- Its web click returns before the DOM event in the measured case.
- Native WDA actions during Automation display Safari's “Running an Automated
  Test” interruption overlay and do not deliver the click.
- Automation window IDs are opaque `page-UUID` values, unlike Appium's
  `WEBVIEW_PID.page` context anchor. Existing mapping must be redesigned before
  enabling that transport. Last-window/session teardown is not proven.
- `stopAutomationSession({closeAllWindows:false})` leaves windows behind;
  deleting an owned simulator afterward is not a remote cleanup solution.

## Verified cancellation work to retain

`session.rs::lock_selection` is shared by ordinary WebDriver sequences and
native operations. A queued caller cancelled before it acquires the selection
lock cannot later send stale input. Once a command is in flight, its spawned
task keeps response/deadline/target-restoration ownership. Shutdown and expired
deadlines also reject queued work. Failed restoration marks the session
uncertain instead of silently continuing.

The ordinary-input regression first failed with an unexpected POST actions
(`/tmp/ferridriver-webdriver-queued-red.log`, exit 101), then passed along with
native cancellation. Ten repetitions of both cancellation tests passed:
`/tmp/ferridriver-shared-selection-repeat.log`. Keep the real key-down/up
payload; an empty actions array would not prove stale input prevention.

## VM and source locations for resuming

The main test guest was shut down through authenticated SSH with
`sudo /sbin/shutdown -h now`; the command returned 0. Docker confirmed
`apple-vms-macos-1` exited with code 0 at `2026-09-13T10:49:02Z`, with no
running PID and no OOM kill. The `apple-vms-safari` forwarding service was
stopped first. Other VMs were left running.

The main test guest is managed by `../apple-vms/scripts/macos`; SSH is
`../apple-vms/scripts/macos-ssh`. Restart with `scripts/macos start` from that
repository. The desktop automatically logs in. Start `scripts/safari start`
only when the desktop Safari forwarding service is needed. Do not start a
second VM against its disk. The separate maintenance VM, Inferno iOS VM and
Debian companion are not the managed Safari test guest.

The guest has macOS 15.7.9, Xcode 26.3 and iOS Simulator 26.2. Preserve original
device `938B5F8C-C87D-4120-A17D-61163AC1C024` (`iPhone SE VM`). Managed probes
create and remove their own devices. Do not delete original devices or kill
unowned Appium/xcodebuild processes. No synthetic Mac keystrokes; prior login
permission was a narrow exception. Credentials must never appear in logs.

Last verified Mac binary:
`/tmp/ferridriver-managed-launch.Cn1OBi/ferridriver-v16`, SHA-256
`40c3e3e0d045447c34bb90ea3962f7ba94997377c3f8b1e7dd1da549183bee2a`.
It does **not** contain the latest navigation edits. Repeatable live command:

```bash
python3 scripts/probe-ios-safari.py --managed --model 'iPhone 16' \
  --runtime test --scenario form \
  --workflow scripts/fixtures/portable-keyboard-workflow.js \
  --ssh-command /home/sashoush/Workspace/apple-vms/scripts/macos-ssh \
  --remote-binary /tmp/ferridriver-managed-launch.Cn1OBi/ferridriver-v16 \
  --diagnostics
```

Change the binary and workflow for new navigation proof. The probe supports
`script`, `napi` and `test` runtimes. Run one Mac probe at a time. Its managed
`--log-protocol` handling still needs inspection: managed target construction
currently replaces the remote capabilities carrying that option.

Cross-build command (check these temporary paths still exist):

```bash
rtk proxy env \
  SDKROOT=/tmp/ferridriver-macos-sdk-content-ic7ybo3_/MacOSX.sdk \
  MACOSX_DEPLOYMENT_TARGET=15.0 \
  CC_x86_64_apple_darwin=/tmp/ferridriver-macos-clang-jdj44toq/cc \
  AR_x86_64_apple_darwin=/usr/bin/llvm-ar \
  CARGO_TARGET_X86_64_APPLE_DARWIN_LINKER=/tmp/ferridriver-macos-clang-jdj44toq/link \
  CARGO_TARGET_X86_64_APPLE_DARWIN_RUSTFLAGS='-C linker-flavor=gcc' \
  cargo build --target x86_64-apple-darwin --bin ferridriver
```

Transfer template: `/tmp/ferridriver-shared-selection-transfer.py`. Use a new
filename, exclusive creation and local/remote SHA verification. Cache override
is `FERRIDRIVER_BROWSERS_PATH`, not `FERRIDRIVER_CACHE_DIR`.

Primary source clones and retained patches:

- `/tmp/playwright`: canonical public signatures and injected helpers.
- `/tmp/ferridriver-appium-xcuitest-driver`: 12.12.3,
  `65585c013009dd3f3dd4e85cb89f7dca185d017c`; prior local window/context patches.
- `/tmp/ferridriver-appium-remote-debugger`: 17.4.2,
  `64e4914bb763be134ec577176118ecda242b04d6`; prior local RPC/page/gesture fixes.
- Both install patches live under `crates/ferridriver/src/install/` and are
  applied to a private pinned installation. Their prior upstream unit/lint
  results were 512 and 397 tests respectively, not proof of current core work.
- `/tmp/ferridriver-webdriveragent`, `/tmp/ferridriver-webkit-primary`, and
  `/tmp/ferridriver-webdriver-spec`: native URL/action semantics, Automation
  transport and WebDriver event-completion requirements.

## Remaining broader milestones

- Audit the current backend/browser selection and transport naming through
  CLI, MCP, config, runner and both bindings. Prove choosing Safari works
  without driver knowledge. Distinguish desktop safaridriver, iOS Safari,
  Playwright WebKit and desktop emulation. Read WDIO/Playwright implementations
  before changing the public architecture.
- Expand the same-script matrix beyond the recorded form/tab/frame/history
  workflows: locators and option effects, trusted keyboard/pointer/touch,
  dialogs, navigation/events, cookies/storage, capture, waits, cancellation,
  reconnect and teardown. Genuine protocol limitations need explicit capability
  detection and typed errors. Existing gate counts do not prove WebDriver
  implements every API.
- Recheck automatic Android installation/launch and Chrome onboarding,
  including the notification prompt the user observed. Preserve device data;
  verify both fresh installation and repeated launch with the same script.
- Complete cross-platform dependency installation and diagnostics, including
  offline/missing dependencies, interrupted installs, versions and ownership.
  Xcode/runtime availability and physical iOS signing are external prerequisites,
  not something to claim installed without running it.
- Finish realistic local remote-provider tests for endpoint credentials,
  capability negotiation, sessions, errors, quotas and cleanup. Read provider
  quota docs before concurrency; do not guess shared limits. Real BrowserStack
  and physical-device runs still require external inputs/paid-use approval.
- Repeat equivalent-task benchmarks after correctness work. Commands, pinned
  sources, distributions and limitations are in `docs/protocol-comparison.md`
  and `scripts/bench-agent-protocols.py`. The recorded read-only MCP workload
  favors ferridriver's warm latency, but not its single cold observation;
  frontend RSS excludes browser/daemon memory. Do not generalize this to full
  capability or performance superiority over agent-browser/devtools-mcp.

## Acceptance issue requiring user input

The acceptance snapshot at
`tests/acceptance/parity/tests/acceptance/parity/tests/acceptance/parity/__screenshots__/mobile/linux/shot.png`
is protected prior work (432 bytes). It contains an incorrect white corner.
The actual solid `#123456` image is 347 bytes at
`test-results/tests-acceptance-parity-parity.spec.ts > parity acceptance > snapshotPathTemplate names the baseline-83d6311e87d4/shot-actual.png`.
An independent Playwright run with the same WebKit 2360 produced identical
pixels (`/tmp/ferridriver-snapshot-reference-OAFA3T/shot.png`, 392 bytes).
Approval to replace the baseline was requested and remains unanswered.
Do not overwrite it, weaken the assertion or call the full acceptance suite
green. The scoped gates above exclude acceptance; this still blocks commits.

## Historical DevTools / Lighthouse handover

A work plan, not a diary. `docs/README.md` says handover notes rot, and
it is right, so this one is built to be deleted a piece at a time:
every section below is a unit of work with the check that closes it.
Delete the section when it lands.

Written against `83c1def2`, minus the sections that have since landed. Every number here was measured with the command beside it, not
taken from anyone's documentation, including mine — a count of upstream
heap tools in `site/docs/comparison/index.md` said 11 for a week; it is
13.

## Where the line currently is

Gates that exist, and what each one actually proves:

| Command | Proves |
|---|---|
| `just perf-diff` | The 19 trace insights and the Lantern model still agree with the real devtools-frontend engine on four recorded traces |
| `just a11y-diff` | `page.checkAccessibility()` still agrees with Lighthouse's axe verdicts on three fixture pages |
| `just quality-diff` | The ten ported live-page audits still agree with Lighthouse |
| `just robots-diff` | Our `robots.txt` parser still agrees with `robots-parser` 3.0.1, which is what `is-crawlable` really has to agree with |
| `just heap-diff` | Our reading of a `.heapsnapshot` still agrees with DevTools' own heap engine, over four snapshots in two pairs: 200 nodes apiece, every node-addressed query, the searches, all seven named node filters, the realm attribution, and what a diff of each pair reports |
| `just lh-record` | Re-derives the recordings the two above compare against |
| `just lh-audit <url>` | What Lighthouse concludes about any live page, for exploring |
| `just test` | All of the offline halves, plus 2163 e2e across four backends, 598 BDD, 1111 NAPI, and the e2e typecheck |

Ported and gated: `crates/ferridriver-heap`, which is what
`page.takeHeapSnapshot()` hands back and covers all thirteen of
`chrome-devtools-mcp`'s heap tools; all 67 axe wrappers (by running the
engine, not the wrappers); and eight audits in
`crates/ferridriver/src/audits.rs` —
`doctype`, `meta-description`, `canonical`, `crawlable-anchors`,
`link-text`, `image-aspect-ratio`, `image-size-responsive`,
`paste-preventing-inputs`.

Four fixture pages in
`crates/ferridriver-perf/tests/fixtures/lighthouse/`, each broken in one
dimension and sound in the others, with a snapshot AND a navigation
recording apiece. That pairing is deliberate: before it existed the
accessibility comparison ran over seven rules on pages where all seven
passed, and it agreed with Lighthouse about almost nothing while missing
five rules entirely.

## 1. `has-hsts` and `csp-xss` — decide what parity means

Both score `informative` in Lighthouse, not `binary`: they emit findings
but no pass/fail. Measured with
`just lh-audit <url>` and the navigation recordings — grep `"mode"` in
`crates/ferridriver-perf/tests/fixtures/lighthouse/*.navigation.json`.

So there is no verdict to agree with, and the comparison in
`tests/lighthouse.rs` filters on `mode == "binary"` and would skip them
even if ported. Porting them means deciding what output they produce
here and accepting that the recorded gate cannot check it — you would be
comparing findings by hand, or building a different kind of comparison.
That is a product decision, not an implementation one.

## 2. `crawlable-anchors`, the listener branch — decide, do not drift

The audit asks whether an anchor with no `href` and no href-associated
attribute has an event listener. Lighthouse answers with CDP's
`DOMDebugger.getEventListeners`. Ours reads the `onclick` attribute,
which Chrome also reports as a listener, so the two agree on
`<a onclick="...">` and part company on an anchor whose only handler came
from `addEventListener`. Stated in the module doc.

I declined to close it, and the reason matters more than the decision.
CDP has `DOMDebugger.getEventListeners`, WebKit has
`DOM.getEventListenersForNode` (via `DOM.requestNode` for a nodeId), and
BiDi has nothing at all. Implementing it makes three backends exact and
leaves Firefox approximate, which is the backend-dependent verdict
CLAUDE.md tells you to stop and not write; the alternative, a typed
`Unsupported` on BiDi, makes `checkPageQuality()` fail on Firefox for a
page shape that is not rare.

`element_handle_remote()` in `backend/mod.rs` already hands you the
per-backend object id if you do take it on. Do not do half of it.

## 3. Extensions, PWA and WebMCP — 10 tools, and why none of them landed

`install_extension`, `list_extensions`, `reload_extension`,
`trigger_extension_action`, `uninstall_extension` (5);
`install_pwa`, `launch_pwa`, `uninstall_pwa`, `get_os_app_state` (4);
`list_webmcp_tools`, `execute_webmcp_tool` (2).

The fourth category, `list_3p_developer_tools` and
`execute_3p_developer_tool`, HAS landed: `page.developerTools()` and
`page.executeDeveloperTool()`, on every backend, because it is a DOM
event rather than a protocol.

The rest read like thin protocol wrappers and are not, for reasons that
only a probe finds. Note the naming collision before you start:
`ferridriver_extensions` is already an MCP tool, and it means
ferridriver's OWN extension packages, not Chrome's.

### What the browser actually offers, measured

Measured on `HeadlessChrome/151.0.7922.34` over a browser CDP session,
not read anywhere:

| Domain | Headless | Headful |
|---|---|---|
| `Extensions.*` | absent | present, and headful is the whole of it: neither `--enable-unsafe-extension-debugging` nor dropping `--disable-extensions` changes the answer either way |
| `PWA.*` | absent | present (`getOsAppState` answers "Unknown web-app manifest id", which is the domain replying) |
| `WebMCP.*` | present | present |

**`Extensions` and `PWA` are headful-only**, and every e2e project runs
headless. That is not a backend-dependent verdict to route around; it is
a browser mode the suite does not have, and closing the row means
deciding how a headful-only spec runs at all.

Do not go looking for a switch that turns them on headless. An earlier
pass here recorded that `Extensions` needed `--disable-extensions`
dropped and `--enable-unsafe-extension-debugging` added, on the strength
of one measurement that had two variables in it. Measured one at a time,
neither matters: headful answers with or without them, headless answers
with neither. The flag is what Puppeteer sends and what upstream's own
docs name, and on this build it changes nothing.

**`WebMCP` has the domain and not the API.** `WebMCP.enable` succeeds,
but nothing on the page can register a tool for it to report:
`navigator.modelContext` is undefined, and with
`--enable-features=WebMCP` (the flag `chrome-devtools-mcp`'s own
`--categoryExperimentalWebmcp` documents) the page gains only
`window.WebMCPEvent`, whose whole prototype is `toolName`. So a port
could send `WebMCP.invokeTool` and never be able to show it working,
which rule 9 says is not done. Re-probe on a newer Chrome before
starting: `Object.getOwnPropertyNames(Object.getPrototypeOf(navigator))`
is the one-line check.

### Two of them are addressed by TAB target, not page target

`Extensions.triggerAction` takes the tab's target id (Puppeteer reads
`page._tabId`, which is its page session's PARENT session's target), and
`PWA.launch` answers with one. Our CDP backend attaches page sessions
flat and tracks no tab layer, so neither id is to hand. `Target.getTargets`
lists tab targets but nothing links one to its page: the exact route is
to attach to the tab and read the child it auto-attaches. Matching a tab
to a page by url or by set difference around the call is the shortcut,
and it is wrong the moment two pages share a url.

### What the landed half does not do

`executeDeveloperTool` takes JSON and returns JSON. Upstream also
substitutes real elements for snapshot uids in a tool's PARAMETERS, and
adopts elements out of its result into the next snapshot. Ours parks a
returned element on `window.__dtmcp.stashedElements` and hands back the
`stashedId`, which is upstream's own mechanism and reachable from
`page.evaluate`; what is missing is the bridge from a `ref=eN` to a
page-side element, in both directions. That is a decision about the
snapshot ref surface rather than about these two methods.

## 4. Smaller, already-stated gaps

Each is recorded in the module doc where it applies; this is the index,
not the detail.

- **Source-mapped console stack traces.** `chrome-devtools-mcp` resolves
  frames through source maps; we report raw positions.
- **`DuplicatedJavaScript`** detects byte-identical bodies at several
  URLs, not duplicated modules inside different bundles. Needs source
  maps, which are not in the trace.
- **`LegacyJavaScript`** has no source-map pass, so a heavily minified
  bundle may under-report.
- **`CLSCulprits`** does not attribute non-composited animations.
- **Codegen has no picker TOOLBAR.** `ferridriver codegen
  --pick-locator` now opens the page, waits for a click and prints the
  selector, and `page.pickLocator()` is declared in the types package so
  a spec can call it. What is still missing is Playwright's affordance:
  its picker is a button on the recorder's own toolbar, so a recording
  can be paused to ask what something is called and then resumed. Ours
  is a separate mode because there is no toolbar to put a button on, and
  building one is a bigger question than this bullet.
- **`SlowCSSSelector`** disagrees with upstream on purpose.
  `STATE_DIVERGENCES` in `crates/ferridriver-perf/tests/differential.rs`
  records why. Do not "fix" it to reach nineteen out of nineteen.

## 5. `@playwright/mcp` parity is probably not the goal

It ships 69 tools to our 11. Before treating that as a gap, read the
argument already made in `site/docs/comparison/index.md`: Microsoft's own
README now points coding agents at `@playwright/cli` with SKILLs
*instead* of the MCP server, on token-cost grounds, and Google ships a
`--slim` mode that cuts `chrome-devtools-mcp` from 58 tools to 3. Our 11
are deliberate, because `run_script` takes a whole program rather than
needing a tool per verb.

The defensible gap on that side is not tool count, it is the **CLI +
SKILLs shape** — `@playwright/cli` 0.1.18 ships a skill with nine
reference documents (named sessions, request mocking, tracing, video,
test generation). There is no ferridriver equivalent. Whether to build
one is a product call.

## Non-negotiables

These are not style preferences. Every one of them exists because
skipping it produced a wrong answer that survived review.

1. **Run the gate for the thing you touched.** `just perf-diff` for
   trace analysis, `just a11y-diff` for the axe path, `just quality-diff`
   for the ported audits. Tests passing is not evidence of agreement
   with upstream: the insight comparison stayed green for a whole
   session while the network analyser underneath had one RTT estimator
   where upstream has four.
2. **A fixture that makes everything pass is not evidence.** Add the
   page that FAILS the thing you are porting, in the same commit. Both
   comparisons assert a floor on how much each page trips for exactly
   this reason.
3. **No backend-dependent verdicts.** Same answer on all four, or a
   typed `Unsupported` with a reason. Not three exact and one
   approximate.
4. **All three layers, plus the types package.** Rust core, NAPI,
   QuickJS — and `packages/ferridriver-test/index.d.ts`, which is
   hand-written, so a binding can ship with no way for a spec to call
   it. `checkAccessibility` and `strictSelectors` both did. `just test`
   now typechecks it.
5. **Probe before believing a comment.** A list in `context.rs` named
   ten context options as unimplemented no-ops; eight had been working
   for months and the two that were broken were indistinguishable from
   the eight that were not. One `ferridriver run` script per option
   settled it in an hour.

## Known flake

`context_options_accept_downloads_denies` on `bidi`
(`tests/e2e/context-options.test.ts:618`) failed once in a full run and
passed three consecutive isolated runs of the file afterwards. What
failed is `dl.failure()` answering `null` rather than the refusal
message, so the download's failure state had not settled by the time the
event resolved. If it recurs, the question is whether Firefox reports a
refused download's failure in the same event that announces it, or in a
later one the wait does not cover.

`route_from_har` on `cdp-pipe` (`tests/e2e/network.test.ts:141`) failed
once in four full-suite runs, then passed three consecutive full runs
(2091 each) and four consecutive isolated runs of the file. The assertion
that failed is `served.includes('from-har')` — the HAR replay did not
serve the recorded response, so the real server answered.

The obvious cause is ruled out: `routeFromHAR` awaits
`ensure_fetch_enabled`, which awaits `Fetch.enable`'s reply, so the route
is registered and the domain enabled before the call resolves. If it
recurs, the question to start from is whether `Fetch.enable` returning
means the renderer has actually applied interception to requests issued
immediately after.
