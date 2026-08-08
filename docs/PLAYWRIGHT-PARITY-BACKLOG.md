# Playwright parity & compatibility backlog

The single tracker for Playwright client-API surface and robustness
behaviours that ferridriver does not yet fully implement, with the
concrete blocker for each. Verified against the code (not memory) as of
2026-07-15. Resolved items are removed, not archived — git history is the
record.

## API surface not yet mapped

### Page methods needing backend plumbing
- `page.workers()` + a public `Worker` type — needs
  `Target.attachedToTarget` worker tracking on CDP/BiDi/WebKit and a new
  class across all three layers (core, NAPI, QuickJS).

### `context.newCDPSession(frame)` (OOPIF form)
- Only the `Page` form is implemented (`context.rs`, script binding
  `bindings/context.rs`). Playwright also accepts an OOPIF `Frame`
  (attaches to the iframe's own target); ferridriver does not track
  per-frame targets yet.

## Partial implementations

### Context-bound `request` (`page.request` / `context.request`)
Cookie bridging (both directions, per redirect hop), live `baseURL` /
`extraHTTPHeaders` / `userAgent` / `ignoreHTTPSErrors` defaults, and the
WHATWG redirect method-rewrite are in (core `ContextBridge`, all three
layers). Remaining gaps:

- `httpCredentials` / `proxy` / `clientCertificates` context options are
  not applied to context-bound requests (the `HttpClient` core has no
  credential/proxy/client-cert support on any path).
- When the context has no `userAgent` option, requests carry reqwest's
  default UA — Playwright falls back to the browser's real UA.
- Cookie persistence routes through the context's active page: a context
  with zero open pages cannot store a response's `Set-Cookie` (dropped
  with a logged warning; reads return empty). Playwright stores
  context-level.
- `request.storageState()` is not exposed on the client
  (`context.storageState()` covers it).
- `page.request === context.request` object identity is not preserved —
  each access mints a wrapper over the same browser-backed state
  (consistent with the `tracing` / `clock` getters).
- `data` routes strings raw and serializable values as JSON, but skips
  Playwright's is-JSON-parsable validation for string bodies under a
  JSON content-type.
- NAPI exposes `status` / `statusText` / `url` on `HttpResponse` as
  getters where Playwright's `APIResponse` has methods. The QuickJS
  binding and `packages/ferridriver-test/index.d.ts` both use methods,
  so only the NAPI surface diverges.
- `maxRetries` and per-request `ignoreHTTPSErrors` reach the core engine
  from both bindings but have no per-option integration test: proving
  them needs a fixture that resets a connection mid-request and one that
  serves a bad certificate, neither of which the axum fixture server can
  do today.
- `request.fetch(pageRequest)` replays the captured request's method and
  headers, and its body where the backend captured one. Firefox (BiDi)
  does not surface post data for a page-initiated `fetch`, so
  `request.postData()` is null there and the replay is body-less.

### Web-platform globals are untyped in e2e specs
`tests/tsconfig.json` sets `types: []` and no `lib` beyond the ES target,
so `fetch` / `Request` / `Response` / `Headers` / `Blob` / `FormData` /
`File` / `AbortController` / `ReadableStream` — all implemented by the
QuickJS runtime — have no declarations a spec can call against. They work
at runtime; only the types are missing, so specs must reach them through
`page.evaluate` strings instead of directly. Declaring them under
`declare global` in `packages/ferridriver-test/index.d.ts` is the fix;
pulling in TypeScript's `DOM` lib is not (it would also declare `document`
/ `window` in spec scope, where they do not exist).

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
Entry enrichment landed: request/response `cookies` (parsed from the raw
Cookie / Set-Cookie headers), `serverIPAddress` + `_serverPort`,
`_securityDetails`, response `httpVersion`, the `dns`/`connect`/`ssl`
timing phases (full mode), and `log.pages[].title` (snapshotted at
flush). Remaining gaps:

- `log.pages[].pageTimings` still carry `-1` for `onContentLoad` /
  `onLoad`. The recorder is post-hoc (rebuilds from the network log at
  flush) and captures no DOMContentLoaded/load timestamps; filling these
  needs the recorder to subscribe to page lifecycle events during
  recording, keyed per page.
- WebSocket frames are not recorded (`_webSocketMessages`). WS frames are
  broadcast-only and transient (`network.rs::WebSocket`), so nothing the
  post-hoc builder reads; capture needs the recorder to subscribe to
  each live socket's frame stream and accumulate with page association.
- `serverIPAddress`, `_serverPort`, and the `dns`/`connect`/`ssl` timing
  phases come from the CDP Network domain only. Firefox/BiDi and WebKit
  do not surface a peer address or timing samples (Playwright's HAR omits
  them there too); those entries carry `-1` timings and no server fields.
- WebKit request `cookies` are empty: the inspector `requestWillBeSent`
  omits the Cookie header and offers no request extra-info / raw-header
  fallback (Playwright's WebKit HAR has the same hole). CDP and BiDi
  populate them.
- Sizes (`headersSize`, `bodySize`, `_transferSize`, content
  `compression`) are still `-1`; `_resourceType` / `_frameref` /
  `_monotonicTime` entry annotations are not written.

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

## Popup tracking residuals

Popup/opener tracking covers `window.open` (including `noopener`) and
the CDP connect flow on all four backends: registration, `'page'`
event, `context.pages()`, `page.opener()`. Remaining known limits:

- CDP popups in a NEW browsing instance (`noopener`, cross-origin
  COOP) answer no session command while parked on
  `waitForDebuggerOnStart`, so the claim watchdog resumes them early
  (~1s) and context config applies just-after-start instead of
  before the first document. Their first document can therefore miss
  context init scripts, and `page.url()` can lag until the next
  navigation (the frame cache misses the pre-registration commit) —
  `evaluate`/locators are unaffected. The fully-ordered fix is
  Playwright's shape: queue the entire init + resume as ONE
  wire-ordered batch (`crPage.ts:548`), which needs the popup claim
  and context config folded into a single command burst.
- BiDi popups run freely (no pause primitive in the spec), so their
  first document can miss init scripts — Playwright has the same
  limitation.
- WebSocket-route install on popups is post-resume everywhere (its
  mock installs by evaluating into the live document).

## WHATWG fetch residuals

The `BodyInit` union is extracted in one place
(`bindings/body_init.rs`) for `fetch`, `new Request` and `new Response`;
`Headers` and the `Request`/`Response` header fields are the core
`fetch::Headers` list; the `fetch` global builds a core `WhatwgRequest`
instead of borrowing the Playwright `RequestOptions` bag; and a
`ReadableStream` request body is streamed onto the socket
(`bindings/streams.rs::to_byte_stream` pumps it off the VM thread into
`fetch::channel_stream`). Point-by-point spec behaviour is pinned by
`tests/fetch_conformance.rs`, body handling by `tests/fetch_body_init.rs`.

Remaining:

- `Request` / `Response` still hold `Vec<u8>` bodies plus a separate
  `net` handle rather than one `fetch::Body`. Collapsing them means
  giving `fetch::Body` a peek/clone story the JS single-use rules can
  sit on; the header half of this item is done.
- A streamed request body cannot follow a redirect (the engine returns
  `RedirectRefused`) — the stream is consumed by the first hop and
  cannot be replayed. Browsers behave the same way, so this is a
  documented limit rather than a gap.
- `CompressionStream` / `DecompressionStream` cover the three formats the
  Compression Streams spec defines (`gzip`, `deflate`, `deflate-raw`).
  Brotli and zstd are deliberately absent — not in that spec.

<!-- Append new findings below as they are discovered. Remove items when they land — git history is the archive. -->
