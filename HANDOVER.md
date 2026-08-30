# Finishing the DevTools / Lighthouse parity surface

A work plan, not a diary. `docs/README.md` says handover notes rot, and
it is right, so this one is built to be deleted a piece at a time:
every section below is a unit of work with the check that closes it.
Delete the section when it lands.

Written against `83c1def2`. Every number here was measured with the
command beside it, not taken from anyone's documentation, including
mine — a count of upstream heap tools in `site/docs/comparison/index.md`
said 11 for a week; it is 13.

## Where the line currently is

Gates that exist, and what each one actually proves:

| Command | Proves |
|---|---|
| `just perf-diff` | The 19 trace insights and the Lantern model still agree with the real devtools-frontend engine on four recorded traces |
| `just a11y-diff` | `page.checkAccessibility()` still agrees with Lighthouse's axe verdicts on three fixture pages |
| `just quality-diff` | The eight ported live-DOM audits still agree with Lighthouse |
| `just lh-record` | Re-derives the recordings the two above compare against |
| `just lh-audit <url>` | What Lighthouse concludes about any live page, for exploring |
| `just test` | All of the offline halves, plus 2091 e2e across four backends, 598 BDD, 1100 NAPI, and the e2e typecheck |

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

## 1. `http-status-code`

Lighthouse fails the page when the main document's status is 400-599.
Trivial arithmetic; the whole cost is getting the status.

`performance.getEntriesByType('navigation')[0].responseStatus` is
**undefined on WebKit** (measured: Chromium 200/404, Firefox 200/404,
WebKit `{has: true}` with no status), so a DOM-only gatherer would answer
differently per backend, which is the one thing not to ship. `goto()`
returns a `Response` with the right status on all three engines, so the
status exists — it is just not reachable from `check_page_quality()`,
which runs later and takes no arguments.

Two routes, neither free:

- Cache the last main-frame navigation status on `Page`. Correct only if
  it updates on every main-frame navigation, not just `goto` — link
  clicks, `reload`, and redirects all have to land there.
- Read the context's `network_log` (`ContextRef::network_log_handle`,
  already `pub(crate)`) and find the navigation request for the current
  URL. Populated on all four backends, but pages opened outside a
  context have no log, and redirects make "which request is the main
  one" a real question rather than a filter.

Worth asking first whether it earns its place: a ferridriver caller gets
this status from `page.goto()` directly. Lighthouse needs the audit
because its users do not drive the navigation. I left it for that
reason, not because it is hard.

**Fixture and gate.** A 404 has no file to put in the fixtures
directory, so both servers need to agree on how to serve one. Suggest a
name-driven convention — `<name>.status404.html` served with that status
by both `record-lighthouse.py`'s server and the one in
`crates/ferridriver-perf/tests/lighthouse.rs` — so the two cannot
drift. Then it records like any other page.

## 2. `is-crawlable`

The one with real work in it. Three independent blocking sources,
checked against five bot user agents (`undefined`, `Googlebot`,
`bingbot`, `DuckDuckBot`, `archive.org_bot`), scoring 0 only when ALL
five are blocked:

1. `<meta name="robots">`, or a per-bot `<meta name="googlebot">`, whose
   content contains `noindex` or `none`. Pure DOM.
2. An `X-Robots-Tag` response header, optionally prefixed with a user
   agent (`X-Robots-Tag: googlebot: noindex`). Needs response headers,
   so it shares problem 1's plumbing.
3. `/robots.txt` disallowing the URL. Needs the file fetched and parsed.

Source: `core/audits/seo/is-crawlable.js`, and the parser it leans on is
vendored `robots-parser` — user-agent groups, `*` and `$` wildcards, and
Allow-beats-Disallow specificity, none of which is guessable. Porting
only source 1 would answer wrongly whenever 2 or 3 blocks, silently,
which is worse than not having the audit.

If you take this on, the parser deserves its own differential against
`robots-parser` over a table of (robots.txt, url, user-agent) triples
before it is wired into the audit at all.

## 3. `has-hsts` and `csp-xss` — decide what parity means

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

## 4. `crawlable-anchors`, the listener branch — decide, do not drift

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

## 5. Heap snapshots — 13 tools, the largest single gap

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

## 6. Extensions, PWA, WebMCP, third-party devtools — 12 tools

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

## 7. Smaller, already-stated gaps

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

## 8. `@playwright/mcp` parity is probably not the goal

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
