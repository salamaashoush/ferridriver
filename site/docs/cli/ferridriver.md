# `ferridriver`

A single static binary. JavaScript / TypeScript BDD step files run
natively through it — no separate TypeScript CLI exists — and so do the
web front-ends: the trace viewer, UI mode and the HTML report are
compiled in, so they work offline with no npm install.

## Synopsis

```
ferridriver [GLOBAL FLAGS] <SUBCOMMAND> [ARGS]
```

| Subcommand           | Purpose |
|----------------------|---------|
| `ferridriver mcp`    | Run the MCP server (stdio or HTTP) for AI agents |
| `ferridriver bdd`    | Run Gherkin features (Rust and / or JS / TS step bodies) |
| `ferridriver test`   | Wrap `cargo nextest` / `cargo test` for Rust unit and integration tests |
| `ferridriver run`    | Execute a JavaScript / TypeScript script with Playwright-style bindings |
| `ferridriver install`| Download browser binaries into the local cache |
| `ferridriver codegen`| Record interactions in a headed browser or a live session; emit a script or a spec |
| `ferridriver trace`  | Read a recorded trace: open it in the viewer, print it, list them |
| `ferridriver merge-reports` | Fold several shards' `blob` reports into one report |

## Codegen

`ferridriver codegen <url> [--output rec.ts] [--language ts|rust|gherkin]`
opens a headed browser, records your clicks / fills / selects / navigation
(generating Playwright-style locators), and writes code to `--output` or
stdout. Stop with Ctrl+C or by closing the browser.

The default `ts` output is a **runnable** script, not a test stub: it reuses
an injected `page` when one exists — so the MCP `run_script` tool replays it
on a live session — and otherwise launches its own browser, so the same file
runs standalone via `ferridriver run rec.ts`. `rust` emits a `#[ferritest]`
test; `gherkin` emits a `.feature`.

`--target test` writes an `@ferridriver/test` spec instead, for
`ferridriver test`:

```bash
ferridriver codegen https://example.com --target test -o login.spec.ts
```

`--attach <session>` records on a live session rather than a browser of its
own, headless or headed, whatever drives its page:

```bash
ferridriver session open dev https://example.com --headed
ferridriver codegen --attach dev --target test -o login.spec.ts
```

While it records, type `visible`, `text` or `value` and Enter, then click an
element, to record an assertion about it (the click does not reach the
page); `url` and `title` assert the page itself, `pause` and `record` stop
and resume recording, and `stop` or Ctrl+C ends it. The same recorder is
available to remote clients over the session wire; see [Named
sessions](/scripting/named-sessions#watching-driving-and-recording-remotely).

## Global flags

```
-v, --verbose...        Cumulative log level (-v debug, -vv trace incl. CDP)
-c, --config PATH       Config file (TOML/YAML/JSON; inferred from extension).
                        Auto-searches ferridriver.{toml,yaml,yml,json} in the
                        current directory, then config.{toml,yaml,yml,json}
                        in ~/.config/ferridriver/.
```

## `ferridriver mcp`

```
--backend BACKEND       cdp-pipe (default) | cdp-raw | webkit | bidi
--headless              Run the browser without a visible window
--executable-path PATH  Path to a Chrome / Chromium binary
--connect URL           WebSocket URL of an already-running browser
--auto-connect CHANNEL  Discover a running Chrome by channel name (mutually exclusive with --connect)
--user-data-dir DIR     Persistent Chrome profile directory (used by --auto-connect)
--transport TRANSPORT   stdio (default) | http
--port N                HTTP port (default 8080)
--host ADDR             HTTP listen address (default 127.0.0.1)
--token TOKEN           Bearer token a client must send; repeat for one
                        isolated set of browsers per token
                        (env FERRIDRIVER_MCP_TOKEN, comma-separated)
```

A non-loopback `--host` without a token is refused.

See [MCP overview](/mcp/overview) and [Client setup](/mcp/setup).

## `ferridriver bdd`

```
ferridriver bdd [--steps GLOB]... [--tags EXPR] [--workers N]
                [--reporter SPEC]... [--strict|--no-strict] [--dry-run] [--list]
                [--fail-fast]
                [--step-timeout MS] [--order defined|random[:SEED]]
                [--language LANG] [--world-parameters JSON]
                [BROWSER FLAGS]
                FEATURE_GLOB...
```

`--steps` loads JavaScript / TypeScript step-definition files
(repeatable; overrides `[test].steps` from the config file). Files are
bundled with rolldown, compiled to QuickJS bytecode once at startup, and
run on the embedded engine — no Node, no Bun.

Defaults to `steps/**/*.{js,ts}` and `step_definitions/**/*.{js,ts}` when
no `--steps` flag is provided.

Browser flags shared with `mcp`: `--backend`, `--headless`,
`--executable-path`, `--connect`, `--auto-connect`, `--user-data-dir`.
`--connect` and `--auto-connect` run the suite on that browser instead of
launching one, for `ferridriver test` too: every worker opens its own
connection and each test still gets a fresh context. The browser is left
running when the run ends.

## `ferridriver test`

```
ferridriver test [FILTER] [-p PACKAGE]... [--runner nextest|cargo]
                 [--profile PROFILE] [-- PASSTHROUGH...]
```

Auto-detects `cargo-nextest` and falls back to `cargo test`. Useful in
mixed projects where one command should drive any Rust test target.

## `ferridriver run`

```
ferridriver run [SCRIPT|-] [-e CODE] [--timeout-ms MS] [-- ARGS...]
```

Executes a script with Playwright-shaped globals (`chromium`, `firefox`,
`webkit`, `page`, `context`, `request`). A `.ts` / `.tsx` file, or any
source with top-level `import` / `export`, is rolldown-bundled and
transpiled before running. `args` is the positional list exposed as a
global. `-` reads source from stdin. `--eval` runs inline source instead
of a file.

## `ferridriver browser serve`

```
ferridriver browser serve [--host ADDR] [--port N] [--token TOKEN]
                          [--ws-path PATH] [--headed] [--executable-path PATH]
                          [--arg SWITCH]...
```

Launches Chromium and serves its DevTools Protocol to other processes. The
first line on stdout is the WebSocket URL to dial, for
`ferridriver test --connect`, `chromium().connectOverCDP()` or any CDP
client. Each client gets its own connection to the browser; nothing is
translated.

A client is admitted on the URL's path, an unguessable string unless
`--ws-path` names one, and, when the server has a token, on
`Authorization: Bearer <token>` or the `token` query parameter the printed
URL carries. Binding anything but loopback without `--token` (or
`FERRIDRIVER_BROWSER_TOKEN`) generates a token. Listening on all
addresses prints the URL with `127.0.0.1`; a remote client swaps in the
machine's own address. SIGTERM, SIGINT and SIGHUP
close the browser and exit; so does the browser exiting. The Rust API is
`BrowserType::launch_server`.

## `ferridriver trace`

```
ferridriver trace view [TRACE] [--port N] [--host ADDR] [--no-open]
ferridriver trace show [TRACE] [--errors] [--json] [--limit N]
                       [--hide logs|console|network]... [--color auto|always|never]
ferridriver trace ls   [DIR] [--json]
```

`TRACE` is a `trace.zip`, a directory of trace files (a recording that
was interrupted), or — for `view` — an `http(s)` URL. Omit it and the
newest trace under the test output directory is used.

`view` serves the embedded Playwright trace viewer and opens it in an
application window; `--no-open` prints the URL and keeps serving, which
is what you want over ssh or in a container.

`show` prints the same trace as text: the call tree with timings, what
failed and what it was waiting for, then console, network and attachment
summaries. `--errors` keeps only the failures; `--json` emits the whole
model for a script to read.

## UI mode

```
ferridriver test --ui [--ui-port N]
ferridriver bdd  --ui [--ui-port N] FEATURE_GLOB...
```

Serves Playwright's UI-mode app — the real one, embedded in this binary —
and answers the test-server protocol it speaks. The test tree, filters,
watch mode, per-test trace with DOM snapshots, source, console and
network all come from it. Traces are forced on for the session and are
written where the viewer can follow them while a test is still running.

Without `--ui-port` an application window is opened for you; with one,
the URL is printed and left for you to open (or for another tool to
drive).

## `ferridriver merge-reports`

```
ferridriver merge-reports [INPUTS...] [--reporter NAME[=TARGET]]... [--output-dir DIR]
```

Reads every `blob` zip and `stream` reporter `.jsonl` file under
`INPUTS` (a directory or the files themselves) and replays the merged
event stream through the named reporters, producing the report an
unsharded run would have produced. Attempts of one test from different
inputs fold into one test. Exits non-zero when any test in the merged
run failed.

```bash
# per shard: test-results/report-$SHARD.zip
ferridriver test --reporter blob --shard $SHARD/$TOTAL

# once, after collecting blobs/
ferridriver merge-reports blobs --reporter html --reporter junit=merged.xml
```

## Exit status of `test` and `bdd`

| Status | Meaning |
|---|---|
| 0 | Every test passed (flaky counts as passed unless `failOnFlakyTests`). |
| 1 | At least one test failed. |
| 2 | Usage or configuration error; nothing ran. |
| 3 | Interrupted by SIGINT or SIGTERM. Tests in flight finish and report; a second signal exits at once. |
| 4 | Infrastructure failure: a browser did not launch, a worker crashed, or a web server did not start. |

See [Reporters](/test-runner/reporters) for the full reporter list and
their options.

## `ferridriver install`

```
ferridriver install [BROWSERS]... [--with-deps] [--no-remove]
```

Browsers: `chromium`, `chromium-headless-shell`, `firefox`, `webkit`.
Defaults to `chromium`. `--with-deps` also installs Linux system
libraries via the platform package manager.

An install removes the cached versions of that browser older than the
one it just installed, and reports them. Newer versions and other
browsers stay. `--no-remove` keeps every version.

`ferridriver install webkit` downloads Playwright's WebKit build into the
ferridriver cache. Alternatively, point `FERRIDRIVER_WEBKIT` at a
Playwright WebKit checkout, or install Playwright once
(`npx playwright install webkit`) and ferridriver picks up its cache.

## Environment variables

| Variable                    | Purpose |
|-----------------------------|---------|
| `RUST_LOG`                  | Standard tracing filter — takes priority. Example: `RUST_LOG=warn,ferridriver::cdp=trace`. |
| `FERRIDRIVER_DEBUG`         | Category-based filter when `RUST_LOG` is unset. Values: `*` / `all`, `cdp`, `step` / `steps`, `hook` / `hooks`, `worker`, `fixture`, `reporter`, `action`, `runner`. |
| `FERRIDRIVER_PROFILE`       | `chrome` writes `trace-{pid}.json`; `console` enables the `tokio-console` dashboard. |
| `FERRIDRIVER_TRACE_FILE`    | Override the Chrome trace output path. |
| `FERRIDRIVER_BROWSERS_PATH` | Override the browser cache directory. |
| `FERRIDRIVER_WEBKIT`        | Override the Playwright WebKit checkout path. |
| `FERRIDRIVER_INSTALL_DIR`   | Where the install script drops the binary (`install.sh` only). |

## Install the binary

```bash
cargo install ferridriver-cli
# or prebuilt
curl -fsSL https://github.com/salamaashoush/ferridriver/releases/latest/download/ferridriver-VERSION-TARGET.tar.gz | tar xz
```

See [MCP > Setup](/mcp/setup) for client configuration snippets.
