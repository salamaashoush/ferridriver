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

### HAR recording gaps
- No cookies / `serverIPAddress` / `_securityDetails` sections.
- `log.pages` entries carry empty `title` and `-1` `pageTimings` (the
  network log tracks no page-load samples); Playwright fills these from
  page lifecycle events.
- WebSocket frames are not recorded (`_webSocketMessages`).
- BiDi records entries but no response bodies: Firefox discards bytes for
  non-intercepted responses (`network.getData` → "no such network data")
  — the same hole as Playwright's own BiDi backend.

### BiDi: native HTML5 drag delivers no `drop`
- Firefox over BiDi starts a native drag from `input.performActions`
  (`dragstart`/`dragenter` fire) but the remote input stack never
  delivers the final `drop`, so `DataTransfer` payloads don't reach the
  target. Same hole as Playwright's own BiDi backend, which carries no
  drag handling at all; the CDP backends intercept via
  `Input.setInterceptDrags` (crDragDrop.ts port) and WebKit's build runs
  the drag natively. `test_script_drag_native_html5` asserts the
  achievable subset on BiDi (drag started + input pipeline alive) until
  the protocol grows drop support.

### `ferridriver test --ui` limits
- nextest is rejected (it cannot enumerate ferritest harness binaries
  via libtest `--list`); compile errors surface in the launching
  terminal, not the app; libtest binaries in scope run during cycles but
  do not report to the app. (`test --watch` is wired — plain re-run of
  the test command on `.rs` changes.)

### `bdd --ui` remaining gaps vs Playwright UI mode
- The Network tab is empty while a test runs (HAR entries are built from
  the context log at `stop`, which the context-less live export cannot
  reach) and fills once the finished trace loads.
- The live trace model re-swaps wholesale on each poll (viewer selection
  resets) — coarser than Playwright's byte-incremental append, which needs
  its websocket test-server (uiMode); the standalone vendored viewer only
  supports the postMessage snapshot-feed (a fresh blob URL per poll).

<!-- Append new findings below as they are discovered. Remove items when they land — git history is the archive. -->
