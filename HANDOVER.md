# Finishing the DevTools / Lighthouse parity surface

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
| `just heap-diff` | Our reading of a `.heapsnapshot` still agrees with DevTools' own heap engine, over four snapshots in two pairs: 200 nodes apiece, every node-addressed query, the searches, and what a diff of each pair reports |
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
| `Extensions.*` | absent | present, once `--disable-extensions` is dropped and `--enable-unsafe-extension-debugging` added |
| `PWA.*` | absent | present (`getOsAppState` answers "Unknown web-app manifest id", which is the domain replying) |
| `WebMCP.*` | present | present |

**`Extensions` and `PWA` are headful-only**, and every e2e project runs
headless. That is not a backend-dependent verdict to route around; it is
a browser mode the suite does not have, and closing the row means
deciding how a headful-only spec runs at all. It also needs
`ignoreDefaultArgs` on the launch surface, which the Rust core has
(`LaunchOptions::ignore_default_args`) and NEITHER binding layer
exposes. That is a parity gap of its own and the smallest first step
here.

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

- **The heap snapshot's named node filters.** `aggregatesWithFilter`
  takes a `NodeFilter`, and `get_heapsnapshot_details` /
  `get_heapsnapshot_class_nodes` expose seven names for it:
  `objectsRetainedByDetachedDomNodes`, `objectsRetainedByConsole`,
  `objectsRetainedByEventHandlers`, `objectsRetainedByContexts`,
  `sharedNativeContext`, `noNativeContext` and
  `attributedToSpecificNativeContext`. Ours is the unfiltered call,
  which is what a default `NodeFilter` produces. The same pass would
  bring `getNativeContextSizes` and `getRetainedByContextSummary`, which
  are the other half of `get_heapsnapshot_summary`. All of it is in
  `HeapSnapshot.ts::createNamedFilter`, and all of it is comparable
  through `just heap-diff` the way everything else there is.
- **Source-mapped console stack traces.** `chrome-devtools-mcp` resolves
  frames through source maps; we report raw positions.
- **`DuplicatedJavaScript`** detects byte-identical bodies at several
  URLs, not duplicated modules inside different bundles. Needs source
  maps, which are not in the trace.
- **`LegacyJavaScript`** has no source-map pass, so a heavily minified
  bundle may under-report.
- **`CLSCulprits`** does not attribute non-composited animations.
- **Codegen has no picker window.** `page.pickLocator()` exists as an
  API and is not wired into `ferridriver codegen`.
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
