# Backend quirks

Behaviour of the browsers, protocols and bindings that the protocol docs do
not state and that each cost a debugging session to find. Read the section for
the layer you are about to change.

## All backends

- **The utility-script `JSON.stringify` wrapper keeps the wire clean.** If the
  page-side wrapper returns the raw isomorphic wire object, CDP and BiDi
  re-serialise it through their own `RemoteValue` format and corrupt its tags.
  The wrapper stringifies inside the page so the backend ships a flat string,
  and Rust parses it back into `SerializedValue`. Canonical wrapper:
  `UTILITY_EVAL_WRAPPER` in `crates/ferridriver/src/backend/cdp/mod.rs`, shared
  with BiDi and WebKit.
- **Version strings come from the protocol.** BiDi takes `browserName` and
  `browserVersion` from the `session.new` capabilities; WebKit reports
  `webkit-playwright/{revision}` from the launcher; CDP reports what the
  protocol returns, unreshaped.

## CDP

- **`Input.dispatchTouchEvent` needs `Emulation.setTouchEmulationEnabled`
  first**, or DOM touch listeners never fire. It surfaced only as a failure in
  the full suite that passed in isolation.
- **`Runtime.bindingCalled` is consumed on the same task as
  `executionContextCreated` and `executionContextDestroyed`.** A separate
  broadcast subscription races the frame-context tracker: a binding call from
  a fresh iframe document resolves against a map that has not caught up,
  misroutes to the main frame, and strands the caller's promise. Resolution
  happens inline in the tracker loop; the user callback is spawned.
- **Lightpanda quirks behind `CdpFlavor::Lightpanda`.** Lightpanda reports
  `Chrome/<v>` (not `HeadlessChrome/<v>`) from `Browser.getVersion`, so
  `headful` must be forced `false` and `Browser.getWindowForTarget` /
  `setWindowBounds` skipped. It omits `frame.name` from `Page.frameNavigated`
  and `Page.getFrameTree` (and leaves `iframe.contentWindow.name` undefined),
  so child frame names are read from `DOM.getFrameOwner` + `DOM.describeNode`
  and synced at `finish_navigation` when unnamed children are present. Its
  `Fetch.requestPaused` keeps `#fragment` on request URLs (and matches
  `Fetch.enable` `urlPattern` against URLs with fragments) and truncates the
  last byte of the paused `Cookie` header; `page_interception.rs` strips
  fragments and appends `*` to narrowed Lightpanda patterns, while
  `InterceptedRequest::fetch` drops inherited `cookie` headers so
  `HttpClient` attaches authoritative context cookies. Its
  `Page.captureScreenshot` supports PNG only, so JPEG and WebP are transcoded
  from PNG in Rust. Its `IDBObjectStore.add` rejects a second `undefined`
  argument on stores with an inline `keyPath`, so `storageScript.ts` passes
  the key argument only when defined.

## WebKit

- **Per-target state is dropped on every cross-document navigation.** The
  backend swaps the target session, so anything applied through target-session
  commands (`Network.setExtraHTTPHeaders`, `Page.setEmulatedMedia`,
  `Page.overrideUserPreference`, `Page.setForcedColors`) is stashed on
  `WebKitPage` and replayed in `handle_provisional_target_created`, mirroring
  Playwright's `_updateState`.
- **`Page.overrideUserPreference` rejects `value: null`, and a name-only call
  does not clear a live override.** Removal is Playwright's full replay: always
  send all five emulation commands, `setEmulatedMedia` first, and remove a
  preference by not re-asserting it.
- **Print media suppresses `prefers-color-scheme` overrides.** While
  `Page.setEmulatedMedia` is `print` the colour-scheme preference stops
  matching, and returns when media is cleared. Engine semantics: Playwright's
  own `page-emulate-media` spec never asserts the print and dark combination.
- **Colour-scheme tests are baseline-aware.** The Playwright WebKit build reads
  the real macOS appearance, so a dark system starts with
  `prefers-color-scheme: dark` true, while headless Chromium starts light.
  Capture the baseline before overriding and assert it returns.
- **Wheel events need a painted frame first.** `Input.dispatchWheelEvent` is
  dropped silently unless preceded by `Page.updateScrollingState` and a
  double `requestAnimationFrame` round trip (`WebKitPage::wait_for_compositor_frame`);
  one frame is not enough right after `setContent`.
- **`Page.snapshotRect` produces PNG only.** `screenshot({ type: 'jpeg' })`
  transcodes in Rust through the `image` crate (default quality 80, like
  Playwright's jpeg-js path); webp is `Unsupported`, Chromium-only in
  Playwright too.
- **The utility world is announced right after the normal world for the same
  frameId.** `Runtime.executionContextCreated` fires twice per document,
  `type: "normal"` then `type: "user"` named `__playwright_utility_world__`. A
  frame-to-context map that keeps the last write anchors frame-scoped
  evaluates in the utility world, where page globals such as the WebSocket
  mock's `__pwWebSocketDispatch` do not exist. Track `type == "normal"` only,
  mirroring the CDP tracker's `isDefault` filter.
- **A page the browser closes is reported only by
  `Playwright.pageProxyDestroyed`**, on the browser session. `window.close()`
  sends nothing on the page's own sessions that says the page is gone, so
  without that event the page stays open to ferridriver, never emits
  `close`, and stays in `context.pages()`. Mirrors
  `wkBrowser._onPageProxyDestroyed`.
- **`callFunctionOn` is synthesised through an inline `evaluate`.** Every handle
  is reachable from page-side JS through the page-side handle registry, so no
  extra protocol method is needed.

## BiDi

- **BiDi marks referenced elements with `data-fdref="<id>"`.** `innerHTML`
  serialises as `<b data-fdref="4">world</b>` where CDP and WebKit return
  `<b>world</b>`. Assert on substrings.
- **Playwright's own BiDi backend is the reference** for what BiDi can and
  cannot do (`packages/playwright-core/src/server/bidi/`). It drops some
  features (`referer` on `goto`) and works around others with
  `network.setExtraHeaders`; matching or beating it is the bar.

## QuickJS bindings

- **The VM is single-threaded and is entered only from its own thread.**
  `tokio::spawn` plus `async_with!` from an event callback drives the
  interpreter concurrently with the script's own execution and dies with a
  silent SIGSEGV (the MCP child exits; tests see a broken pipe). Buffer events
  through an unbounded mpsc and drain them with a `ctx.spawn` pump on the
  interpreter thread: `ensure_event_pump` in `bindings/page/callbacks.rs`, and the pump in
  `bindings/sidecars.rs`.

## Event plumbing

- **State-mutating protocol consumers use the lossless taps.** Tolerating
  `Lagged` on a tokio broadcast still loses the dropped events, and a lost
  `Fetch.requestPaused`, `executionContextCreated` or `downloadWillBegin`
  corrupts tracker state or hangs the page. Use the wire-ordered unbounded
  taps: CDP `tap_event_methods` and `tap_event_domains`,
  `BidiTransport::tap_events`, WebKit route subscriptions. Broadcast is for
  best-effort fan-out only: screencast frames, transient waits.
- **Broadcast loops use `crate::events::recv_tolerant`.** A bare
  `while let Ok(e) = rx.recv().await` exits on the first `Lagged` during an
  event storm and silently disables the listener for the rest of the page's
  life; `recv_tolerant` skips `Lagged` and exits only on `Closed`.
- **Page-event teardown listeners stay synchronous.** A `PageEvent` consumer
  that awaits slow work (an evaluate into a dead frame waiting on frame-context
  resolution) processes navigation events late and tears down state the new
  document just created. Do the bookkeeping inline; spawn anything slow.

## Processes and test servers

- **A browser child's piped stderr is drained for its whole life**
  (`backend/process.rs::drain_child_stderr`). Chrome logs every renderer
  console message to stderr; at 64KB unread, about 1000 `console.log` calls,
  it blocks in `write(2)` on the thread routing DevTools traffic and every CDP
  command and event freezes, observed as 30s `Runtime.evaluate` timeouts.
  Playwright drains unconditionally (`processLauncher.ts`). Regression test: the console storm case in
  `tests/integration/core-page-contracts.test.mjs`.
- **Test HTTP servers spawn a thread per connection and reply
  `Connection: close`.** WebKit opens a speculative preconnection that carries
  no request and holds it idle for about 60s; a one-connection-at-a-time
  accept loop blocks reading it while the real request starves, observed as
  30s `goto` timeouts.
