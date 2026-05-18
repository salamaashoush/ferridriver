# Migration

## TS CLI users (`@ferridriver/test`, `bun run src/cli.ts test … .feature`)

- Step files stay cucumber-js shaped: `Given`/`When`/`Then`/`Before`/
  `After`/`BeforeAll`/`AfterAll`/`defineParameterType`/
  `setWorldConstructor`/`setDefaultTimeout` are global, same overloads
  (`(pattern, fn)`, `(pattern, opts, fn)`, `(tags, fn)`, `(opts, fn)`).
- `this` is still the World, with `attach`/`log`/`parameters`. `DataTable`
  exposes `raw`/`rows`/`hashes`/`rowsHash`/`transpose`.
- Cucumber return protocol preserved: returning `'pending'` / `'skipped'`.
- Run with `ferridriver bdd` (single static binary) instead of
  `bun run … test`. No `package.json`, no `node_modules`, no Bun.
- Reporters are the core `ferridriver-test` reporters (cucumber-json,
  junit, messages, terminal, ...). Names map straight across.
- Behavioural differences to expect: see `DECISIONS.md` (TS step files,
  parameter-type transformers, `world` proxy export, custom World).

## NAPI test-runner users (`@ferridriver/node` `TestRunner`/`test()`/`expect`)

- The NAPI test runner is removed. `@ferridriver/node` becomes a thin
  "core Playwright in Rust" binding: `Browser`/`BrowserContext`/`Page`/
  `Frame`/`Locator`/`ElementHandle`/`Mouse`/`Keyboard`/network/dialog.
  Build your own runner on top, or move tests to `ferridriver bdd` /
  the Rust test harness.
- `expect()` moves into `ferridriver` core (still reachable from the
  thin node binding if retained — see `REMOVAL_INVENTORY.md` step 1).

## Rust `#[given]`/`#[when]`/`#[then]` macro-step users

- Unaffected and still supported. The inventory-collected Rust step
  registry is built first; JS-registered steps are added on top of the
  same `StepRegistry`. Rust and JS step definitions coexist; matching is
  keyword-agnostic and ambiguity is reported as today.
- A feature suite can be all-Rust, all-JS, or mixed.
