# Checking ferridriver against the engines it is a port of

`ferridriver-perf` is a port of devtools-frontend's trace analysis. A
port's own tests can only show that it does what its author thought, and
that is not the same as agreeing with the thing it was ported from: the
crate looked finished when it had passed only its own fixtures, and
running it against the real engine on the same traces found four
defects, one of which told people to delete code they need.

So the engine gets to mark our homework.

```sh
just perf-diff          # re-derive the engine's verdicts and compare
just perf-diff-update    # re-record them after a deliberate change
```

## How it fits together

`engine.mjs` runs the real devtools-frontend engine over a trace and
prints its verdicts as JSON. Those recordings live beside the traces in
`crates/ferridriver-perf/tests/fixtures/` and are checked in, so
`cargo test -p ferridriver-perf --test differential` compares our
analysis against them offline, on every `just test`. `just perf-diff` is
the other half: it re-derives the recordings from the engine and fails
if they have drifted, which is what keeps the offline gate honest.

The engine is not vendored. `package.json` pins
`chrome-devtools-mcp@1.8.0`, which ships the prebuilt devtools-frontend
bundle; `just perf-diff` installs it on first use.

The comparison itself is Rust, in `tests/differential.rs`. `engine.mjs`
extracts mechanically — every state, checklist, number and array length
it can see — and knows nothing about what any insight means, so it does
not change when the Rust side starts comparing another quantity. What is
compared, and the unit each side uses, is a table at the top of that
test.

## The fixtures

- `plain-load` — a straight page load.
- `interaction-and-thrash` — the same page with a click and a layout
  thrash, which is where the forced-reflow and interaction paths get
  exercised.
- `redirect-and-shift` — the one with data in it. A redirect, a text
  LCP, images served an order of magnitude larger than they are drawn,
  a layout shift after load and two byte-identical bundles.
- `fonts-and-selectors` — a blocking web font and CSS selector
  statistics, which need the `disabled-by-default-blink.debug` category
  that no ordinary capture records. This page also never reaches a
  largest contentful paint, which is the branch where upstream's own
  Lantern context fails to build and half the insights stop predicting
  anything.

The last two fixtures exist because the first two agree with upstream
largely by both finding nothing: on that page most insights report zero,
so a comparison passes without exercising the code. Adding pages that
actually trip them found ten more defects, among them an insight that
vanished from the report entirely when the LCP was text, a font handler
that had never once seen a font, and a request priority read before
Chrome had finished changing it. A fixture that makes everything pass is
not evidence.

`fixture-server.py` serves both pages, on 8732 with a second origin on
8733: `/` is the plain one and `/rich` the loud one. Capture a new trace
with `cargo run -p ferridriver-perf --example capture -- <url>
<out.json>`, then gzip it into the fixtures directory and record it with
`just perf-diff-update`.

Capture the selector statistics with
`+disabled-by-default-blink.debug,disabled-by-default-devtools.timeline.invalidationTracking`
as the capture example's fourth argument.

The font body in the fixture is not a decodable font and does not need
to be: Chrome emits `BeginRemoteFontLoad` when it starts fetching one,
and the insight is arithmetic over that request's timings.

## Two things that will waste your time

`engine.mjs` creates the `DevToolsLocale` instance BEFORE registering
locale data. Without that every insight fails with "No LanguageSelector
instance exists yet", which reads like a data problem and is not one.

`npm install` here needs the public registry. If your npm is pointed at
a private mirror the install fails with a 403 on `chrome-devtools-mcp`;
the recipe passes `--registry` explicitly for that reason.

## What is compared

Metrics, every insight's state, every checklist, and the numbers under
them. Beyond that the comparison reaches the simulator itself: the round
trip and throughput the analyser derived, and the paint estimates the
graphs produce. Those last two matter because every predicted saving is
a DIFFERENCE of two simulations, so a round trip that is wrong in the
same direction on both sides cancels out and the insight comparison
stays green while the model underneath is out. It was out.

## The one disagreement that is meant to be there

Upstream reports `SlowCSSSelector` as a pass when the trace carries no
selector statistics, claiming a result it has no data for. Ours reports
that it was not measured. `STATE_DIVERGENCES` in `tests/differential.rs`
records it. Do not "fix" this to reach nineteen out of nineteen.

## Lighthouse

`just lh-audit <url>` runs Lighthouse's own audits against a live page
and prints what they concluded, down to the selectors they objected to.
It is the counterpart to `just perf-diff` for anything that is not
derived from a trace.

It has to drive a real page. Lighthouse audits read artifacts gathered
from the DOM — `MetaElements`, `AnchorElements`, `ImageElements`,
`Doctype`, `Accessibility` — and none of them come out of a trace file,
so the recorded-verdict trick that makes `perf-diff` offline does not
transfer. Snapshot mode keeps it deterministic: it reads the page as it
stands and navigates nothing, so a static fixture answers the same way
every run.

Serve the fixtures first, then point it at a page:

```sh
python3 scripts/perf-diff/fixture-server.py &
just lh-audit http://127.0.0.1:8732/rich/
```

### Accessibility

`just a11y-diff <url>` compares ferridriver's own audit against
Lighthouse's on the same page. Both run axe-core, so where they overlap
they must agree exactly — same rules failing, same element counts. What
they do not share is scope: Lighthouse wraps 67 of axe's rules and
reports only those, while `page.checkAccessibility()` runs the engine
and reports every rule it has. Extra rules on our side are the point,
so the comparison is over the intersection.

axe-core is pinned to the version Lighthouse bundles. A rule that only
one of them has would otherwise read as a disagreement about the page.

### What is actually in Lighthouse, measured rather than assumed

Of the ~93 non-performance audits the bundle implements:

- **67 are axe-core wrappers.** Every one requires the `Accessibility`
  artifact and does nothing but look up a rule id in what axe-core
  already decided. Porting them means porting nothing; running axe-core
  in the page and formatting its output covers all 67.
- **26 need artifacts gathered from a live page.** Not one of them runs
  off a trace. In snapshot mode seven get a score on a normal page:
  `doctype`, `meta-description`, `crawlable-anchors`, `link-text`,
  `image-aspect-ratio`, `image-size-responsive`,
  `paste-preventing-inputs`. The rest — `is-on-https`, `csp-xss`,
  `has-hsts`, `canonical`, `is-crawlable`, `http-status-code` and
  friends — need a network log, which means navigation mode.
- **The performance audits are wrappers around the DevTools insights**,
  which `ferridriver-perf` has already ported and checks against the
  engine on every `just test`. There is nothing left to port there.
