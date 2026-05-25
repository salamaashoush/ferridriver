# BDD JavaScript / TypeScript API

Cucumber-js-shaped surface, native-backed. Step bodies in JS / TS run
through the same Rust `TestRunner` that drives Rust `#[ferritest]`
tests — same workers, retries, reporters, fixtures.

## Step registration

```ts
Given("I navigate to {string}", async function (url: string) {
  await this.page.goto(url);
});

When("I click {string}", async function (selector: string) {
  await this.page.locator(selector).click();
});

Then("the URL contains {string}", async function (fragment: string) {
  if (!this.page.url().includes(fragment)) throw new Error("mismatch");
});

// Keyword-agnostic (matches Given / When / Then / And / But)
Step("I wait {int} seconds", async function (n: number) {
  await new Promise((r) => setTimeout(r, n * 1000));
});
```

Pattern can be a **Cucumber expression** (default) or a **RegExp**:

```ts
Given(/^I have (\d+) items$/, async function (count: string) {
  // count is the matched substring; parse if needed
});
```

### Per-step timeout

Per-step options object goes between pattern and handler:

```ts
Given("slow thing", { timeout: 30000 }, async function () { /* ... */ });
```

## Hooks

```ts
Before(async function () {
  await this.context.clearCookies();
});

// Tag-filtered
Before("@auth", async function () {
  await this.page.goto("https://app.example.com/login");
});

// With explicit options
Before({ tags: "@auth", name: "login", timeout: 10000 }, async function () { /* ... */ });

After(async function (result) {
  if (result?.result?.status === "FAILED") {
    this.attach(await this.page.screenshot(), "image/png");
  }
});

BeforeStep(async function () { /* before every step */ });
AfterStep(async function () { /* after every step */ });

BeforeAll(async () => { /* once per run */ });
AfterAll(async () => { /* once per run */ });
```

Tag expressions support the full boolean grammar: `@smoke and not @wip`,
`(@fast or @critical) and not @wip`.

`After*` hooks run even when earlier hooks or steps failed (cleanup
guarantee). Hook order: `Before` hooks run in ascending `order`, `After`
hooks run in descending — cleanup mirrors setup.

## The World

`this` inside any step or hook is the **World** — a per-scenario object
carrying fixtures and helpers:

| Property              | Type            | Notes |
|-----------------------|-----------------|-------|
| `this.page`           | `Page`          | Live browser page (Playwright-shaped). |
| `this.context`        | `BrowserContext`| Cookies, permissions, init scripts, geolocation. |
| `this.browser`        | `Browser`       | Multi-page operations. |
| `this.request`        | `HttpClient`    | Runner-side HTTP. Net-restricted if `allow.net` is set. |
| `this.parameters`     | `Record<string, any>` | `--world-parameters` JSON. |
| `this.attach(content, mediaType?)` | function | Attach bytes / strings to the test report (screenshots, logs). |
| `this.log(message)`   | function        | Free-text log line attached to the report. |
| `this.skip()`         | function        | Mark scenario as skipped (throws the `__ferri_skip__` sentinel). |

### Custom World

```ts
setWorldConstructor(class MyWorld {
  constructor({ parameters }: { parameters: Record<string, any> }) {
    this.tenant = parameters.tenant ?? "default";
  }
  tenant: string;
});
```

`setWorldConstructor` is per-VM (last call wins). Fixtures (`page`,
`context`, `browser`, `request`) are augmented onto the instance after
construction.

## Step return values

| Return / throw                | Result |
|-------------------------------|--------|
| (nothing) / resolved promise  | **passed** |
| string `"pending"`            | **pending** (yellow; `--strict` makes it fail) |
| string `"skipped"` or `this.skip()` | **skipped** |
| throw                         | **failed** — error remapped to original `.ts` / `.js` location via the rolldown source map, including the stack |

## Parameter types

Built-in Cucumber expression parameters:

| Type        | Regex                       | TypeScript |
|-------------|-----------------------------|------------|
| `{string}`  | `"[^"]*" \| '[^']*'`         | `string`   |
| `{int}`     | `[+-]?\d+`                  | `number`   |
| `{float}`   | `[+-]?\d+\.\d+`             | `number`   |
| `{word}`    | `\S+` (non-whitespace)      | `string`   |
| `{}`        | `\S+` (anonymous)           | `string`   |

### Custom parameter type

```ts
defineParameterType({
  name: "color",
  regexp: "red|green|blue",
  transformer: (s: string) => ({ name: s, hex: colorMap[s] }),
});

Given("I pick {color}", async function (color: { hex: string }) {
  // color.hex is "#ff0000" etc.
});
```

Or shorthand: `defineParameterType("color", "red|green|blue")`.

Type inference: `Given('I have {int} {string}', (count, item) => {})`
gives `count: number, item: string` in TS-aware editors.

## Data tables

A step that ends with a table receives a `DataTable` argument by name:

```gherkin
Given I have these users:
  | name | role  |
  | Ada  | admin |
  | Grace| editor|
```

```ts
Given("I have these users:", async function (table: DataTable) {
  for (const row of table.hashes()) {
    // row = { name: "Ada", role: "admin" }
    await this.request.post("/users", { data: row });
  }
});
```

### `DataTable` methods

| Method            | Returns                              | Notes |
|-------------------|--------------------------------------|-------|
| `raw()`           | `string[][]`                         | All rows including header |
| `rows()`          | `string[][]`                         | Data rows (header excluded) |
| `hashes()`        | `Record<string, string>[]`           | One object per data row, keyed by header |
| `rowsHash()`      | `Record<string, string>`             | Two-column tables → `{key: value, ...}` |
| `transpose()`     | `DataTable`                          | Swap rows / columns |

## Doc strings

A `"""` block after a step is passed as a string argument:

```gherkin
When I send this JSON:
  """json
  { "name": "Ada", "role": "admin" }
  """
```

```ts
When("I send this JSON:", async function (body: string) {
  await this.request.post("/users", { data: JSON.parse(body) });
});
```

Media-type hints (`"""json`, `"""yaml`) are parsed and surfaced in the
report but do not change the value type (always `string`).

## Defaults and globals

```ts
setDefaultTimeout(10000);                   // ms; per-registry default
setDefinitionFunctionWrapper((fn) => fn);   // wrap every step body (retry, trace)
setParallelCanAssign((/* ignored */) => true); // accepted but inert
```

`setParallelCanAssign` is accepted for cucumber-js compat but is inert:
ferridriver parallelises at the test-runner worker level (one VM per
worker), not cucumber-js's per-pickle scheduler.

## Built-in Rust step library

There is a **shipped Rust step library** (`ferridriver-bdd/src/steps/`)
of 144 steps — see [Built-in steps](/bdd/steps). Those are registered
via `#[given]` / `#[when]` macros on the Rust side and merge into the
same registry as your JS / TS steps. A `.feature` file can mix steps
from both languages freely.

## Imports

```ts
import { helper } from "./helpers.js";          // resolved by rolldown
import * as utils from "../shared/utils.ts";    // TS imports work
import semver from "semver";                    // node_modules bundled
```

No bare specifier resolution from inside extensions / steps **outside**
what rolldown can resolve at bundle time. There is no runtime
`require()` and no Node module loader.

## See also

- [BDD overview](/bdd/overview) — Rust + JS, hybrid suites
- [BDD running](/bdd/running) — CLI, reporters, profiles
- [Extensions](/scripting/extensions) — sharing a file between MCP + BDD
- [Sandbox](/scripting/sandbox) — `process`, `fetch`, `fs`, what is absent
