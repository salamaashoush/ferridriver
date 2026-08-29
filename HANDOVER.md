# Handover

Written 2026-08-28. Delete this file once it has been read; it describes
one session, not the project.

## What this session did

Started from "why use ferridriver over chrome-devtools-mcp or
playwright-mcp", which turned into: close the gap those two have, in
Rust.

Eleven commits on `main`, oldest first:

| commit | what |
|---|---|
| `d205a139` | your `run --fresh` work, committed (I only refactored `run()` to clear a `too_many_lines` failure it introduced) |
| `4623d577` | dependency update; crypto crates off release candidates |
| `942e02e6` | trace capture + `ferridriver-perf` foundation, 4 insights |
| `2101679d` | LCP breakdown/discovery, cache, font display, viewport |
| `a9fbf4b3` | INP, DOM size, forced reflow |
| `f79b2a80` | image delivery, dependency chains, CLS culprits |
| `191f7ff5` | the last four insights; all 19 present |
| `5edcc27d` | the Lantern simulator |
| `2745ab39` | **reconciliation against devtools-frontend** |
| `78431816` | parse optimisation |
| `b7bb5194` | Lantern's CPU graph |
| `324623c9` | `generateLocator` |
| `4e8d3c7c` | `networkidle` fix + MCP settle |

## Verification state

All green as of `4e8d3c7c`:

- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace --exclude ferridriver-cli`
- `cargo test -p ferridriver-cli -- --test-threads=1`
- 2011 e2e tests across all four backends
- 1090 bun tests, 62 `ferridriver-perf` tests

The last two Rust suites finished after the commit was made and are
confirmed clean; `4e8d3c7c`'s message says they were unconfirmed, which
was true when it was written and is not now.

## The differential harness — read this first

The single most valuable thing here. I built `ferridriver-perf` against
my own reading of upstream and my own fixtures, and it looked finished.
Running it against the real devtools-frontend engine on the same trace
files found four defects I had no other way to see, including a false
positive that told people to delete code they need.

It is now wired in: `just perf-diff` runs it, and
`cargo test -p ferridriver-perf --test differential` compares against
the recorded verdicts on every `just test`.
`scripts/perf-diff/README.md` has the detail.

```
scripts/perf-diff/engine.mjs                        drives the real engine on a saved trace
scripts/perf-diff/fixture-server.py                 the fixture the traces came from
crates/ferridriver-perf/tests/fixtures/
  plain-load.json.gz                                differential trace 1 (plain load)
  interaction-and-thrash.json.gz                    differential trace 2 (interaction + thrash)
  *.upstream.json                                   what the real engine says about each
```

The engine itself is not vendored: `npm install chrome-devtools-mcp@1.8.0`
brings the prebuilt devtools-frontend bundle with it.

`diff.mjs` needs one non-obvious bootstrap or every insight fails with
"No LanguageSelector instance exists yet": create the `DevToolsLocale`
instance BEFORE registering locale data. It is in the file.

Ours is produced by `cargo run -p ferridriver-perf --example report --
trace.json`.

Current agreement: **metrics exact to three decimals on both traces,
insights 18/19.**

The one difference is deliberate and should stay. Upstream reports
`SlowCSSSelector` as "pass" when the trace carries no selector
statistics, which claims a result it has no data for; ours reports "not
measured".

## What is done

All 19 devtools-frontend insights, plus the Lantern simulator: graph,
network analyser, TCP slow-start, connection pool, DNS cache, the
event-driven loop, the CPU graph, and the FCP subgraph. Render-blocking
savings match upstream (74 vs 75, 77 vs 77).

Speed on the same input: 484KB trace 1.6ms against upstream's 48.9ms;
13MB trace 28ms against 191ms.

## What is NOT done

- **The 26 live-page SEO/security audits.** `doctype`,
  `meta-description`, `crawlable-anchors`, `link-text`,
  `image-aspect-ratio`, `image-size-responsive` and
  `paste-preventing-inputs` are reachable through snapshot mode and
  checkable with `just lh-audit`; `is-on-https`, `csp-xss`, `has-hsts`,
  `canonical`, `is-crawlable` and `http-status-code` need a network log,
  so verifying those needs navigation mode and a harness that does not
  exist yet.
- **Lighthouse audits.** Accessibility is DONE, by running the engine
  rather than porting its wrappers: `page.checkAccessibility()` covers
  all 67 at once and `just a11y-diff` checks it against Lighthouse. The
  rest of the shape of the job is now measured
  rather than guessed (`scripts/perf-diff/README.md`): 67 of them are
  axe-core wrappers, so running axe-core in the page covers all 67 at
  once; 26 need artifacts gathered from a live DOM and NOT one of them
  runs off a trace, which is where the earlier note that they were
  "pure functions over artifacts we already collect" was wrong; and the
  performance audits are wrappers around the insights already ported.
  `just lh-audit <url>` runs Lighthouse's own audits so anything built
  here can be checked against them.
- **Heap snapshot analysis** (13 upstream tools).
- **Chrome extension management, PWA tools, WebMCP, third-party devtools
  hooks.**
- **Source-mapped console stack traces.**
- **DuplicatedJavaScript** detects byte-identical bodies at several URLs,
  not duplicated modules inside different bundles; that needs source
  maps, which are not in the trace.
- **LegacyJavaScript** has no source-map pass, so a heavily minified
  bundle may under-report.
- **CLSCulprits** does not attribute non-composited animations.

Each is stated in the module doc where it applies. Keep that habit.

## Traps that cost real time

Written down because rediscovering them is expensive.

**Chrome's trace does not match what upstream's source implies.**
`Layout` hides `dirtyObjects` under `args.beginData`; `UpdateLayoutTree`
puts `elementCount` directly on `args`; neither uses the `args.data`
every other event does. `EventTiming` emits pointerdown, pointerup and
click under ONE `interactionId` and its timings are `performance.now()`
milliseconds while `ts` is trace microseconds. LCP candidates no longer
carry `imageUrl`, so the image request is recovered from
`imageLoadStart`/`imageLoadEnd`. `TracingStartedInBrowser` reports the
frame URL as `about:blank`. Script source arrives on `ScriptCatchup`
only under the `v8-source-rundown-sources` category.

**Trace order matters.** Forced reflow found nothing on a page that was
genuinely thrashing layout because I sorted events by timestamp; upstream
walks in trace order, and sorting interleaves threads and corrupts the
containment stack. Lantern's task collection has the same shape of
problem from the other side: it must be filtered to the renderer main
thread first, or a task on one thread truncates a task on another.

**A root with a dependency can never start.** If anything makes the
document request depend on a CPU node, the whole simulation silently
returns 0ms. Upstream's resource-type filter (XHR, Fetch, Script only) is
what prevents it.

**A thrown message does not survive page evaluation.** The host sees
`Uncaught` and nothing else. `normalize()` promised a typed strict error
and delivered `backend error: Uncaught` for exactly this reason. Injected
helpers should return `{ok} | {strict} | {none}` as data.

## Immediate next steps, in the order I would take them

1. ~~Wire the differential into `just`.~~ Done: `just perf-diff`,
   `just perf-diff-update`, and a `differential` test in the crate.
   Running it found four more divergences, all now fixed.
2. `site/docs/comparison/index.md` is stamped 2026-05-25 and is wrong.
   playwright-mcp now ships 69 tools including `browser_run_code_unsafe`,
   a `playwright-cli` with named sessions, and an agent skills bundle
   with test generation. chrome-devtools-mcp is at ~56 tools.
3. Trace parsing: `args` deserialisation is the remaining cost. Deferring
   it further means a borrowed `&RawValue` and a lifetime through the
   handlers. simd-json is not the lever; it measured 7%.

## One thing I would push back on

`ferridriver-perf` has no fixture trace in its own `tests/`. Its 62 tests
are hand-built event arrays, which are precise but prove only that the
code does what I thought. The real traces are what caught the actual
bugs. They now live in the crate's own `tests/fixtures/` and
`tests/differential.rs` reads them, so a regression against real Chrome
output fails the build.
