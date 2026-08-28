# Differential harness artifacts

Rescued from a session scratchpad. See `../HANDOVER.md`.

These exist because `ferridriver-perf` looked finished when it was
verified only against its own tests and fixtures. Running it against the
real devtools-frontend engine on the same trace files found four defects,
including a false positive that told people to delete code they need.

- `final.json.gz`, `t2.json.gz` — the two traces the comparison runs on.
  `t2` carries an interaction and a layout thrash; `final` is a plain
  load. Gunzip before use.
- `perfserver.py` — the fixture the traces were captured from
  (render-blocking CSS, a lazily-loaded image LCP, a transpiled bundle, a
  third-party origin). Serves on 8732 and 8733.
- `diff.mjs` — drives the real devtools-frontend engine over a saved
  trace and prints metrics and insight verdicts in our shape.

## Running it

```sh
mkdir -p /tmp/cdt && cd /tmp/cdt
npm install chrome-devtools-mcp@1.8.0
cp <this dir>/diff.mjs .
gunzip -c <this dir>/final.json.gz > trace.json
node diff.mjs trace.json                                   # theirs
cargo run -p ferridriver-perf --example report -- trace.json  # ours
```

`diff.mjs` creates the `DevToolsLocale` instance before registering
locale data. Without that every insight fails with "No LanguageSelector
instance exists yet", which reads like a data problem and is not one.

## Expected agreement

Metrics exact to three decimals; insights 18/19.

`SlowCSSSelector` differs on purpose: upstream reports "pass" when the
trace carries no selector statistics, claiming a result it has no data
for. Ours reports "not measured". Do not "fix" this to reach 19.
