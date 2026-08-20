# Playwright parity & compatibility backlog

The single tracker for Playwright surface — client API, robustness
behaviours AND the test-runner surface — that ferridriver does not yet
fully implement, with the concrete blocker for each. Verified against the
code (not memory) as of 2026-08-19. Resolved items are removed, not
archived — git history is the record.

The scope line used to say "client-API surface and robustness
behaviours" while the contents already tracked runner-side gaps. Both
belong here; a reader looking for the runner's parity state should not
have to discover that the tracker covers it anyway.

## Test-runner surface

### `toHaveScreenshot({ signal })`
- Playwright takes an `AbortSignal` that cancels the assertion's polling
  (`LocatorAssertions.toHaveScreenshot`). Every other option on that bag
  is implemented; this one needs a cancellation token threaded through
  `ferridriver-expect`'s poll loop and an `AbortSignal` lowered from the
  JS host, which no matcher takes today.

### A screenshot `mask` naming an element inside an iframe
- A mask resolves through the injected selector engine in ONE document,
  so every selector engine works but a Locator that crosses an
  `enter-frame` hop matches nothing. Playwright resolves each mask
  entry in its own frame (`server/screenshotter.ts::_maskElements` sends
  one `callOnSelector` per `{ frame, selector }`), which needs the frame
  identity carried from the caller instead of just the selector string —
  the JS host lowers a Locator to its selector today and the frame is
  lost at that boundary.

### `screenshot({ scale: 'css' })` on Firefox
- `browsingContext.captureScreenshot` has no scale parameter, so the
  capture is always at device pixels. Playwright's BiDi backend takes the
  same argument and drops it the same way
  (`bidi/bidiPage.ts::takeScreenshot`), so this is the engine's ceiling
  rather than a shortcut; CDP and WebKit both honour it.

### A bare relative reporter path is not resolved against the config's own directory
- `reporter: [{ name: './my-reporter.ts' }]` resolves against the cwd and
  then `testDir` (`ferridriver-script/src/reporter.rs::resolve`), not the
  config's own directory as Playwright does, so `--config
  sub/ferridriver.config.ts` reports "neither a known reporter name nor a
  file that exists". `require.resolve('./my-reporter.ts')` sidesteps it
  (it returns an absolute path), which is what a Playwright config
  typically writes anyway.

### VRT baseline layout for a BDD suite migrating from playwright-bdd
- playwright-bdd runs generated `.feature.spec.js` leaves under a
  per-project `testDir`, so an existing suite's committed screenshot
  baselines sit at paths native BDD does not reproduce. Either reproduce
  that leaf shape, or land a one-time baseline move plus the matching CI
  snapshot-path change. The driving acceptance suite deliberately does
  not cover it.

### `use`-level worker options in a spec's `test.use`
- `trace`, `video` and `screenshot` are WORKER options in Playwright, so
  setting one from a spec's `test.use({ … })` needs a worker whose
  options differ — the runner resolves them per config/project only.
  `actionTimeout`, `navigationTimeout` and `baseURL` are test-scoped and
  do work from a spec.
- `video: { show }` (the action/test overlay upstream draws onto the
  recording) parses and is ignored; ferridriver's recorder has no
  overlay.

### `test.extend` restoring an option default with `undefined`
- Playwright's `_appendFixtureList` walks `optionOverride` so that
  extending with `undefined` restores the original default rather than
  setting the value to `undefined`. ferridriver treats it as a value.

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

### `selectors.register` scope and `contentScript`

`selectors.register(name, script)` and `selectors.setTestIdAttribute()`
are implemented and reach every document, but two properties differ from
Playwright because its workers are separate PROCESSES and ferridriver's
share one:

- The registry is process-global. A second registration of the same name
  with an IDENTICAL script is therefore a no-op (every worker evaluates
  the same spec file); only a conflicting script raises Playwright's
  "has been already registered". `setTestIdAttribute()` likewise sets a
  process-wide default, so a spec calling it mid-run affects workers that
  did not. `use: { testIdAttribute }` is per context and is unaffected —
  that is the isolated path, and the one a project should use.
- `contentScript` is accepted and recorded but cannot change anything:
  ferridriver evaluates every selector engine in the page's own world,
  having no isolated world at all. `contentScript: true` is therefore
  honoured exactly; `contentScript: false` (Playwright's default) runs in
  the page's world too, so an engine is never isolated from page globals.
  Closing this means isolated-world execution across all four backends,
  which is a backend-architecture change, not a selector one.

### `expect` block: what the keys reach

`[test.expect]` and a project's own `expect` block carry every key
Playwright has, resolved the way it resolves them (a project's block
REPLACES the config's whole object). Two of those keys have nothing to
bite on yet, and it is the matcher that is missing, not the config:

- `expect.toMatchSnapshot.{threshold,maxDiffPixels,maxDiffPixelRatio}`
  are image-comparison budgets, and `toMatchSnapshot` only takes a string
  or a locator's text today — `expect(buffer).toMatchSnapshot('x.png')`
  needs a byte subject through the expect seam before an image budget can
  apply. The screenshot equivalents (`expect.toHaveScreenshot.*`) are
  honoured.
Also per-call only, as upstream (`NonConfigProperties`): `clip`, `mask`,
`maskColor`, `fullPage`, `omitBackground`, `signal` — of which
ferridriver takes `clip`, `mask` and `maskColor`.

### `test.use` cannot register a fixture, only set an option

A `use` bag — from the config, a project, a file's `test.use` or a
describe's — sets the value of a fixture registered with
`{ option: true }`, in Playwright's precedence order. Playwright's
`test.use` is implemented as a fixture LIST appended to a new pool per
suite level, so it can additionally override a non-option fixture, or
introduce a name the chain never registered, for the tests under it.
Ferridriver's chains are decided at collection and identified by index
(`fixture_sets`), which the collection/worker determinism check and the
plan digest both read — giving a describe its own chain is a change to
that identity, not a new branch in the resolver, so it belongs in its
own phase rather than in the option-fixture work. Setting a non-option
fixture from a CONFIG `use` block is refused with Playwright's own
message, which is upstream behaviour and unaffected.

### `route.fulfill` / `unroute` residuals
`fulfill` takes `status`, `headers`, `contentType`, `body` (string or any
byte source), `json`, `path` (read through the session sandbox) and
`response`; `continue` takes a byte `postData`; `unroute(url, handler)`
removes exactly the registration a handler installed. What Playwright
still does and ferridriver does not: replaying an `APIResponse` that came
from the SAME connection by reference (`fetchResponseUid`) — the body is
copied instead, which costs a buffer for a large response — and
`page.route` does not intercept traffic the HTTP client (`request`
fixture) makes, only page traffic.

### `@ferridriver/test` module surface
`mergeTests`, `_baseTest`, `chromium` / `firefox` / `webkit`, `request`
and the module-object-is-the-test-function shape are served (also under
`@playwright/test` and `playwright/test`). Still absent, and therefore
NOT exported rather than exported as `undefined`: `selectors`, `devices`,
`defineConfig`, `mergeExpects`, `errors` (Playwright's `{ TimeoutError }`),
`by`, and the `_electron` / `_android` / `_utilityTest` internals.

### `test.extend` type inference needs explicit type arguments
`test.extend<{ myFixture: string }>({ … })` infers correctly;
`test.extend({ … })` without the type argument falls back to the
constraint and the new fixture names are lost from the body's parameter
type. Playwright's own declarations have the same shape and its docs also
pass the type argument, so this is a papercut rather than a divergence —
but the inference site (`FixtureValue<T[K], TFixtures & T>` in
`packages/ferridriver-test/index.d.ts`) is where a fix would go.

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

## Node `console` residuals

`crates/ferridriver-script/src/console_fmt.rs` implements every method on
<https://nodejs.org/api/console.html> and the `util.format` specifiers
(`%s %d %i %f %j %o %O %c %%`), each verified against Node v22 output.
The value renderer is `util.inspect`-shaped but is not a port of it.

Remaining, in rough order of how visible each one is:

- **No layout engine.** Node breaks a rendering across lines once it
  exceeds `breakLength` (80) and column-aligns long numeric arrays
  (`groupArrayElements`); we always emit one line. A wide object prints
  as one long line, and a 100-element array as one very long line. This
  is the largest single remaining piece — Node's `reduceToSingleString`
  plus the grouping pass — and it is cosmetic, not incorrect.
- **No circular-reference tracking.** Node prints
  `<ref *1> { n: 1, self: [Circular *1] }`; we repeat the structure until
  the depth cap and show `{ n: 1, self: { n: 1, self: [Object] } }`.
  Bounded (it cannot hang) but misleading. The blocker is object
  identity: rquickjs keeps `Value::get_ptr` `pub(crate)`, so this needs a
  JS-side `Map` of ancestors threaded through the renderer.
- `ArrayBuffer` renders as `ArrayBuffer { byteLength: 8 }` rather than
  Node's `[Uint8Contents]: <00 00 …>` form.
- No `showHidden` / `showProxy` / getter evaluation / symbol-keyed
  properties, and no `numericSeparator` or `maxStringLength`. `%o` accepts
  Node's depth but not its `showHidden`.
- A rejected promise shows `<rejected> Error: x` without the stack; the
  renderer only prints stacks for a top-level error.
- **`console.Console` is deliberately absent.** It binds a console to
  caller-supplied writable streams, and the sandbox exposes no writable
  stream to bind — implementing it would mean accepting the argument and
  ignoring it. Its options bag (`inspectOptions`, `colorMode`,
  `groupIndentation`, `ignoreErrors`) goes with it; group indentation is
  fixed at Node's default of 2.
- Extension-authored ANSI is stripped along with page-bridged output:
  every JS-supplied string passes through `strip_ansi` so page content
  cannot smuggle terminal control codes into logs. An extension that
  colours its own output (some host tools do) loses it.
  Distinguishing the two sources needs a trusted-output channel.

## e2e suite: load-correlated roaming flake

`ferridriver test` fails one test per run, roughly a third of the time,
and it is a different test each time. Measured 2026-08-10 on macOS, 6
workers, by running the full suite repeatedly on the same machine:

| Build | Runs | Green | Failing test |
|---|---|---|---|
| `feat/run-console-streaming` | 5 | 3 | `network > request_existing_response`, `network > route_from_har` (30s timeout) |
| clean `main` | 3 | 2 | `events > context_popup_page_event_and_opener` (8s timeout) |

Comparable rates on both sides, so it is not something the console /
sweep work introduced — the point of measuring a clean baseline was to
settle exactly that. Every red run is also a slow run (95.5s and 102.7s
against ~70s for green ones), and each failing test passes 4/4 across all
backends when run in isolation.

The common shape is a wait that expires under contention — a navigation
response that arrives late, a `waitForURL` that misses its window. That
points at the suite's worker count versus the machine rather than at any
one test, but it has not been root-caused. Before blaming a diff for an
e2e failure, re-run: one red run out of three proves nothing on its own.

Related: the BDD suite has its own known load-correlated hang, same
advice.

<!-- Append new findings below as they are discovered. Remove items when they land — git history is the archive. -->
