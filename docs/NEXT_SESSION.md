# Next session — Tier 7 follow-ups (§7.22 TS Reporter bridge + WebServer runtime polish)

All seven Tier 7 clusters are shipped. Follow-up work that the
previous clusters explicitly carried forward:

1. **§7.22 — TS-authored Reporter interface bridge** (cluster 6b).
   The four built-in Rust reporters and `merge-reports` are in;
   missing piece is a NAPI shim + TS helper so user-authored
   `Reporter` objects can be registered and receive lifecycle
   callbacks.
2. **§7.25 — WebServer runtime honoring** of `graceful_shutdown` and
   `ignore_https_errors`. The schema and lowering shipped in cluster
   7; the runtime piece needs server-side wiring in
   `crates/ferridriver-test/src/server.rs` (signal-first kill +
   readiness probe with TLS toggle).
3. **§7.17 carry-forward** — wire `mask`, `clip`, `animations`,
   `caret`, `scale`, `stylePath` from `ScreenshotMatcherOptions`
   into the underlying `Page::screenshot` capture path. Today they
   round-trip on the option struct but don't affect capture.
4. **toMatchAriaSnapshot full integration** — switch from the
   structural-by-line cursor walk to the Playwright
   `injected/ariaSnapshot.ts` bundle for sibling/ancestor
   enforcement and role/state diffing.
5. **WebKit + test-runner `new_context` workaround** — teach the
   worker to reuse `Browser::default_context()` when the backend
   can't open multiple contexts, so `--reporter html` runs against
   a real cdp-pipe-style page fixture on webkit too.

These are the documented Section B follow-ups. The natural next
focus depends on what you want to ship first:

- **TS Reporter bridge** unlocks user-authored reporters — high
  ergonomic value once §7.20 / §7.21 are in.
- **WebServer runtime polish** is the smallest single fix; it just
  needs `tokio::process::Child::start_kill` with a configurable
  signal and a delay.
- **§7.17 capture-time options** + ariaSnapshot integration are the
  two largest follow-ups but they unlock visual / a11y testing
  workflows.

## Read-first

1. `CLAUDE.md` — rules + lessons.
2. `PLAYWRIGHT_COMPAT.md` — the Tier-7 entries flipped this round
   note their carry-forward gaps.
3. `HANDOVER.md` — Cluster 7 recap (most recent).
4. `/tmp/playwright/packages/playwright/types/test.d.ts::Reporter`
   for the TS Reporter signature.
5. `/tmp/playwright/packages/playwright/src/runner/webServer.ts`
   for the graceful-shutdown reference.

## Baseline (must stay green)

```
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p ferridriver --lib                                 # 125
cargo test -p ferridriver-test --lib                            # 12
cargo test -p ferridriver-script --lib                          # 13
cargo test -p ferridriver-mcp --lib                             # 38
cargo test -p ferridriver-test --test new_features_e2e          # 15
cargo test -p ferridriver-test --test reporters                 # 4
cargo test -p ferridriver-test --test cluster7                  # 3
cd crates/ferridriver-node && bun run build:debug && bun test   # 940
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1
# cdp-pipe 175 / cdp-raw 175 / bidi 170 / webkit 171
```

## Cluster 6b — TS Reporter interface bridge (§7.22)

Surface (TS):

```ts
import { defineReporter } from '@ferridriver/test';

const reporter = defineReporter({
  printsToStdio: () => true,
  onBegin: (config, suite) => { ... },
  onTestBegin: (test, result) => { ... },
  onStepBegin: (test, result, step) => { ... },
  onStepEnd: (test, result, step) => { ... },
  onTestEnd: (test, result) => { ... },
  onEnd: async (result) => { ... },
  onError: (error) => { ... },
  onStdOut: (chunk, test, result) => { ... },
  onStdErr: (chunk, test, result) => { ... },
  onExit: () => { ... },
});

export default reporter;
```

Implementation:

- New NAPI method `TestRunner.registerJsReporter(impl: any)` that
  wraps the JS callbacks in a Rust `Reporter` impl.
- The wrapper translates each `ReporterEvent` variant into the
  matching JS callback, adapting the payload shapes to Playwright's
  `TestCase` / `TestResult` / `TestStep` types.
- TS `defineReporter` is a typed wrapper — accepts the same shape as
  Playwright's `Reporter` interface and just round-trips the object.

Tests:

- Register a JS reporter that records every event into an array;
  drive a small plan; assert each callback fired with the right
  shape.

## Cluster 7-followup — WebServer runtime polish (§7.25)

Files:
- `crates/ferridriver-test/src/server.rs::WebServerManager::stop`

Changes:
- Read `WebServerConfig.graceful_shutdown.signal` (default SIGTERM)
  and send it via `tokio::process::Child::start_kill` with the
  configured signal, then wait `graceful_shutdown.timeout` ms before
  SIGKILL.
- Read `WebServerConfig.ignore_https_errors` and skip TLS
  verification on the readiness HTTP probe.

Tests:
- Spawn a Node server that traps SIGTERM and exits cleanly; assert
  the manager waits and the process exit status reflects the soft
  signal.

## Prompt

> Continue ferridriver Playwright parity. Tier 7 is shipped; Section
> B follow-ups remain. Read first, in order: `CLAUDE.md`,
> `PLAYWRIGHT_COMPAT.md` (carry-forward sections), `HANDOVER.md`
> (cluster 7 recap), `docs/NEXT_SESSION.md` (this file).
>
> Pick one of: §7.22 TS Reporter bridge, §7.25 WebServer runtime
> polish, §7.17 capture-time options, ariaSnapshot integration, or
> the WebKit + test-runner `new_context` workaround. Each has its
> own scope spelled out in HANDOVER.md / PLAYWRIGHT_COMPAT.md.
>
> Baseline that must stay green is in HANDOVER.md.
>
> Non-negotiables: matching layer in Rust core; rebuild NAPI; no
> shortcuts; no emojis; no AI attribution.
