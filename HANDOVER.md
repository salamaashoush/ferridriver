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

The analysis is the work: a `.heapsnapshot` is a flat typed-array
encoding of nodes and edges with a separate `meta` describing the field
layout, and every tool above is a query over the resulting graph
(dominator tree, retaining paths, shortest path to a GC root). This is
the same shape of job as `ferridriver-perf` and deserves the same
treatment — a `ferridriver-heap` crate with a differential against
devtools-frontend's own `HeapSnapshotWorker`, over checked-in snapshots.
Do not port it against your own reading of the format; that is exactly
how `ferridriver-perf` passed 62 of its own tests and was still wrong in
ten places.

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
