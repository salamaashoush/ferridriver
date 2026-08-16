# Reporters

A run publishes events to every configured reporter at once. Pick them
on the command line or in the config file; they compose freely, so a CI
job can print a live `line` view, annotate the PR, and write JUnit XML
from one run.

```bash
ferridriver test --reporter line --reporter junit --reporter html
```

```toml
[[test.reporter]]
name = "list"

[[test.reporter]]
name = "junit"
outputFile = "reports/junit.xml"
includeProjectInTestName = true
```

Options may sit beside `name`, as above, or in an explicit table — both
are read, and the table wins on a collision:

```toml
[[test.reporter]]
name = "junit"
[test.reporter.options]
outputFile = "reports/junit.xml"
```

A reporter with no options can be named on its own:

```toml
reporter = ["list", "html"]
```

## Built-in reporters

| Name | Output | Notes |
|---|---|---|
| `list` (`terminal`, `default`) | terminal | One line per test, then the numbered failure bodies and summary. The default. |
| `line` | terminal | One self-rewriting status line; failures print above it as they happen. Best for large suites. |
| `dot` | terminal | One character per test, then the same failure epilogue. |
| `progress` | terminal | `dot` plus a running `done/total` counter. |
| `json` | `results.json` | Playwright's `JSONReport` shape. |
| `junit` | `junit.xml` | JUnit XML with Xray-style `<properties>`. |
| `html` | `report.html` | Self-contained report: every attempt, artifacts, steps. |
| `blob` | `report.zip` | The event stream itself — input to `merge-reports`. |
| `github` | annotations | GitHub Actions `::error` / `::warning` / `::notice`, wrapping `list`. |
| `markdown` | `report.md` | Summary table plus per-failure detail; also appended to `$GITHUB_STEP_SUMMARY`. |
| `tap` / `tap-flat` | terminal | TAP version 13, nested per file or one flat plan. |
| `teamcity` | terminal | JetBrains service messages, streamed live. |
| `ctrf` | `ctrf-report.json` | [Common Test Report Format](https://ctrf.io). |
| `allure` | `allure-results/` | Allure 2.x results plus attachments. |
| `rerun` | `@rerun.txt` | Failed test locations for `--last-failed`. Always on. |
| `cucumber-json` | `cucumber.json` | Cucumber JSON, for BDD runs. |
| `messages` | `cucumber-messages.ndjson` | Cucumber Messages NDJSON. |
| `usage` | terminal | Step-definition call counts and timings (BDD). |
| `null` (`empty`) | nothing | Silences reporting entirely. |

Every terminal reporter ends a run the same way: the numbered failure
bodies, the slow-file warning, then the counts. A `dot` run that fails
prints the same diagnostics a `list` run does.

## Where files go

A file reporter resolves its path in this order, first match wins:

1. `FERRIDRIVER_<REPORTER>_OUTPUT_FILE` — e.g. `FERRIDRIVER_JUNIT_OUTPUT_FILE=/tmp/j.xml`
2. the reporter's `outputFile` option
3. `FERRIDRIVER_<REPORTER>_OUTPUT_DIR` or the `outputDir` option, joined with
   `FERRIDRIVER_<REPORTER>_OUTPUT_NAME` or the `fileName` option
4. the run's `outputDir` plus the reporter's default name

The environment variables are what a CI matrix uses to give each shard
its own file without editing the config.

## Reporter options

Options go beside the reporter's `name` (or in its `options` table), and
boolean ones can also be flipped from the environment as
`FERRIDRIVER_<REPORTER>_<OPTION>` (e.g.
`FERRIDRIVER_JUNIT_INCLUDE_RETRIES=1`).

**`junit`**

| Option | Effect |
|---|---|
| `includeProjectInTestName` | Prefix each `<testcase name>` with `[project]`. |
| `includeRetries` | Report each retry as `<flakyFailure>` / `<rerunFailure>` instead of collapsing to the final attempt. |
| `stripANSIControlSequences` | Drop terminal escapes from attributes and text. |
| `omitTags` | Leave tag annotations out of `<properties>`. |
| `suiteId`, `suiteName` | Fill the `<testsuites>` `id` / `name` attributes. |

**`html`**

| Option | Effect |
|---|---|
| `open` | `never` (default), `always`, or `on-failure` — whether to open the finished report in a browser. |

**`blob`**

| Option | Effect |
|---|---|
| `path` | Where the zip is written. |
| `shardIndex`, `shardTotal` | Recorded in the blob header so a merge keeps the run boundary. |

**`allure`**

| Option | Effect |
|---|---|
| `outputDir` | Results directory (default `allure-results`). |
| `suiteTitle` | Overrides the suite label. |

## Sharded runs and `merge-reports`

Each shard writes a blob; one merge step turns them into the report the
unsharded run would have produced.

```bash
# on each CI machine
FERRIDRIVER_BLOB_OUTPUT_FILE=blobs/report-$SHARD.zip \
  ferridriver test --reporter blob --shard $SHARD/$TOTAL

# once, after collecting blobs/
ferridriver merge-reports blobs --reporter html --reporter junit
```

The blob is lossless: steps, attachments, annotations, stacks, worker
indexes and start times all round-trip, so a merged HTML or JUnit report
carries everything a direct run's would. Inline artifacts (a failure
screenshot) are stored as entries inside the zip rather than inlined
into the event stream.

`merge-reports` exits non-zero when any test in the merged run failed,
so it gates a pipeline the same way the shards do.

## Writing to CI

`github` emits an annotation per failure pointing at the line the error
actually came from — parsed out of the stack, not the test's declaration
— plus a run-summary notice and a warning per slow file. It wraps the
`list` reporter, so the job log stays readable:

```bash
ferridriver test --reporter github
```

`markdown` writes `report.md` and, when `GITHUB_STEP_SUMMARY` is set,
appends the same text there — the summary renders on the job page with
no extra workflow step.
