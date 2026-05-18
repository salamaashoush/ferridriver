# Decisions and open points

## Resolved

- **World model**: the cucumber World is a first-class object. The shared
  `install_*_on` helpers install `page`/`context`/`request`/`browser`
  onto a per-scenario World (step `this`); scripting keeps installing
  onto `globalThis`. One binding implementation, two install targets.
- **Parallelism**: one engine session per `ferridriver-test` worker;
  per-VM step registry. Real parallel scenarios; no shared-VM
  serialization.
- **Glue location**: `ferridriver-bdd` gains a `js` module and a
  `ferridriver-script` dependency. No new crate. `script` stays
  bdd-agnostic; dependency direction `bdd -> script -> ferridriver` is
  acyclic.
- **No JS shim**: the cucumber surface is native Rust `Func`s; the
  registry is Rust state in context userdata (`RefCell`, not
  `Arc`/`Mutex` — single-threaded VM). No `globalThis.__*`.

## Open — need a decision before the TS CLI is deleted

1. **TypeScript step files.** The TS CLI accepts `.ts` steps. QuickJS
   runs JS only. Options: (a) bundle/transpile `.ts` -> `.js` ahead of
   load with an embedded transpiler (e.g. an SWC/oxc Rust crate) — keeps
   the single binary, adds a build dep; (b) JS-only, document `.ts` as
   unsupported; (c) accept precompiled `.js` (user runs their own
   tsc/esbuild). Recommendation: (a) for true TS-CLI parity.
2. **Step-file discovery + CLI surface.** Mirror cucumber-js / ts-cli:
   a `[bdd].require`/`import` glob list (config) plus a CLI flag (e.g.
   `--steps <glob>`/`--import <glob>`). Decide config key names and
   whether ESM `import './helpers.js'` resolves through the existing
   sandbox module loader (recommended) vs flat eval.
3. **Full `TestRunner` integration.** The delivered `JsBddSession` runs
   scenarios through the core `feature`/`scenario`/`filter`/`registry`
   but not yet through `TestRunner` (parallel workers, retries,
   reporters, fixtures). Target: `translate_scenario` builds `TestCase`s
   whose `test_fn` drives a worker-scoped `JsBddSession` built from the
   worker's `TestFixtures`. Decision: confirm the per-worker Session
   lifecycle (created lazily on first JS scenario per worker, dropped at
   worker shutdown) and how `setWorldConstructor`/`BeforeAll` interact
   with per-worker VMs.
4. **`defineParameterType` transformer.** Matching/extraction is
   Rust-side; the JS `transformer` is not executed (parameters arrive as
   strings/typed by the core's built-in types). Options: run the JS
   transformer per captured arg before invoking the step (extra JS
   round-trip), or document Rust-side custom parameter types as the
   supported path.
5. **`attach`/`log`.** Currently no-ops on the World. Wire to the
   scenario `TestInfo` (attachments, cucumber-messages) — needed for
   cucumber-json/messages reporter parity.
6. **`world` / `context` proxy exports.** cucumber-js exports a `world`
   proxy for arrow-function steps. Decide whether to expose an
   equivalent (an `AsyncLocalStorage`-style current-World accessor) or
   require `function` steps (so `this` binds).
7. **Custom World + fixtures precedence.** When `setWorldConstructor` is
   used, the core constructs the class and augments the instance with
   `page`/`context`. Confirm precedence if the user's World defines its
   own `page`.
8. **Step file load path for diagnostics.** Loading via the sandbox
   module loader by path makes JS stack traces report the real filename
   (instead of `eval_script`). Recommended; confirm sandbox-root rules
   for step directories.

## Status of the delivered foundation

- Native cucumber bindings on the shared engine: built, clippy-clean
  (`cargo clippy -p ferridriver-script -p ferridriver-bdd --all-targets
  -- -D warnings`).
- `ferridriver-script` lib tests: pass (no scripting regression).
- `ferridriver-bdd/tests/js_steps.rs`: passes — JS steps via the shared
  engine through the Rust core: passing scenario, failing step with JS
  source location, data table, scenario outline, tag filter.
- Not yet done (the open points above): CLI step-glob wiring, full
  `TestRunner`/parallel integration, TS transpile, `attach`/`log`
  wiring, transformer execution.
