# Finishing the DevTools / Lighthouse parity surface

A work plan, not a diary. `docs/README.md` says handover notes rot, and
it is right, so this one is built to be deleted a piece at a time:
every section below is a unit of work with the check that closes it.
Delete the section when it lands.

Written against `83c1def2`, minus the two sections that have since
landed. Every number here was measured with the command beside it, not
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
| `just heap-diff` | Our reading of a `.heapsnapshot` still agrees with DevTools' own heap engine, node by node over a sample of 200 |
| `just lh-record` | Re-derives the recordings the two above compare against |
| `just lh-audit <url>` | What Lighthouse concludes about any live page, for exploring |
| `just test` | All of the offline halves, plus 2131 e2e across four backends, 598 BDD, 1102 NAPI, and the e2e typecheck |

Ported and gated: all 67 axe wrappers (by running the engine, not the
wrappers), and eight audits in `crates/ferridriver/src/audits.rs` —
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

## 3. Heap snapshots — 13 tools, the largest single gap

`chrome-devtools-mcp` 1.8.0 exposes 13 (verify:
`node --input-type=module -e "import {createTools} from
'./build/src/tools/tools.js'; console.log(createTools({slim:false})
.map(t=>t.name).filter(n=>/heapsnapshot/.test(n)).length)"` in an
unpacked copy of that package):

`take_heapsnapshot`, `close_heapsnapshot`, `compare_heapsnapshots`,
`query_heapsnapshot_objects`, and `get_heapsnapshot_` ×
{`summary`, `details`, `edges`, `class_nodes`, `dominators`,
`duplicate_strings`, `object_details`, `retainers`, `retaining_paths`}.

The capture is `HeapProfiler.takeHeapSnapshot` over CDP, which is
Chromium-only — WebKit and Firefox have their own heap formats and
Playwright exposes neither, so `Unsupported` on the other three is the
honest shape here and does not violate the no-divergence rule, because
the feature is absent rather than approximated.

### What has landed

The harness and the format layer, in `crates/ferridriver-heap`. The
engine this is a port of turned out to be drivable: the real
`devtools-heap-snapshot-worker.js` ships inside `chrome-devtools-mcp`
and takes the same `HeapSnapshotWorkerProxy` its own tools use, so
`just heap-diff` runs it over a checked-in snapshot and records what it
concluded, and `cargo test -p ferridriver-heap --test differential`
replays that offline in `just test`.

It earned its place on the first run. 198 of 200 sampled nodes agreed
immediately; the two that did not disagreed on DETACHEDNESS, because
what DevTools reports is not the field V8 wrote — `propagateDOMState`
walks attachment through the graph, stops at the first non-native node,
and renames what it finds to `Detached <name>`. A port written against
a reading of the format would have shipped the raw field and been wrong
about exactly the objects a leak hunt is looking for.

The snapshot rather than the page is checked in
(`tests/fixtures/leaky.heapsnapshot.gz`, 550KB), because object ids and
how much of V8 is alive differ run to run; `just heap-diff --capture`
re-takes it deliberately.

### What is left

Everything downstream of the format, in the order `HeapSnapshot.ts`
does it — the order is load-bearing, since shallow sizes move from
owned nodes onto their owners BEFORE retained sizes propagate:

1. `calculateFlags` — detached DOM, queriable, page-owned.
2. `calculateShallowSizes` — moves an owned array or hidden node's size
   onto its owner, which changes every size downstream.
3. `initEssentialEdges` — which edges count for dominance. Weak edges
   never retain; a WeakMap value is retained by key and table together
   and only the key's edge counts; shortcuts at the root are markers.
4. Lengauer-Tarjan dominators, then retained sizes propagated up in
   reverse DFS order, then `buildDominatedNodes`.
5. `calculateDistances` — a two-phase breadth-first walk, user roots
   first and then everything else.
6. Node naming — cons strings are assembled by walking their parts, and
   a plain `Object` is named from its constructor.
7. `getStatistics`, which needs all of the above.

All seven have landed and agree with the engine on both fixtures. Every
field the engine reports is compared: ids, types, detachedness, names,
self and retained sizes, distances, and all eight statistics.

### The fixture that a browser will not give you

A snapshot taken over CDP has NO USER ROOTS. The synthetic root's only
child is `(GC roots)` and every child of that is synthetic too.
Measured over http and file, with and without `exposeInternals`,
`captureNumericValue` and `treatGlobalObjectsAsRoots`, and through
Puppeteer's own `captureHeapSnapshot`, which is what
`chrome-devtools-mcp` calls.

Upstream reads that as "internals were exposed" and skips
`calculateShallowSizes`, so over a captured snapshot three passes never
run on either side: the shallow-size transfer, the page-object marking
that feeds the essential-edge filter, and the first half of the
distance walk. Agreement there is agreement about a branch neither side
takes, which is not evidence.

So there is a second fixture,
`tests/fixtures/handmade.heapsnapshot`, built by
`scripts/perf-diff/make-heapsnapshot.mjs` the way upstream's own
`HeapSnapshot.test.ts` builds snapshots. Twenty nodes, each there to
make one branch tell itself apart from its absence: a backing store
with one retainer and another with two, a hidden node the JS-array
branch would otherwise claim, an ephemeron pair, a weak-only retainer,
a detached subtree. It is still a differential -- the real engine
analyses it too.

It has found two bugs so far. The ephemeron name parser matched
nothing, so both edges of a `WeakMap` pair counted and the value came
out dominated by the window rather than by its key. And a plain
object's label carried an ellipsis for properties that had all fitted,
because the cursor walking in from the end was clamped where upstream
lets it cross the start.

Nine branches are confirmed live by deleting each and watching the gate
go red: the shallow-size transfer, the statistics early exit for hidden
nodes, the single-retainer test in the JS-array measurement, the
`WeakMap` table-edge exclusion, both naming rules, the `__proto__`
skip, the property-name escaping, and the label budget. Do the same for
anything added here; three of those passed with the code removed until
this fixture existed.

### The query layer

The node-addressed reads the tools are built from have landed and are
compared whole against the engine's own providers: `object_info`
(`get_heapsnapshot_object_details`), `dominator_chain`
(`get_heapsnapshot_dominators`), `edges_of` (`get_heapsnapshot_edges`)
and `retainers_of` (`get_heapsnapshot_retainers`), plus
`ordinal_for_id`, over 25 nodes per fixture.

Three more bugs came out of that comparison, all of them the kind that
looks right until something else answers the same question:

- A retaining edge is read from the other end. Its result embeds the
  node doing the RETAINING, not the one retained, and we had the target.
- `retainingEdgesFilter` drops three kinds -- invisible edges, the root
  as a retainer, and weak edges -- so a node held only by those reports
  no retainers. We reported them.
- `markQueriableHeapObjects` seeds from the NODE's `isUserRoot` (only
  "not synthetic"), not the snapshot's (which also admits
  `(Document DOM trees)`). Seeding from the wrong one marks the whole
  DOM queriable.

Still to write: `aggregatesWithFilter` and the class-node provider
(`get_heapsnapshot_class_nodes`, and the aggregate half of
`get_heapsnapshot_details`), `getRetainingPaths`, `getDuplicateStrings`,
`queryObjects`, and `calculateSnapshotDiff` for
`compare_heapsnapshots`. Then the 13 tools themselves, the CDP capture
with `Unsupported` on the other three backends, and the three binding
layers.

## 4. Extensions, PWA, WebMCP, third-party devtools — 12 tools

`install_extension`, `list_extensions`, `reload_extension`,
`trigger_extension_action`, `uninstall_extension` (5);
`install_pwa`, `launch_pwa`, `uninstall_pwa` (3);
`list_webmcp_tools`, `execute_webmcp_tool` (2);
`list_3p_developer_tools`, `execute_3p_developer_tool` (2).

All Chromium-only, all thin protocol wrappers rather than analysis.
Cheap per tool, and the least interesting work on this list — but it is
the row where `site/docs/comparison/index.md` currently says plainly
that if that is your job, use `chrome-devtools-mcp`. Closing it changes
what that page can claim.

Note the naming collision before you start: `ferridriver_extensions` is
already an MCP tool, and it means ferridriver's OWN extension packages,
not Chrome's.

## 5. Smaller, already-stated gaps

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
- **Codegen has no picker window.** `page.pickLocator()` exists as an
  API and is not wired into `ferridriver codegen`.
- **`SlowCSSSelector`** disagrees with upstream on purpose.
  `STATE_DIVERGENCES` in `crates/ferridriver-perf/tests/differential.rs`
  records why. Do not "fix" it to reach nineteen out of nineteen.

## 6. `@playwright/mcp` parity is probably not the goal

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
