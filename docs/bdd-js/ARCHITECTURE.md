# JavaScript BDD on the shared QuickJS engine

## Goal

One Rust BDD command. JavaScript (and, later, TypeScript) step files run on
the same `ferridriver-script` QuickJS engine that powers `ferridriver run`
and the MCP `run_script` tool. No Node, no Bun, no second JS runtime. The
TS CLI and the NAPI test runner are retired (see `REMOVAL_INVENTORY.md`).

`ferridriver` (Rust) stays the source of truth: Gherkin parsing, scenario
and outline expansion, tag filtering, Cucumber-Expression matching, hook
ordering, retries, parallelism and reporters all live in
`ferridriver-bdd` / `ferridriver-test`. JavaScript only supplies step
*bodies*.

## One engine, two entry points

`ferridriver-script` owns the VM and every binding (`page`, `locator`,
`context`, ...). It exposes two ways in:

- **scripting / MCP** — `RunContext` carries `page`/`context`/`request`/
  `browser`; bindings install onto `globalThis`; `globalThis` persists
  REPL-style across `Session::execute`.
- **BDD** — the cucumber surface (`Given`/`When`/`Then`/`Before`/`After`/
  `defineParameterType`/`setWorldConstructor`/`setDefaultTimeout`/...) is
  installed as **native Rust `Func`s**. Registrations land in a Rust
  `BddRegistry` held as QuickJS context userdata (the context is
  single-threaded, so the registry uses `RefCell`, never `Arc`/`Mutex`).
  Step bodies are kept as `Persistent<Function>` and called back by the
  Rust core.

There is no JavaScript shim string and no `globalThis.__*` registry. The
binding layer is shared, not duplicated: `install_page_on` /
`install_browser_context_on` / `install_browser_on` /
`install_request_on` take a target object. Scripting passes
`ctx.globals()`; BDD passes the per-scenario World object. The same
`PageJs`/`LocatorJs`/... wiring serves both.

## Context model: the World is a first-class object

cucumber-js gives each scenario a fresh World; step `this` is that World;
it carries `attach`/`log`/`parameters` plus user state. In ferridriver the
World is a real JS object built per scenario by the Rust core:

1. The core builds a `ScenarioWorld` from that scenario's `TestFixtures`
   (`page`/`context`/`request`/`browser`).
2. `set_scenario_world` creates the World object (or constructs the user's
   `setWorldConstructor` class), augments it with `attach`/`log`/
   `parameters`, and installs the fixtures via the shared
   `install_*_on` helpers.
3. Each step is invoked with that World as `this`
   (`Args::this(world)` + `Function::apply`), the Cucumber-Expression
   captured parameters, then the optional `DataTable` and doc string.

`DataTable` is a real `#[rquickjs::class]` (`raw`/`rows`/`hashes`/
`rowsHash`/`transpose`), not a JS object literal.

## Parallelism

A QuickJS `AsyncContext` is single-threaded; the `parallel` rquickjs
feature only makes the handle `Send`. The step registry is per-VM JS
state (it is populated by evaluating the step files in that VM, exactly
as plugins install per session VM). Therefore: **one engine session per
`ferridriver-test` worker.** Each worker evaluates the step files once
and builds its own `StepRegistry` from that VM's registry snapshot;
scenarios run in parallel across workers, each VM driving its own
scenarios sequentially. A single shared VM would be correct but would
serialize all scenarios behind the context lock.

## Error mapping

A thrown JS error is converted by the script engine's existing
`caught_to_script_error` into a `ScriptError` carrying message, JS stack
and (when QuickJS exposes it) line/column plus a source snippet. The BDD
core surfaces that on the failing step, e.g.

```
Then this step always fails  -> Failed("boom from js step
    at <anonymous> (eval_script:35:13)")
```

When step files are loaded through the sandbox module loader by path
(see `DECISIONS.md`), the stack reports the real `.js` filename instead
of `eval_script`.

## Mapping table: cucumber-js  <->  ferridriver

| cucumber-js (`@cucumber/cucumber`) | Where it runs | Notes |
|---|---|---|
| `Given/When/Then/defineStep/And/But(pattern, opts?, fn)` | native `Func` -> `BddRegistry.steps`; matched by `ferridriver-bdd` Cucumber-Expression engine | string -> CucumberExpression, `RegExp` -> regex (via `.source`) |
| `Before/After/BeforeStep/AfterStep(tagsOrOpts?, fn)` | native `Func` -> `BddRegistry.hooks`; ordering/tag-filter in core (`TagExpression`) | After runs reverse order |
| `BeforeAll/AfterAll(opts?, fn)` | native `Func` -> `BddRegistry.hooks` | run once per worker session |
| `setDefaultTimeout(ms)` | native `Func` -> `BddRegistry.default_timeout_ms` | enforced by core step timeout |
| `setWorldConstructor(Class)` | native `Func` -> `Persistent<Constructor>`; constructed per scenario, fixtures augmented | |
| `setParallelCanAssign(fn)` | accepted; parallelism decided by core worker model | |
| `defineParameterType({name, regexp, transformer?})` | name+regexp -> core param-type registry | transformer execution: see `DECISIONS.md` |
| `DataTable` (`raw/rows/hashes/rowsHash/transpose`) | `#[rquickjs::class] DataTableJs` | passed as trailing step arg |
| World `this` + `attach/log/parameters` | Rust-built per-scenario object | `attach`/`log` wired to `TestInfo`: see `DECISIONS.md` |
| return `'pending'` / `'skipped'` | `StepOutcome::Pending/Skipped` | cucumber return protocol |
| Gherkin parse / outline / tags / Background / Rule | `ferridriver-bdd` core (unchanged) | `feature`/`scenario`/`filter` |
| retries / sharding / reporters / workers | `ferridriver-test` core (unchanged) | |

## Code map (delivered)

- `crates/ferridriver-script/src/bindings/bdd.rs` — native cucumber
  surface, `BddRegistry` userdata, `DataTableJs`, `collect_registry`,
  `set_scenario_world`, `invoke_step`, `invoke_hook`, `reset_world`.
- `crates/ferridriver-script/src/bindings/mod.rs` — `install_*_on`
  target-parameterized helpers (shared by scripting and BDD).
- `crates/ferridriver-script/src/engine.rs` — `Session::async_context()`,
  `install_bdd` wired into `Session::create`, `caught_to_script_error`
  exposed `pub(crate)`.
- `crates/ferridriver-bdd/src/js.rs` — `JsBddSession`: loads step files in
  the shared session, builds the real `StepRegistry`, runs scenarios
  through the core (`feature`/`scenario`/`filter`/`registry`).
- `crates/ferridriver-bdd/tests/js_steps.rs` + `tests/fixtures/` —
  end-to-end proof: pass, fail-with-source-location, data table,
  scenario outline, tag filter.
