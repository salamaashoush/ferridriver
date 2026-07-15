# Playwright parity & compatibility backlog

The single tracker for Playwright client-API surface and robustness
behaviours that ferridriver does not yet fully implement, with the
concrete blocker for each. Verified against the code (not memory) as of
2026-07-15. Resolved items are removed, not archived — git history is the
record.

## API surface not yet mapped

### Page methods needing backend plumbing
- `page.opener()` — needs opener/popup target-relationship tracking (CDP
  `Target.targetCreated.openerId` plus the BiDi/WebKit equivalents). No
  target-opener bookkeeping exists yet.
- `page.request` getter — Playwright's `page.request === context.request`
  shares the context cookie jar / storage state. ferridriver's `request`
  global is a standalone `HttpClient`; a context-bound client wired to the
  context's cookie state does not exist yet.
- `page.workers()` + a public `Worker` type — needs
  `Target.attachedToTarget` worker tracking on CDP/BiDi/WebKit and a new
  class across all three layers (core, NAPI, QuickJS).

### `page.waitForFunction` signature mismatch
- ferridriver ships `(expression: string, timeoutMs?)`; Playwright is
  `(pageFunction: string | Function, arg?, options?)`. The current form
  cannot pass a function, an arg, or the polling option bag. Rework
  touches existing callers. (`locator.waitForFunction` already matches
  Playwright.)

### `context.newCDPSession(frame)` (OOPIF form)
- Only the `Page` form is implemented (`context.rs`, script binding
  `bindings/context.rs`). Playwright also accepts an OOPIF `Frame`
  (attaches to the iframe's own target); ferridriver does not track
  per-frame targets yet.

## Partial implementations

### Trace recording (`crates/ferridriver/src/trace.rs`)
- Snapshots: documents already open in frames when tracing starts pick the
  streamer up only on their next navigation (main frames are seeded
  immediately).
- Console `args` previews (`args: [{preview, value}]`) not captured —
  text, type, and location only.
- `sources: true`: protocol actions (`goto`, locator ops) carry no stack
  (no JS call site in a Rust-driven session); BDD steps do carry their
  `.feature` file + line.
- Network timing: no `dns`/`connect`/`ssl` phases (the 3-field HAR timings
  struct); backends that do not fill timing samples fall back to ordinal.
- Action coverage: locator operations (via the retry funnel) plus
  `page.goto/reload/goBack/goForward` are traced; other page-level APIs
  (`screenshot`, `evaluate`, keyboard/mouse, waits) are not.
- `tracing.start({ screenshots: true })` and `recordVideo` on the same
  page contend for the single screencast stream — whichever starts second
  gets no frames.

### `page.routeFromHAR({ update: true })`
- Page-scoped variant returns typed `Unsupported` (`page.rs`);
  context-level update recording works. Needs per-page attribution in the
  context network log (`Request.frame_id` → owning page).

### Route predicate `times` budget
- A JS predicate route whose predicate rejects falls through to the next
  handler (chain-correct), but its `times` budget is still consumed: the
  predicate runs inside the wrapped handler, not the matcher (matchers are
  sync Rust; the predicate is an async JS call). Playwright evaluates
  predicates during matching, before `willExpire`.

### HAR recording gaps
- No cookies / `serverIPAddress` / `_securityDetails` sections; the HAR log
  carries no `pages`.
- WebSocket frames are not recorded (`_webSocketMessages`).
- BiDi records entries but no response bodies: Firefox discards bytes for
  non-intercepted responses (`network.getData` → "no such network data")
  — the same hole as Playwright's own BiDi backend.

### Clock date-string parsing
- ISO-8601 only (`YYYY-MM-DD`, `YYYY-MM-DD[T ]HH:MM[:SS[.mmm]][Z|±HH:MM]`).
  Bare date-times parse as UTC; non-ISO forms JS accepts ("Feb 1 2024")
  are rejected with `Invalid date`.

### WebKit: navigation-wait timeout resolves instead of rejecting
- `wait_for_lifecycle` maps its own timeout to `Ok(())`
  (`backend/webkit/page.rs`), so a `goto` whose lifecycle never fires
  resolves silently instead of throwing `TimeoutError`. Provisional-load
  FAILURES reject correctly (wired via `Playwright.provisionalLoadFailed`);
  only the silent-timeout path diverges.

### `ferridriver test --watch`
- `ferridriver test --ui` is done (it hosts the same UI server as
  `bdd --ui` plus a unix-socket NDJSON bridge, respawning `cargo test` per
  cycle). `test --watch` (re-run cargo on file change, cargo-watch shape)
  is not wired — the `--ui` cycle spawner is the natural base for it.
  Known `--ui` limits: nextest is rejected (it cannot enumerate ferritest
  harness binaries via libtest `--list`); compile errors surface in the
  launching terminal, not the app; libtest binaries in scope run during
  cycles but do not report to the app.

### `bdd --ui` remaining gaps vs Playwright UI mode
- The Network tab is empty while a test runs (HAR entries are built from
  the context log at `stop`, which the context-less live export cannot
  reach) and fills once the finished trace loads.
- The live trace model re-swaps wholesale on each poll (viewer selection
  resets) — coarser than Playwright's byte-incremental append, which needs
  its websocket test-server (uiMode); the standalone vendored viewer only
  supports the postMessage snapshot-feed (a fresh blob URL per poll).

<!-- Append new findings below as they are discovered. Remove items when they land — git history is the archive. -->
