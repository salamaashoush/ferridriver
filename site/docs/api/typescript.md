# TypeScript API reference

`@ferridriver/node` ships its own `index.d.ts` with all classes and interfaces. The test runner at `@ferridriver/test` re-exports a curated Playwright-compatible surface plus BDD helpers.

## `@ferridriver/node`

Classes: `Browser`, `BrowserContext`, `Page`, `Frame`, `Locator`, `Route`, `Keyboard`, `Mouse`, `ApiRequestContext`, `ApiResponse`, `Codegen`, `StepHandle`, `TestFixtures`, `TestInfo`, `TestRunner`.

Helpers: `findInstalledChromium()`, `findInstalledHeadlessShell()`, `getBrowserCacheDir()`, `installChromium()`, `installChromiumHeadlessShell()`, `installChromiumWithDeps()`, `installSystemDeps()`.

## `@ferridriver/test`

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

Subpath exports: `@ferridriver/test/config`, `@ferridriver/test/bdd`.

## Component-testing adapters

Each adapter package exports only the framework glue:

```ts
import { registerSourcePath, frameworkName, vitePlugin } from '@ferridriver/ct-react';
// same surface for ct-vue, ct-svelte, ct-solid
```

The `test`, `expect`, and `mount` fixtures come from `@ferridriver/test`, not from the adapter.

## Generated reference

An auto-generated TypeDoc page is on the roadmap. Until then, types are discoverable via your editor or by reading [`index.d.ts`](https://unpkg.com/@ferridriver/node/index.d.ts) directly.
