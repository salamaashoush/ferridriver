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

`just a11y-diff` compares ferridriver's own audit against Lighthouse's.
Both run axe-core, so where they overlap they must agree exactly — same
rules failing, same element counts. What they do not share is scope:
Lighthouse narrows axe to the rules its own audits wrap, while
`page.checkAccessibility()` runs the engine and reports every rule it
has. Extra rules on our side are the point, so the comparison is over
the intersection. In that direction only: a rule Lighthouse reached and
we never ran fails, because ours is meant to be the superset. It was
not. Lighthouse hands `axe.run` a `rules` map turning on five the
defaults drop — `target-size` and `identical-links-same-purpose` ship
`enabled: false`, and `label-content-name-mismatch`,
`table-fake-caption` and `td-has-header` are tagged `experimental` —
and until the fixture below existed, nothing noticed we were missing
them.

axe-core is pinned to the version Lighthouse bundles. A rule that only
one of them has would otherwise read as a disagreement about the page.

### The audits that are a port

Everything Lighthouse scores in snapshot mode is `doctype`,
`meta-description`, `crawlable-anchors`, `link-text`,
`image-aspect-ratio`, `image-size-responsive` and
`paste-preventing-inputs`, plus `canonical`, `http-status-code` and
`is-crawlable` after a navigation. All ten are ported, in
`crates/ferridriver/src/audits.rs`, and `just quality-diff` compares
them.

The last two read the main document's own RESPONSE rather than the
page, which nothing page-side can see: the status and the response
headers come out of the context's network log, by Lighthouse's own rule
for which request is the main one (the last document request whose URL
matches the document's, so a reload and a redirect chain both resolve
to the response on screen), and `is-crawlable` fetches `/robots.txt`
from the page as well. A document that arrived without a response of
its own — `about:blank`, `setContent` — is not scored on either, and
they are simply absent from the report, which is what Lighthouse's
`notApplicable` amounts to.

`lighthouse.mjs --navigation` drives a real load instead of reading the
page as it stands, which is the only way Lighthouse scores the audits
needing a network log, so every fixture carries a second recording.
Snapshot stays the recording of record and navigation only fills in what
snapshot cannot score at all: navigation applies its own emulation, so
merging its axe verdicts over the snapshot ones would silently change
what is being compared.

This comparison is the one that matters most, because it is the only
one with no shared engine underneath. The accessibility comparison has
axe-core doing the work on both sides, so a disagreement means we
called it wrong; here both sides are separate implementations of the
same arithmetic, and nothing agrees by construction. Verdicts and
element counts are both compared: an audit can reach the right answer
from the wrong set of elements, and a count is the cheapest way to
notice.

`is-crawlable` does not parse robots.txt itself, and neither does
Lighthouse: it imports `robots-parser`, so agreeing with Lighthouse
means agreeing with that package. `just robots-diff` runs the real
package over a table of (robots.txt, url, user-agent) triples and
`cargo test -p ferridriver-perf --test robots` replays the answers
offline. It is its own comparison rather than part of the fixture
pages because the parts most easily got wrong never show up in a page:
which of two equal-length rules wins, what an empty `Disallow:` does to
the `*` fallback, how a pattern is percent-encoded before it is
matched.

One thing it cannot see. `crawlable-anchors` asks whether an anchor
with no `href` has an event listener, and Lighthouse answers with CDP's
`DOMDebugger.getEventListeners`. Nothing page-side enumerates
listeners and no BiDi command exposes them, so ours answers the visible
half: an `onclick` attribute, which Chrome also reports as a listener.
The two part company on an anchor whose only handler came from
`addEventListener`. Stated in the module doc rather than papered over.

One more, on a different backend. Gecko joins repeated response headers
before WebDriver BiDi sees them, so an `X-Robots-Tag` sent twice reaches
Firefox as one header holding both values — Playwright's own BiDi
backend is in the same position. It changes how many items
`is-crawlable` reports, not what it concludes, because a comma already
separates directives inside one value. WebKit reports headers as a map
too, but its values are recoverable, and are split the way Playwright's
WebKit backend splits them.

### The fixtures, and the gate

The input cannot be a recording, because an audit has to look at a live
DOM. So the PAGE is what is checked in, in
`crates/ferridriver-perf/tests/fixtures/lighthouse/`, and Lighthouse's
whole verdict about it is the recording beside it. That puts the gate
back offline: `cargo test -p ferridriver-perf --test lighthouse` serves
the page itself
and needs neither node nor Lighthouse, so both comparisons run in
`just test`. One recording covers both. `just lh-record` re-records
after a deliberate change; `just a11y-diff` and `just quality-diff`
with no argument re-derive the verdicts and fail if they have drifted.

Eight pages, for the reason the trace fixtures grew from two to four.
`a11y-broken.html` fails 22 axe rules and one audit;
`quality-broken.html` fails seven audits and no axe rule; `clean.html`
fails nothing; `canonical-broken.html`, `status-404.html`,
`meta-robots-blocked.html`, `x-robots-blocked.html` and
`robots-blocked.html` fail exactly one apiece. `is-crawlable` gets three
of its own because it has three independent blocking sources, and a page
tripping all three would agree with Lighthouse even if two of the three
were never read. Each page is broken in one dimension and sound in the
others,
so neither comparison can agree because both sides found nothing —
which is what the first two pages were doing. Before `a11y-broken.html`
existed the accessibility comparison ran over seven rules on pages
where all seven passed, and the five missing rules above sat behind
that. Both tests assert floors on how much each page compares and how
much it trips, so a fixture cannot be quietly defused.

`just a11y-diff <url>` and `just quality-diff <url>` still take a live
page, for anything not in the fixtures. Nothing is recorded and nothing
gates.

Behaviour, as opposed to agreement, is `tests/e2e/accessibility.test.ts`
and `tests/e2e/page-quality.test.ts` on all four backends, and
`crates/ferridriver-node/test/accessibility-audit.test.ts` and
`page-quality.test.ts` through NAPI.

### What is actually in Lighthouse, measured rather than assumed

Of the ~93 non-performance audits the bundle implements:

- **67 are axe-core wrappers.** Every one requires the `Accessibility`
  artifact and does nothing but look up a rule id in what axe-core
  already decided. Porting them means porting nothing; running axe-core
  in the page and formatting its output covers all 67, once axe is
  asked for the five rules it hides by default. **Done.**
- **26 need artifacts gathered from a live page.** Not one of them runs
  off a trace. In snapshot mode seven get a score on a normal page:
  `doctype`, `meta-description`, `crawlable-anchors`, `link-text`,
  `image-aspect-ratio`, `image-size-responsive`,
  `paste-preventing-inputs`. **Those seven are done**
  (`crates/ferridriver/src/audits.rs`), and so are `canonical`,
  `http-status-code` and `is-crawlable`, which need a navigation. The
  remaining three are measured rather than guessed, and none of them is
  comparable:

  - `has-hsts` and `csp-xss` score `informative` in Lighthouse, not
    `binary`. There is no pass/fail verdict to agree with.
  - `is-on-https` treats localhost as secure
    (`URL.isLikeLocalhost`), so every request a hermetic fixture can
    make already passes it. Porting it would add a gate that agrees
    by finding nothing, which is the trap the fixtures above exist to
    avoid.
- **The performance audits are wrappers around the DevTools insights**,
  which `ferridriver-perf` has already ported and checks against the
  engine on every `just test`. There is nothing left to port there.
