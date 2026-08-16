# UI mode and traces: handover

Where the Playwright-compatible UI and trace work stands, what is left,
and the traps that cost time getting here. Everything below is
uncommitted work in the current tree, alongside unrelated in-flight work
(config/extension overhaul, session surface, `test --debug`).

## Version pin — read this first

The comparison target is **playwright-core 1.62.1**, the newest RELEASE.
`/tmp/playwright` (main) is `1.63.0-next` and describes things that have
not shipped: the split `screencast/ screenshots/ aria/` trace layout,
`file` instead of `sha1`, `aria-snapshot` and `screenshot` trace events.
Building against main would produce traces the shipping viewer cannot
read.

Read the release you ship, not main:

```bash
git -C /tmp/playwright fetch --depth 1 origin refs/tags/v1.62.1:refs/tags/v1.62.1
git -C /tmp/playwright archive v1.62.1 packages | tar x -C /some/scratch
```

## What ships in the binary

`scripts/vendor-playwright-assets.sh [version]` pulls one pinned
`playwright-core` from npm and commits three static apps as zips under
`crates/ferridriver-viewer/assets/` (version recorded in
`PLAYWRIGHT_VERSION`). npm is needed only to re-vendor; building and
running ferridriver never shells out.

| zip | app | used by |
|---|---|---|
| `traceviewer.zip` | trace viewer (`index.html`) **and** UI mode (`uiMode.html`) | `ferridriver trace view`, `--ui` |
| `recorder.zip` | recorder / inspector | vendored, not wired (gap 2) |

Playwright's HTML-report app is deliberately NOT vendored: the run's own
reporter (`reporter/html.rs`) writes one self-contained file, which is
the report ferridriver ships.

## Crate layout

`ferridriver-viewer` owns everything about looking at a trace:

- `apps.rs` — the embedded archives, unpacked on first use, served with
  the content types and cache policy the service-worker registration
  needs (a worker that does not arrive as JavaScript is refused).
- `files.rs` — `GET /trace/file?path=…`, the route the viewer reads
  traces through. An existing file is streamed (Range included); a
  MISSING `<prefix>.json` is answered with a synthesized descriptor of
  the loose files a still-running recording has produced. That
  descriptor IS the live-trace mechanism — no zip is built while a test
  runs.
- `model.rs` / `dump.rs` — the trace read back into Rust and rendered as
  text. Backs `ferridriver trace show|ls` and makes traces assertable in
  tests without a browser.

## Trace format (v8, 1.62.1 shapes)

Landed:

- recordings are **named loose files** under a `tracesDir`
  (`<name>.trace`, `<name>.network`, `resources/`), zipped into
  canonical `trace.trace` / `trace.network` entries on `stop`;
- `browserType.launch({ tracesDir })` is now real (it was accepted and
  ignored), plus a per-context override — parallel workers share one
  browser but each write into their own artifacts directory
  (`BrowserState::traces_dir_for`, `ContextRef::set_traces_dir`);
- `live: true` flushes every event as written, so a viewer can follow a
  recording in progress (`TraceStreaming` in `crates/ferridriver/src/trace.rs`);
- `stepId` on every action, `tracing.group()` / `groupEnd()`, run-level
  `error` events (Errors tab), `platform: darwin|linux|win32` and a
  recorder version in `context-options`.

Bindings (core, NAPI, QuickJS) carry `live`, `startChunk({name, title})`
and `group`/`groupEnd`. One deliberate divergence: Playwright's
`tracing.group()` returns a promise for a disposable (`using` closes the
group); ours is synchronous and returns nothing, so a group is closed by
`groupEnd()`. `await tracing.group(...)` still works.

The runner writes each test's trace to
`<outputDir>/.playwright-artifacts-<worker>/traces/<testId>.trace`. Both
halves of that path are dictated by the embedded UI — it computes a
running test's trace location itself from `artifactsFolderName(workerIndex)`
and the test id — so the directory name and the Playwright-shaped id
(`sha1(file)[..20]-sha1("[project=…]"+titlePath)[..20]`,
`TestId::stable_id`) are not free choices.

## UI mode

`ferridriver test --ui` and `ferridriver bdd --ui` serve the embedded
uiMode app and answer its protocol
(`crates/ferridriver-test/src/test_server/`):

- `mod.rs` — transport. `/` redirects to `/trace/uiMode.html?ws=<guid>`,
  the websocket lives at `/<guid>`, requests reach the run loop one at a
  time, events fan out per-client (one unbounded queue each; the client
  is a state machine, so dropping an `onTestEnd` corrupts rather than
  degrades its view).
- `tele.rs` — our runner model in Playwright's reporter events.
- `driver.rs` — the loop: listing, filtered runs, cancellation mid-run,
  watch → `testFilesChanged`, editor open, browser install.

`TestRunner::run_test_server` owns the session: one
shared browser for every run, traces forced on and live, and the app
window whose closing ends the session. `--ui-port` (or a host) serves
instead of opening a window — that is also how the tests drive it.

`ferridriver rust-test --ui` still serves the OLD app (`ui_server.rs` +
`ui_assets/index.html`) because that path aggregates several cargo-built
harness binaries over a socket.

## What a run carries (options, projects, errors)

A call's options reach the run through a runner of its own:
`Driver::runner_for` clones the session config, forces the single
attempt a UI run is (`retries = 0`, `repeatEach = 1`, as
`testRunner.ts::_innerRunTests` does), applies the request, and hands
back `TestRunner::with_run_options`. Honoured: `projects`, `headed`,
`workers` (count or `"50%"`), `maxFailures`, `timeout`,
`updateSnapshots`, `trace`, `video`, `reporters`, plus the
`testIds`/`locations`/`grep`/`grepInvert` narrowing. Refused with a
reason — never ignored — `reuseContext`, `connectWsEndpoint`,
`updateSourceMethod` other than `overwrite`, `onlyChanged`, and any
unknown mode; a refusal fails the call AND reports `onError`.

Every project of the config is listed and run:
`TestRunner::project_runs` merges `[[test.projects]]` onto the config,
`tele::configure` carries them all, and `onProject` arrives once per
project with each test's id hashed against its own project name.
`execute_projects_with_summary` is the shared scheduler (dependency
order, teardowns, `maxParallelProjects`) that both `run` and the UI use;
the UI passes `ProjectHooks` so each project gets its own event stream —
two projects run the same file, and only the project name tells their
results apart. A project's events are pumped back onto the run's shared
bus so configured reporters still see one run.

Discovery failures reach the UI: a plan factory returns `PlanBuild`
(plan + errors), so a bundling error becomes `onError` and a failed
listing instead of an empty tree.

## What is NOT done

Ordered by what I would do next.

1. **Trace action coverage — the biggest functional gap.** Only
   `goto`/`goBack`/`goForward`/`reload`, every `Locator` method (the
   `retry_resolve!` macro) and `expect` open spans. `page.setContent`,
   `evaluate`, `title`, `screenshot`, the waits, keyboard/mouse, and
   every `BrowserContext` call (`addCookies`, `route`, …) produce no
   action, so our action lists are visibly thinner than Playwright's.
   The recipe is the triple `goto` already uses:

   ```rust
   let span = self.trace_span("setContent", json!({}));            // page.rs:353
   let span = crate::trace::open_action(self.snapshot_before(span).await).await;  // page.rs:371
   let result = /* the call */;
   if let Some(span) = span {
     self.snapshot_after_and_finish(span, result.as_ref().err()).await;           // page.rs:479
   }
   ```

   Note `page.click(selector)` and friends already appear, because they
   delegate to `Locator`. Do the whole public surface in one pass rather
   than a subset.
2. **Interactive debugging.** `pauseOnError` / `pauseAtEnd` are accepted
   and REFUSED with a message unless the debugger is armed; nothing
   pauses. Real pause/resume/step means
   driving the vendored `recorder` app: open it in a window, expose
   `window.sendCommand`, push `pauseStateChanged` / `callLogsUpdated` /
   `sourcesUpdated`, and map `resume` / `step` onto the existing
   `ActionGate`. Contract is `recorder/src/recorderTypes.d.ts` in the
   1.62.1 source. Playwright's own UI-mode app has no pause controls
   either — upstream that interaction lives in the Inspector.
3. **stdio is buffered per test.** `interceptStdio` is accepted and
   ignored; a test's output arrives in one lump at its end
   (`tele::stdio`, `driver.rs` forwarder) instead of streaming into the
   UI's terminal pane.
4. **`initialize` options are still accepted and ignored** —
   `closeOnDisconnect` (a VS Code session that sets it leaves the server
   running after its client goes away), `watchTestDirs` (we always
   watch), `populateDependenciesOnList`, `serializer`. Run options are
   honoured or refused now; these are the remaining silent ones.
5. **A served report.** The HTML reporter writes a file; there is no
   `show-report` command that serves it (and no history across runs).
6. **1.63 trace additions** — `aria-snapshot` / `screenshot` events and
   the split directory layout. Blocked on the release; do it when the
   vendored bundle moves.
7. **Retire the classic UI** once `rust-test --ui` speaks the test
   server too.

## Traps already paid for

- The embedded bundle dictates paths: `.playwright-artifacts-<workerIndex>`
  and `<testId>.json` are hardcoded in the app. Change either and the
  live view is silently empty.
- The UI sends `locations` as escaped regexes over ABSOLUTE paths with
  the leading slash stripped; plan files may be relative. Comparing them
  naively makes "Run all" run nothing (found only by driving the real
  app — the wire-level tests passed).
- Concurrent projects share one output directory. Sweeping
  `.playwright-artifacts-*` from a per-project run deletes another
  project's in-flight trace; sweep only in the outermost runner
  (`execute_projects_with_summary`).
- A project's merged config must carry the project's NAME
  (`merge_project`): a test's identity is hashed with it and that
  identity names the trace file, so leaving the base name there made two
  projects write over each other's trace.
- A session's shared browser is only reusable by a project whose launch
  plan matches it (`same_launch`). Without that check a WebKit project
  ran on the session's Chromium and reported passes for an engine it
  never touched.
- `testDir` is anchored to the config file it was written in, so it is
  absolute, while a plan's files are relative to the run's cwd. The
  project filter compared them as written and kept nothing — any config
  with `testDir` plus `[[test.projects]]` discovered tests and then ran
  none.
- A per-run `timeout` could not work while `translate.rs` baked
  `config.timeout` into every `TestCase`: the runner's
  `test.timeout.unwrap_or(config.timeout)` never reached the run's
  config. A test now carries only the timeout its own source asked for.
- `maxFailures` stops dispatch, but a worker has usually already picked
  up the next test — assert that a run stopped early, not that exactly
  one test ended.
- `cargo fmt` / edits during a `cargo test -p ferridriver-cli` run break
  it — `test_ui_mode` spawns `cargo test` children that recompile.
- The BDD suite occasionally hangs in teardown after all scenarios pass.
  Pre-existing roaming flake; rerun before blaming a diff.

## How to verify

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo test --workspace --exclude ferridriver-cli --exclude ferridriver-node
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver cargo test -p ferridriver-cli -- --test-threads=1
./target/debug/ferridriver test --project cdp-pipe        # 342 passed / 5 skipped
./target/debug/ferridriver bdd tests/features/            # 574 passed / 22 skipped
cd crates/ferridriver-node && bun run build:debug && bun test   # 1077 pass
cd tests && bun x tsc --noEmit -p tsconfig.json
```

Driving the real UI (this is what caught the location bug — the wire
tests did not):

```bash
ferridriver test --ui --ui-port 9377        # serves, no window
# then, in a scratch script run with `ferridriver run`:
#   goto http://127.0.0.1:9377/trace/uiMode.html?ws=<guid>&pathSeparator=%2F
#   click [title^="Run all"], read .ui-mode-sidebar innerText
```

## Tests

- `crates/ferridriver-viewer` — assets, file route, descriptor, trace
  model, text rendering.
- `crates/ferridriver/src/trace.rs` — named/live recordings, chunk
  rename, groups, `stepId`, error events.
- `crates/ferridriver-test/src/test_server/` — protocol shapes, event
  fan-out, request lifecycle, run options onto the config, refusals,
  per-project id resolution.
- `crates/ferridriver-cli/tests/trace_command.rs` — `trace show|ls|view`
  through the built binary.
- `crates/ferridriver-cli/tests/test_server_ui.rs` — the UI's side of
  the protocol for `test --ui`, incl. a live trace read off disk while a
  test runs, two projects listed and run under their own names, and each
  run option observed taking effect (`workers`, `maxFailures`, `timeout`,
  `trace`, `video`, `updateSnapshots`, `projects`). `headed` is the one
  option only unit-tested — asserting it needs a display.
- `crates/ferridriver-cli/tests/ui_mode.rs` — the same for `bdd --ui`,
  plus step nesting, DOM snapshots and embedded sources in the trace.
