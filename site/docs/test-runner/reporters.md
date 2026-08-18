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

## Custom reporters

A reporter name that is not one of the built-ins is a path to a module,
resolved against the working directory and then against `testDir`. The
module's default export is the reporter class; it is bundled and
compiled through the same TypeScript pipeline the specs take, and
constructed once per run with the entry's own options plus `configDir`
and `outputDir`:

```toml
[[test.reporter]]
name = "./reporters/failure-reporter.ts"
outputFile = "reports/failures.json"
```

```ts
import type { Reporter, ReporterFullConfig, ReporterSuite, ReporterTestCase } from '@ferridriver/test';

export default class FailureReporter implements Reporter {
  private readonly failed: string[] = [];

  constructor(private readonly options: { outputFile?: string } = {}) {}

  onBegin(config: ReporterFullConfig, suite: ReporterSuite): void {
    console.log(`${suite.allTests().length} tests on ${config.workers} workers`);
  }

  onTestEnd(test: ReporterTestCase): void {
    if (test.outcome() === 'unexpected') this.failed.push(test.titlePath().join(' > '));
  }

  async onEnd(): Promise<void> {
    await fs.writeFile(this.options.outputFile ?? 'failures.json', JSON.stringify(this.failed));
  }
}
```

A reporter runs in the same sandbox every other script does: `fs` is
rooted at the project directory and `fetch` obeys the `[scripting]`
network policy, so a reporter that posts to a webhook needs that host
allowed like any other outbound call.

The module is compiled before the first test runs, so a reporter that
does not resolve, does not parse, or throws while constructing fails
the command rather than the run. Once the run has started, a reporter is
isolated: a hook that throws is reported and the run carries on, exactly
as it does upstream.

### The two interfaces

There are two, and which one a class implements is decided by
`version()`:

| | `Reporter` | `ReporterV2` |
|---|---|---|
| `version()` | absent | returns `'v2'` |
| `onConfigure(config)` | never called | called first |
| `onBegin(...)` | `(config, suite)` | `(suite)` |

Everything else is the same. `Reporter` is the interface third-party
Playwright reporters implement, so an existing one works unchanged.

### What a hook is handed

The objects are Playwright's: a `Suite` tree of root → project → file →
describe with `titlePath()`, `entries()`, `allTests()` and `project()`;
a `TestCase` with `outcome()` and `ok()` computed from the results it
has so far; a `TestResult` whose `status` is `undefined` until the
attempt ends, and whose `steps` fill in as `onStepBegin` fires; and
`attachments` whose `body` is a `Buffer`.

### Deciding whether the run printed anything

`printsToStdio()` says whether the reporter writes to the terminal. If
nothing in the whole set does — a run configured with only `json` and a
custom file reporter, say — a `line` reporter (or `dot` under CI) is put
in FRONT of the set, so the run is never silent. A reporter that does
not declare `printsToStdio` counts as one that prints.

### Editing the run, and deciding how it ended

`preprocess({ config, suite, testRun })` runs once before the first
test with the whole corpus in hand. `testRun.exclude(target)` drops a
case or a whole suite, `testRun.skip` / `fixme` / `fail` annotate one,
and `testRun.skipSharding()` says the reporter has taken sharding over.
Unlike every other hook, an error here is not swallowed — it aborts the
run rather than letting a half-applied edit reach the workers. The
handle is dead the moment `preprocess` returns.

`onEnd(result)` may return `{ status }` to change how the run is
reported to have ended, and the process exit code follows it. The last
reporter to answer wins.
