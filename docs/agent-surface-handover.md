# Agent surface: handover

Where the session / scripting / codegen work stands, and what is left.

Context: this line of work started from a comparison against Playwright's
agent tooling at HEAD `07730b7a9` (2026-08-12) — `playwright-cli` (88 commands
over a daemon), `init-agents` / `init-skills`, `test --debug=cli`, and the
response contract their tools return. The goal is not to copy that surface but
to beat it where our architecture is genuinely stronger: one scripting engine
instead of a verb table.

## Where it landed

Committed as `feat(agent): script-carrying sessions, code echo, response
contract` (52 files). The files that carry the design:

```
crates/ferridriver/src/response.rs                       the shared builder
crates/ferridriver/src/codegen/echo.rs                   actions -> source
crates/ferridriver-script/src/session_host.rs            the ScriptHost
crates/ferridriver-cli/src/script_setup.rs               one scripting env
crates/ferridriver-cli/tests/backends_support/response_contract.rs
site/docs/scripting/named-sessions.md
docs/agent-surface-handover.md                           this file
```

The repository's history was rewritten separately (`git filter-repo`) to drop
vendor-specific names from every commit and to remove an accidentally
committed `.claude` memory file — every SHA changed, and `main` is the only
branch that survives.

## What shipped

### The session protocol carries a script, not verbs

`ferridriver-session` used to expose 14 hand-rolled verbs (`snapshot`, `goto`,
`click`, …) and a `ScriptHook` trait with **zero implementors** — every bind
site passed `None`, so `run-script` answered "scripting is not available". The
verb table was the only thing that worked, and it was the wrong surface.

Now: one verb, `RUN_VERB` (`protocol.rs`), whose args are a `ScriptRequest`.
`ScriptHook` became `ScriptHost` (`dispatch.rs`), implemented by
`SessionScriptHost` (`ferridriver-script/src/session_host.rs`). `session exec`
is gone; `session attach` is an ordinary script returning
`page.snapshotForAI()`.

- Wire: `ServerFrame` splits streamed `Event`s from the terminating
  `Response`. `serve_connection` interleaves them; `SessionClient::call_with_events`
  surfaces them. The `events_open` flag in the select loop is load-bearing —
  without it a dispatcher that drops its sink early spins the loop.
- Descriptors carry `WIRE_VERSION`; attaching across builds reports a version
  mismatch instead of failing to decode.
- Registry dir `0700`, socket `0600`. A peer that reaches the socket runs code
  in the host process — that is the whole access boundary.

### `ferridriver run --session <id> [--context c]`

The client bundles (its cwd is what relative imports resolve against) and the
host compiles (`compile_bundled_source`, split out of `bundle_and_compile`), so
QuickJS bytecode never crosses between differently-built binaries. Console
streams live; `--json` folds the streamed lines back into the one document.

`script_setup.rs` is the single place `run` and `session host` both resolve
sandboxes / caps / extensions / sidecars, so a script behaves the same locally
and against a session. Local `run` gained the `artifacts` binding it was
missing as a side effect.

### Per-session action attribution

`trace.rs` grew `observe_session_actions(composite, observer) -> guard`
alongside the process-global `set_action_observer`. Resolution happens in
`begin_action`, which **already receives the composite** — no new plumbing
through call sites. Unobserved path is unchanged (one relaxed atomic load); a
scoped observer costs one `RwLock` read + hash lookup per action, which is
noise next to a protocol round-trip.

This unlocked, in order:

1. `--trace` against a session (actions stream back as `EventPayload::Action`).
2. Code echo (below).

### Code echo / codegen-by-doing

`crates/ferridriver/src/codegen/echo.rs`: `line_for(&ActionInfo, language)`
renders any action as TypeScript, Rust `#[ferritest]`, or Gherkin — including
actions no curated emitter covers, because dropping them would misrepresent
what ran.

- CLI: `--code[=lang]` streams lines, `--code-out <file>` writes a runnable
  file. The generated file replays **both** standalone and against a session
  (verified by test), because the scaffolding defines `page` only when absent
  and every line drives plain `page`.
- MCP: `run_script` takes `code_language` and returns a `code[]` array under a
  `RunScriptOutput` schema. Nothing is installed when unasked.

### Bugs found and fixed along the way

- A long `FERRIDRIVER_SESSION_DIR` overflowed `sun_path` and made sessions
  unbindable. `bind.rs` now falls back to a hashed name under
  `$XDG_RUNTIME_DIR` (or the OS temp dir, per-user on macOS), in a `0700`
  subdirectory — the socket file's own mode is not consulted on connect on
  BSD-derived kernels, so the directory is the boundary.
- `backend::reaper`'s test asserted on a signal the kernel delivers
  asynchronously. That was the recurring "flaky" failure; it now polls.
- Generated TypeScript mixed `page` and `__page`. One vocabulary now.

### NAPI `browser.bind()`

Deliberately refuses, per an explicit decision: a session runs scripts, and the
Node addon is the core browser surface with no script engine. It points at the
two hosts that work (`ferridriver session open`, `browser.bind()` from a
ferridriver script). Revisit only if the addon is meant to carry QuickJS.

## The BDD failure, resolved

Not reproducible. `ferridriver bdd tests/features/` was run **28 times** —
15 on an idle machine, 13 more while cargo builds loaded the box (the
documented correlation) — and every run reported 574 passed / 0 failed / 22
skipped, 45–70s. No hang.

At the documented ~1-in-6 rate, 28 clean runs has probability `(5/6)^28` ≈
0.6%, so either the rate has dropped well below that or the original failure
needed conditions heavier than a compile (the run that saw it was sharing the
machine with the rest of the gate). The scenario name was never captured and
cannot now be recovered.

Conclusion: nothing attributable to this diff, and nothing left to chase
without a failure to look at. If it reappears, the harness that would have
caught it is a loop that greps the log for `✗` and stops — the reporter's
failure marker is `\u{2717}`, and the summary line is the other tell.

## Gate status

Green: clippy `-D warnings`, fmt, workspace lib tests, full CLI integration
serially, `ferridriver test`, `ferridriver bdd`, `bun test`.

## The response contract (done)

`crates/ferridriver/src/response.rs` is the shared builder: `Response`
(titled sections, markdown or one JSON object), `PageState`, `Secrets`, and
`OutputBudget`. Core is the lowest common dependency, so the MCP server, the
CLI and the session host all render the same shape.

- **Page state.** `PageState::capture` reads the URL from the frame cache, the
  title over the wire, and the console/page-error counts from the retained
  history — and opens no action span, so it never appears in echoed code. MCP
  `run_script` always includes it (`page` in the output schema); a session run
  asks for it with `ScriptRequest::page_state`, and the host replies with
  `EventPayload::Page` after the last action.
- **Secret redaction.** Configured as `[secrets] file = … / env = […]` (names
  in the document, values in a dotenv file or the environment). Applied at the
  engine — `ConsoleCapture::push` for every console line, the returned value
  and `ScriptError` in `Session::finish` — so no host can forget one of the
  three routes. Echoed code substitutes `process.env['NAME']` /
  `std::env::var("NAME")` / `<NAME>` instead of the literal, which is what
  makes a generated test committable. The host redacts before the wire, so a
  session client never receives the value.
- **Output budget.** `artifactsMaxBytes` evicts least-recently-modified files
  until the directory fits, never the ones the current call wrote.
  `PathSandbox` records every path it hands out for writing, which is how "the
  current call's outputs" is known; the long-lived hosts diff that record
  across the call.
- **Sections in the CLI**: `--report`, opt-in, because `ferridriver run`'s
  default output is a deliberate Node-shaped contract (stdout is console plus
  the return value, nothing else) with tests pinning it. `--raw` would have
  meant inverting that; the default already *is* the raw mode. With `--json`
  the sections fold into the document under `report`.

Two defects fell out of writing the tests, both fixed:

- **`retry_resolve!` recorded only the selector**, so every locator action
  echoed with empty parens — `fill()`, `press()`, `selectOption()`. The
  generated file did not reproduce the run. The macro now takes the call's own
  arguments, and `dragTo`'s target renders as a locator expression rather than
  a string (which would not have compiled).
- **`session open` dropped the global `--config`** when spawning its host, so a
  session opened with `-c` ran under a different configuration than the command
  that opened it. It forwards `--config` and `--no-inherit` now.

And one in the tests: several `run_command` cases drove a bare `page`, which
stock `ferridriver run` does not bind — they only passed because the
developer's own user-level config loads an extension that injects one. The
suite now runs `--no-inherit` and those scripts launch their own browser.

Left for later, from Playwright's version of this: a snapshot section written
to a file and referenced by path, and console as a link.

## The tool layer: measured, then fixed without a registry

The earlier plan here was a shared tool registry —
`{name, schema, capability, handler(ctx, params) -> Response}` in core, with
MCP, CLI one-shots and script `tools.*` all binding to it, on the grounds that
"MCP tools, script bindings and the session path are three implementations".

Measuring it first killed that plan, and the measurement is worth keeping:

- The session path stopped being a third implementation when it collapsed to
  `RUN_VERB`.
- There is **no duplicated logic** to hoist. MCP tools and script bindings are
  both already thin over core — handlers ran 18–47 lines, of which one was the
  core call.
- What *was* duplicated is ceremony: 20 `session_guard`, 17 `sess(p.session…)`,
  14 `self.page(s)` — the same three-line preamble seventeen times, in an order
  that is load-bearing (resolving the page before taking the guard races a
  concurrent call on a cold context into opening two pages).

So the fix is two helpers on `McpServer`, not an abstraction layer:

- `on_page(session, |page, s| …)` — resolve, guard, page, in that order, once.
  Also the single home for the `Box::pin` that `clippy::large_futures` wants.
- `on_session(session, |s| …)` — same, for tools that act on the context
  itself (list, close, read logs) and must not launch a browser to do it.

Counts after: `session_guard` 20 → 4, `self.page(` 14 → 4, `sess(p.session` 17
→ 4. The remaining four are `connect` (which must connect *before* a page
exists) and `run_script` (which holds its guard across a longer span).

**This also closed a real hole.** Redaction was wired at the script engine, so
it covered `run_script` and nothing else — `evaluate`, `snapshot`,
`search_page` and the extension-tool path returned raw `ContentBlock::text`,
and a credential sitting in the DOM came straight back. Every reply now goes
through `McpServer::text` / `ok_text`, which redacts. Regression test:
`response_contract` reads the filled password back through `evaluate` on all
four backends and asserts both that the raw value is absent and that the
redaction marker is present (so it cannot pass vacuously).

What a registry would still add, and why none of it is pulling yet: built-ins
callable from scripts (redundant — scripts have `page.snapshotForAI()`); CLI
one-shots (anti-thesis — `run --session` already covers the whole API, and a
command table is what we deleted); capability gating for built-ins (the one
real item, speculative until an operator asks). Revisit if a second non-MCP
host appears.

## Remaining work, ranked

### 1. `ferridriver test --debug`

Pause a worker, bind the live test context as `tw-XXXX` (the machinery all
exists now: `bind_in` with a real host), print the attach line. The agent then
runs bundled TypeScript against a test paused mid-fixture, with auth and seed
state live — strictly better than the lambda Playwright gives you there.

Open design question: `resume` / `step-over` / `pause-at` must not become new
protocol verbs (that would undo the whole "one verb" property). The natural
fit is a script-callable binding on the paused session, e.g.
`await testDebug.resume()`.

### 2. `init-skills` / `init-agents`

Embed a SKILL.md plus planner/generator/healer agent definitions in the binary
and write them into `.claude/skills/…` on demand. A single static binary beats
`npx` here. Mostly content work; see Playwright's
`packages/playwright-core/src/tools/skills/playwright-cli/SKILL.md` (12K, plus
nine reference docs) for the shape and depth expected.

### 3. Small surface wins

Trace CLI over our zips (`ferridriver trace actions|console|errors|…`),
snapshot `find` with context, `generate-locator`, `highlight`, `--mobile`
(cheaper snapshots).

## Traps worth knowing

- **Not every API opens an action span.** `page.title()` does not; `page.goto`
  and everything through `retry_resolve!` (locator actions) do. A trace/code
  test that picks the wrong method sees nothing and looks like a wiring bug.
  Check `begin_action` call sites before writing the assertion.
- **The session id is not the composite.** `RunContext.session` is the id the
  client addressed (`<id>:<context>`); actions and trace recorders are keyed by
  the browser state's own composite (`<instance>:<context>`), which
  `ContextRef::composite()` returns. Registering an observer under the wrong
  one silently observes nothing — this cost a debugging cycle.
- **A session opened by an older binary silently ignores new request fields.**
  Serde defaults them. When a new flag "does nothing", close and reopen the
  session before suspecting the code.
- **Long scratchpad paths break unix sockets.** Use a short
  `FERRIDRIVER_SESSION_DIR` for manual testing (`/tmp/fdsess`), or rely on the
  new hashed fallback.
- **A CLI integration test inherits the developer's own config.** The layered
  loader reads the machine and user layers before anything in the repo, so a
  `~/.config/ferridriver` that loads an extension can hand every `ferridriver
  run` a global (a `page`, say) that CI will not have. `run_command` passes
  `--no-inherit` for exactly this reason; anything asserting on stock
  behaviour must do the same, or it is testing the machine it runs on.
- **A trace span's params are the echoed call's arguments.** `retry_resolve!`
  used to record only `{selector}`, so `fill`/`press`/`selectOption` echoed
  with empty parens. When adding a locator action, pass its arguments as the
  macro's fourth argument or the generated code silently drops them — nothing
  fails, the file just does not reproduce the run.
- **`PathSandbox::written()` accumulates for the sandbox's whole life.** For a
  one-shot `run` that IS the run's output set; for the MCP server and the
  session host it is every call ever, so those diff it across the call before
  using it as the budget's keep-set. Passing the whole record protects
  everything and the ceiling never evicts.
- **The console sink is captured at VM build, not per run.** That is why the
  host routes through a retargetable `ConsoleRouter` per context rather than
  swapping the engine config.
