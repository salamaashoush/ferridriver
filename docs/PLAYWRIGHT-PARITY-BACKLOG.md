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
- **DOM snapshots not captured.** No `frame-snapshot` events, no
  `beforeSnapshot`/`afterSnapshot` names on actions — the viewer's
  snapshot pane renders blank (actions, film strip, network, errors all
  work). Needs Playwright's `snapshotterInjected.ts` vendored into the
  injected bundle plus per-action capture plumbing.
- Console messages and page lifecycle events are not written into the
  trace (`console` / `event` entry types exist in the serializer,
  unwired).
- `sources: true` accepted but source files are not embedded
  (`resources/src@<sha1>.txt` + inline stacks).
- Network entries carry ordinal `_monotonicTime` (arrival order), not
  real per-request capture times; HAR `timings` are zeros/-1 because
  per-request timing is not wired through the network log.
- Screencast capture is steady-state throttled (1 frame/200ms) without
  Playwright's unthrottled burst window around each action.
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
- Step traces have no DOM snapshots (same `snapshotterInjected.ts` gap
  as the library recorder above), no screencast frames, and no network
  entries — the viewer shows actions, source locations, and errors; the
  snapshot pane renders blank.
- Step timelines are reconstructed sequentially from durations
  (`TestStep` carries no start timestamps), so concurrent-step overlap
  is not represented.
- `screenshot-on-failure` attachments are captured as in-memory bytes,
  not files, so the UI lists them without a download link.
- No embedded trace viewer; the "Open in trace viewer" link requires
  network access to trace.playwright.dev.
