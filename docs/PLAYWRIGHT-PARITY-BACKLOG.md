# Playwright parity backlog

Tracks Playwright client-API surface that ferridriver does not yet fully
implement, with the concrete blocker for each. Verified against the code
(not memory) as of 2026-07-10.

Landed since the previous revision of this file — do not re-implement:
hit-target interceptor wired into the click gate; `addLocatorHandler` on
QuickJS; `routeWebSocket` (all scopes); route-chain parity
(`route.fallback` chains to the next matching handler, newest-first,
page scope before context scope; context routes reach future pages with
one context-wide `times` budget; `RouteScope` separates
`page.unrouteAll` from `context.unrouteAll`); HAR zip archives +
`attach` bodies + `routeFromHAR({ update: true })` at context level;
`CDPSession` (`browser.newBrowserCDPSession`,
`context.newCDPSession(page)`, send/detach/events, Chromium-only);
`Clock` (full seven-method surface, vendored Playwright engine,
protocol-agnostic, log replay across navigations);
`context.addInitScript` now reaches future pages (per-context
registry); trace recording (`tracing.start/stop/startChunk/stopChunk`)
emitting Playwright format VERSION 8 that `npx playwright show-trace`
opens; `ferridriver bdd --watch` interactive TUI watch mode;
`ferridriver bdd --ui` web UI mode (localhost app: live test tree over
a websocket, run all/failed/file/single/grep, stop, file-change re-runs
with a shared browser, per-test v8 traces forced on and served with
CORS for trace.playwright.dev; the runner-side step-trace recorder in
`crates/ferridriver-test/src/tracing.rs` now emits VERSION 8 too).

## Partial / known limitations

### Trace recording (`crates/ferridriver/src/trace.rs`)
- DOM snapshots ARE captured (`tracing.start({ snapshots: true })`,
  vendored playwright-core 1.58.2 `frameSnapshotStreamer` in
  `src/injected/snapshotter_injected.js`, capture wiring in
  `src/snapshotter.rs`): `frame-snapshot` events with incremental
  NodeSnapshot trees, `beforeSnapshot`/`afterSnapshot` names on every
  traced action, CSSOM-mutation re-serialization, stylesheet resource
  overrides by sha1, and network BODIES attached as sha1 resources
  (`response.content._sha1`) so snapshot subresources render. Child
  frames annotate onto the parent's `<iframe>` via `markIframe`
  (protocol-level frame-owner resolution on all four backends: CDP
  `DOM.getFrameOwner`, WebKit `DOM.resolveNode {frameId}`, BiDi
  `browsingContext.locateNodes` context locator; fired on frame attach
  and re-asserted before every capture), so subframe snapshots inline
  instead of rendering placeholders. Remaining snapshot gap: documents
  already open in frames when tracing starts only pick the streamer up
  on their next navigation (main frames are seeded immediately).
- Console messages and page lifecycle events (`page` / `pageClosed` /
  `dialog` / `download` / `pageError`) are written into the trace from
  the per-page lossless event listener; console lines carry the same
  `page@<id>` the actions and screencast frames use. Console `args`
  previews (`args: [{preview, value}]`) are not captured — text,
  type, and location only.
- `sources: true` embeds each file referenced by an action's stack
  frames as `resources/src@<sha1-of-path>.txt`; BDD steps carry their
  `.feature` file + line as the stack frame, so the viewer's Source
  tab lights up. Protocol actions (`goto`, locator ops) carry no stack
  — there is no JS call site in a Rust-driven session.
- Network entries now carry real `_monotonicTime` / `startedDateTime` /
  `time` and `wait`/`receive` phases derived from the backend timing
  samples (`Request.timing()`), with an ordinal fallback for requests
  without a sample; `dns`/`connect`/`ssl` phases are not emitted (the
  3-field HAR timings struct) and backends that do not fill timing
  samples still fall back to ordinal.
- Screencast capture mirrors Playwright's throttle model: steady-state
  1 frame/200ms, lifted for 500ms at every action boundary
  (`unthrottleDuration` burst).
- Action coverage: every locator operation (via the retry funnel) plus
  `page.goto/reload/goBack/goForward`. Other page-level APIs
  (`screenshot`, `evaluate`, keyboard/mouse, waits) are not yet traced.
- `tracing.start({ screenshots: true })` and `recordVideo` on the same
  page contend for the single screencast stream — whichever starts
  second gets no frames.

### `context.newCDPSession(frame)` (OOPIF form)
- Only the `Page` form is implemented. Playwright also accepts an OOPIF
  `Frame` (attaches to the iframe's own target); ferridriver does not
  track per-frame targets yet.

### `page.routeFromHAR({ update: true })`
- Typed `Unsupported`. Context-level update recording works; the
  page-scoped variant needs per-page attribution in the context network
  log (`Request.frame_id` → owning page).

### Route predicate `times` budget
- A JS predicate route whose predicate rejects now falls through to the
  next handler (chain-correct), but its `times` budget is still
  consumed: the predicate runs inside the wrapped handler, not the
  matcher, because matchers are sync Rust and the predicate is an async
  JS call. Playwright evaluates predicates during matching, before
  `willExpire`.

### HAR recording gaps
- `full` mode emits zero timings (see network timing above) and no
  cookies/serverIP/security sections.
- WebSocket frames are not recorded into HAR (`_webSocketMessages`).
- BiDi records entries but no response bodies: Firefox discards bytes
  for non-intercepted responses (`network.getData` → "no such network
  data") — same hole as Playwright's own BiDi backend.

### Clock date-string parsing
- ISO-8601 only (`YYYY-MM-DD`, `YYYY-MM-DD[T ]HH:MM[:SS[.mmm]][Z|±HH:MM]`).
  Bare date-times parse as UTC (JS `new Date` treats them as local);
  non-ISO forms JS accepts ("Feb 1 2024") are rejected with
  `Invalid date`.

### WebKit: no multiple browser contexts
- `browser.newContext()` rejects on the WebKit backend (single-context
  driver); context-options integration tests skip WebKit for this
  reason. Playwright's WebKit supports contexts via
  `Playwright.createContext` — ferridriver creates one at launch but
  cannot mint more.

### WebKit: navigation-wait timeout resolves instead of rejecting
- `wait_for_lifecycle` maps its own timeout to `Ok(())`
  (`backend/webkit/page.rs`), so a `goto` whose lifecycle never fires
  resolves silently instead of throwing `TimeoutError`. Provisional-load
  FAILURES reject correctly (wired via `Playwright.provisionalLoadFailed`);
  only the silent-timeout path diverges.

### `ferridriver test --watch` / `--ui`
- Watch and UI modes are wired for `bdd` only. The `test` subcommand
  shells out to cargo nextest/cargo for `#[ferritest]` suites, so watch
  there means re-running cargo on change — closer to cargo-watch than to
  the in-process runner loop; the harness `main!` entry could adopt
  `run_watch` / `run_ui` the same way `bdd` did.

### `bdd --ui` remaining gaps vs Playwright UI mode
- Per-test traces are now recorded live by the library recorder (real
  action/step timelines, DOM snapshots, screencast frames, network
  entries with bodies, protocol actions nested under their step spans);
  tests that never touch a browser produce no trace (the recorder is
  context-scoped).
- The trace viewer is embedded (vendored playwright-core 1.61.1 static
  app served at /trace-viewer/) and is THE pane — Playwright UI mode's
  model, with no separate Steps list (the viewer's Actions panel is the
  step list). It is LIVE while the test runs: the shell polls
  `/live-trace?key=<test>` (the UI server exports a snapshot of the
  still-growing recorder via `trace::export_live_snapshot`) and feeds
  each zip to the viewer through its `postMessage({method:'load'})`
  hook, so actions / DOM snapshots / filmstrip update without an iframe
  reload; on finish it loads the finished zip (`?trace=`). The vendored
  viewer's service worker caches models by URL, so a constant live URL
  never refreshes — Playwright's own smooth incremental live needs its
  websocket test-server (uiMode); the postMessage snapshot-feed (fresh
  blob URL each poll) is the equivalent the standalone viewer supports.
  Live LIMITATION: the Network tab is empty while running (HAR entries
  are built from the context log at `stop`, which the context-less
  live export can't reach) and fills once the finished trace loads;
  the model re-swap on each poll is coarser than PW's byte-incremental
  append (viewer selection resets), and the poller only re-feeds when
  the trace actually grew. The viewer is dark (matches the shell) via
  `localStorage.theme='dark-mode'` set same-origin, and its own header
  is hidden via a same-origin injected stylesheet on the source-defined
  stable class `.workbench-loader-header` — never build-hashed names.
  Focus model: viewer widgets own the keys while focused (its
  action-list arrows keep working); arrow keys landing on the viewer's
  body forward to the sidebar. Fully offline; trace.playwright.dev
  remains a secondary link.
- Rust-registry BDD steps (no JS step files) are live boundaries: the
  executor observer opens the `TestInfo` step before the handler runs
  (`on_step_start`), so their protocol actions nest under step spans
  exactly like the QuickJS path.
- Screencast filmstrip: steady-state 1 frame / 200ms with a 500ms
  around-action burst window (Playwright's throttle model).
