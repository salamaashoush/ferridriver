# @ferridriver/test

Playwright-compatible E2E, BDD, and component test runner for Node.js and Bun, powered by the ferridriver Rust engine.

## Install

```bash
npm install -D @ferridriver/test
# or
bun add -d @ferridriver/test
```

The install pulls in `@ferridriver/node` (the native addon) via `optionalDependencies`. Platform binaries are published for `darwin-arm64`, `linux-x64-gnu`, `linux-arm64-gnu`, and `win32-x64-msvc`.

## Usage

```ts
// tests/login.spec.ts
import { test, expect } from '@ferridriver/test';

test('login flow', async ({ page }) => {
  await page.goto('https://app.example.com/login');
  await page.locator('#email').fill('user@example.com');
  await page.locator('button[type=submit]').click();
  await expect(page).toHaveURL(/dashboard/);
});
```

```bash
npx @ferridriver/test test tests/
```

## CLI

Four subcommands share the same runner flags:

| Command | Purpose |
|---|---|
| `test` | Run `.spec.ts` / `.test.ts` / `.feature` files (E2E + BDD, auto-detected) |
| `ct` (alias `component`) | Run component tests against a Vite dev server |
| `codegen URL` | Record interactions; emit Rust / TypeScript / Gherkin |
| `install [BROWSER]` | Download Chromium (add `--with-deps` for system libs) |

**Shared runner flags** (common subset):

```
-j, --workers <N>           parallel workers
    --retries <N>           retry failed tests
    --timeout <MS>          per-test timeout
    --headed                visible browser window
-g, --grep <RE>             filter test names
    --grep-invert <RE>      exclude test names
    --shard <CUR/TOTAL>     CI shard selection
    --tag <NAME>            filter by tag annotation
    --backend <B>           cdp-pipe | cdp-raw | webkit | bidi
    --browser <B>           chromium | firefox | webkit (sets default backend)
    --reporter <R>          terminal | junit | json
    --video <M>             off | on | retain-on-failure
    --trace <M>             off | on | retain-on-failure | on-first-retry
    --update-snapshots      refresh stored snapshots
    --list                  list discovered tests without running
    --forbid-only           fail if any test.only() is present (CI safety)
    --last-failed           re-run only previously failed tests
    --storage-state <PATH>  pre-authenticated storage state
-w, --watch                 re-run on file changes
-v, --verbose               debug-level logging
    --debug <CATS>          cdp,steps,action,worker,fixture
    --output <DIR>          report + artifact directory
    --profile <NAME>        config profile override
    --web-server-dir <DIR>  serve static dir (sets base_url)
    --web-server-cmd <CMD>  run dev server before tests
    --web-server-url <URL>  URL to wait for with --web-server-cmd
-c, --config <PATH>         config file path
```

**`test`-only flags:** `--steps <GLOB>` (append), `-t, --tags "<EXPR>"`, `--strict`, `--order defined|random[:SEED]`, `--language <LANG>` (Gherkin keyword language).

**`ct`-only flags:** `--framework react|vue|svelte|solid` (default `react`), `--register-source <PATH>`.

**`codegen` flags:** positional `URL`, `-l, --language rust|typescript|bdd` (default `rust`), `-o, --output <FILE>`, `--viewport <WxH>`.

## Public API

```ts
import {
  test, describe, expect,                    // runner + assertions
  defineConfig,                               // config helper
  Given, When, Then, Step, defineStep,        // BDD step registration
  Before, After, BeforeAll, AfterAll,         // lifecycle hooks
  BeforeStep, AfterStep,
  defineParameterType,                        // BDD custom param types
  setDefaultTimeout, setWorldConstructor,     // BDD runtime config
  Pending, Status, DataTable,                 // BDD types
  configureBdd, runFeatures,                  // BDD programmatic API
  version,
} from '@ferridriver/test';
```

Type exports: `MountFunction`, `FerridriverTestConfig`, `UseOptions`, `ProjectConfig`, `WebServerConfig`, `ExpectConfig`, `TestRunnerConfig`, `TestFixtures`, `TestInfo`, `StepContext`, `StepCallback`, `HookCallback`, `HookOptions`, `StepOptions`, `ParameterTypeOptions`.

Subpath exports: `@ferridriver/test/config`, `@ferridriver/test/bdd`.

## Features

- Auto-retrying `expect` matchers (Playwright-compatible). 13 are exposed in the TypeScript surface today; the Rust core has 38 total. All polling and actionability checks happen in Rust — the TS wrapper is a thin shim with zero NAPI round-trips per retry.
- Parallel workers with browser-per-worker isolation
- BDD / Gherkin support — mixed `.spec.ts` + `.feature` runs in one invocation
- Component testing via `@ferridriver/ct-{react,vue,svelte,solid}` adapters
- Text and visual (pixel-diff) snapshots
- Video recording, CDP traces, screenshots on failure
- `--grep`, `--tag`, `--shard`, `--last-failed` filtering
- JUnit XML and JSON reporters for CI

## Documentation

See the [ferridriver README](https://github.com/salamaashoush/ferridriver) for architecture and the full Page/Locator API.

## License

MIT OR Apache-2.0
