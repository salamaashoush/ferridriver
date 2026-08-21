# Configuration

ferridriver looks for `ferridriver.{toml,yaml,yml,json}` in the current
directory, then in `~/.config/ferridriver/`. Override with `-c PATH`.
Wire keys are **camelCase**.

## Example

```toml
[test]
workers       = 4
timeout       = 30000
expectTimeout = 5000
retries       = 1
fullyParallel = true
outputDir     = "test-results"

[test.browser]
backend  = "cdp-pipe"
headless = true

[test.browser.viewport]
width  = 1280
height = 720

[[test.reporter]]
name = "terminal"

[[test.reporter]]
name = "html"
```

## Projects (matrix runs)

```toml
[[test.projects]]
name = "chromium"
[test.projects.browser]
browser = "chromium"

[[test.projects]]
name = "firefox"
[test.projects.browser]
browser = "firefox"
backend = "bidi"

[[test.projects]]
name = "webkit"
[test.projects.browser]
browser = "webkit"
backend = "webkit"
```

Run a single slice with `--project firefox`.

### BDD projects

A project can also bring its own Gherkin corpus — `features` globs,
`steps` globs, and `tags`, a Cucumber tag EXPRESSION with the grammar
`--tags` takes:

```toml
[[test.projects]]
name = "smoke"
tags = "@smoke and not @wip"

[[test.projects]]
name = "regression"
tags = "@regression"
```

Those three are a discovery, not a narrowing: `features` chooses
different files, `steps` a different registry, `tags` a selection made
before the outline rows exist. So a project naming any of them is
planned separately, and two projects with different `steps` get
different worker VMs rather than silently sharing the first one's step
definitions. Two projects that resolve to the same three inputs share
one build, and a project naming none of them narrows the shared plan
exactly as it did before.

`tags` is distinct from `tag`, which is a list of Playwright test tags
that must all be present.

## `use` and option fixtures

`use` is the Playwright `use` block: context options (`locale`,
`colorScheme`, `testIdAttribute`, …) plus any key of your own. A key no
built-in option claims is the value of a fixture registered with
`{ option: true }`.

It may be written at the top of `[test]`, where Playwright writes it, or
as `[test.browser.use]`; they are the same block, and a key written in
both keeps the `browser.use` one. A project takes `[test.projects.use]`
or `[test.projects.browser.use]` for the same reason.

```toml
[test.use]
locale  = "de-DE"
profile = "guest"          # -> the `profile` option fixture

[[test.projects]]
name = "admin"
[test.projects.use]
profile = "admin"          # -> only this project's tests
```

### Devices

`device` names an entry of Playwright's device registry and pre-seeds
every key that descriptor carries — `userAgent`, `viewport`, `screen`,
`deviceScaleFactor`, `isMobile`, `hasTouch` and `defaultBrowserType`.
Anything you write beside it wins, in that layer or a higher one:

```toml
[[test.projects]]
name = "phone"
[test.projects.use]
device = "iPhone 15"       # -> webkit, 393x659, dpr 3, touch
locale = "de-DE"           # -> beside the device, not overwritten by it
```

The same table is a value on the framework module, so a TypeScript
config spreads it the way a Playwright config does:

```ts
import { defineConfig, devices } from '@ferridriver/test';

export default {
  test: defineConfig({
    projects: [{ name: 'phone', use: { ...devices['iPhone 15'] } }],
  }),
};
```

### Runner options in `use`

Playwright spells `baseURL`, `actionTimeout`, `navigationTimeout`,
`trace`, `video` and `screenshot` inside `use`, so they are resolvable
per project. Each wins over the top-level `[test]` key that says the
same thing:

```toml
[test.use]
baseURL           = "http://localhost:3000"
actionTimeout     = 5000
navigationTimeout = 15000
trace             = "retain-on-failure"
video             = "off"
screenshot        = "only-on-failure"
```

`trace`, `video` and `screenshot` each take a mode or an object:

```ts
use: {
  trace: { mode: 'on', snapshots: true, sources: false, attachments: false },
  video: { mode: 'retain-on-failure', size: { width: 640, height: 480 } },
  screenshot: { mode: 'only-on-failure', fullPage: false },
}
```

The modes are Playwright's, whole: `off`, `on`, `retain-on-failure`,
`on-first-retry`, `on-all-retries`, `retain-on-first-failure` and
`retain-on-failure-and-retries` for `trace` and `video`; `off`, `on`,
`only-on-failure` and `on-first-failure` for `screenshot`. Recording and
keeping are separate decisions — `retain-on-failure` records every
attempt and keeps only the failures, while `on-first-retry` does not
record the first attempt at all.

`screenshotOnFailure` is the older boolean spelling of
`screenshot = "only-on-failure"`. `actionTimeout` and `navigationTimeout`
are test-scoped, so a spec may also set them with `test.use({ … })`;
`trace`, `video` and `screenshot` are worker options and come from the
config or the project.

`defaultBrowserType` only decides the engine nobody else named — an
explicit `browserName`, or a project browser block, wins. `viewport` in
`use` overrides `[test.browser].viewport`, the spelling that predates
the block, and `viewport = null` (in YAML/JSON/TS) asks for no fixed
viewport at all, which is not the same as leaving it out.

## Viewport

Written nowhere, the viewport is Playwright's default — **1280x720** —
for every host: the test runner, the MCP server and `ferridriver run`
alike. `viewport = null` is the only way to ask for none, and it means
the page takes whatever size the browser window happens to have.

The distinction matters most on a persistent profile. A browser launched
with a `userDataDir` comes back at the window bounds its last run left
behind, so a host that emulates nothing inherits whatever size that
browser was resized to — and one launch then decides the viewport of
every session after it. Emulating the default is what keeps a run
reproducible.

Declare it once at the top level and every host inherits it:

```toml
[browser.viewport]
width  = 1280
height = 720
```

```yaml
# Or opt out of viewport emulation entirely
browser:
  viewport: null
```

A section that states its own wins: `[mcp.browser].viewport` and
`[test.browser].viewport` (or `use: { viewport }`) each override the
top-level key without restating the rest of it. Precedence, most
specific first:

| Where | Applies to |
|-------|------------|
| `use: { viewport }` in a spec or project | that test / project |
| `[test.browser].viewport` | the test runner |
| `[mcp.browser].viewport` | the MCP server |
| `[browser].viewport` | every host |
| unwritten | 1280x720 |

Pages ferridriver opens are emulated with
`Emulation.setDeviceMetricsOverride`, and on a headed Chromium the
window is resized to match (viewport plus the platform's window chrome),
so what you see is what the page reports.

```ts
const test = base.extend<{ profile: string }>({
  profile: ['guest', { option: true }],
});

test('reads it', async ({ profile }) => { /* 'guest' or 'admin' */ });
```

Precedence, innermost first: a `test.use({ … })` in the spec — which
takes `device` too — then the project's block, then the config's, then
the fixture's own default. Each
layer overlays key by key, so a project setting one key keeps the
config's others.

A key naming a fixture that is NOT an option (including `page`,
`context`, `request` and `browser`) fails the run — only option fixtures
can be set from a config. A key naming nothing at all is reported as
`use.unknownKey` and ignored.

## Web server

```toml
[[test.webServer]]
command            = "npm run preview"
url                = "http://localhost:4173"
reuseExistingServer = true
timeout            = 60000
```

Multiple `[[test.webServer]]` blocks can run in parallel.

## Config in TypeScript

`.ts` / `.js` is a config **format**, not a special case. A
`ferridriver.config.ts` is discovered in the same places a
`ferridriver.toml` is, holds the same document, layers by the same
rules, and shadows the same way when two formats sit in one directory:

```ts
// ferridriver.config.ts — the same document ferridriver.toml holds
export default {
  test: {
    testDir: './specs',
    projects: [{ name: 'chromium' }, { name: 'firefox' }],
  },
};
```

```bash
ferridriver test        # discovered; no flag needed
```

Discovered basenames, in precedence order per directory:
`ferridriver.toml`, `.yaml`, `.yml`, `.json`, then
`ferridriver.config.ts`, `.mts`, `.js`, `.mjs`. `--config <path>` names
one explicitly, in any of those formats.

The module is bundled and evaluated through the same rolldown → QuickJS
pipeline every spec takes, so a config can import helpers and be written
in TypeScript with no build step.

**A configuration written in documents never constructs a bundler or a
JavaScript runtime to read itself.** The loader is installed only when
the stack actually holds a module layer, so a Rust suite on
`ferridriver.toml` pays nothing for a format it does not use — measured
at +2.1 ms above process floor for a document config against +4.9 ms for
a module one.

Four settings cannot come from a module, and each is refused by name
rather than ignored: `extensions`, `bundler`, `scripting` and
`[test].moduleAliases`. Every one of them had to be read before any
module could be compiled, which is why the stack folds the documents
first and the modules second. Put them in a `.toml` / `.yaml` / `.json`
layer.

### `defineConfig`

`defineConfig` is Playwright's function and folds Playwright's shape,
which is ferridriver's `[test]` section — so it goes inside the
document, not around it:

```ts
import { defineConfig } from '@ferridriver/test';

export default {
  test: defineConfig({
    use: { baseURL: 'http://localhost:3000' },
    projects: [{ name: 'chromium' }],
  }),
};
```

It folds layers rightmost-winning: scalars are replaced; `use`, `expect`
and `build` merge one level deep; `webServer` normalizes each side to a
list and concatenates; `projects` merge by `name`, each match taking the
incoming project's `use` on top, with new names appended.

## Priority

Lowest to highest:

1. Extension `defineDefaults` contributions
2. Config file defaults, in any format — a `.ts` layer is a config file
3. `main!()` / `HarnessConfig` macro arguments (Rust)
4. Environment variables — `FERRIDRIVER_BACKEND`, `FERRIDRIVER_WORKERS`,
   `FERRIDRIVER_TIMEOUT`, `FERRIDRIVER_RETRIES`, …
5. CLI flags — `--headless`, `--backend`, `--workers`, `--timeout`, …

## Profiles

Named presets that merge into the base config via `--profile NAME`,
passed through the Rust test harness:

```toml
[test.profiles.ci]
workers = 8
retries = 2
[[test.profiles.ci.reporter]]
name = "junit"
[[test.profiles.ci.reporter]]
name = "github"
```

```bash
cargo test --test e2e -- --profile ci
```

## Full schema

The `TestConfig` Rust type is the canonical reference. Notable fields:

| Field                  | Type      | Default | Notes |
|------------------------|-----------|---------|-------|
| `testMatch`            | `Vec<String>` | `[]` | Glob patterns for test files (JS / TS path) |
| `timeout`              | `u64`     | 30000   | Per-test timeout (ms) |
| `expectTimeout`        | `u64`     | 5000    | Assertion polling timeout (ms). Older spelling of `expect.timeout`; the nested key wins when both are set |
| `expect`               | object    | `{}`    | Matcher defaults — see [Expect block](#expect-block) |
| `workers`              | `u32`     | 0       | 0 = number of logical CPUs |
| `retries`              | `u32`     | 0       | Per-test retries on failure |
| `fullyParallel`        | `bool`    | false   | Treat all tests as parallel even within suites |
| `repeatEach`           | `u32`     | 1       | Repeat each test N times (flakiness detection) |
| `forbidOnly`           | `bool`    | false   | Fail the run if any `#[ferritest(only)]` is present |
| `failFast`             | `bool`    | false   | Stop after first failure |
| `maxFailures`          | `u32`     | 0       | Stop after N failures (0 = no limit) |
| `globalTimeout`        | `u64`     | 0       | Whole-run timeout (ms; 0 = no limit) |
| `screenshotOnFailure`  | `bool`    | true    | Capture screenshot on test failure |
| `video`                | object    | `{ mode = "off" }` | `mode`: `off` / `on` / `retain-on-failure` |
| `trace`                | enum      | `off`   | `off` / `on` / `retain-on-failure` / `on-first-retry` |
| `outputDir`            | path      | `test-results` | Test output root |
| `snapshotDir`          | path?     | none    | Snapshot baseline directory (`{snapshotDir}` in a template) |
| `snapshotPathTemplate` | string?   | legacy  | Where snapshots live — see [Snapshot paths](#snapshot-paths) |
| `updateSnapshots`      | enum      | `missing` | `all` / `changed` / `missing` / `none` |
| `storageState`         | path?     | none    | Saved auth state JSON |
| `baseUrl`              | string?   | none    | Base URL for relative `page.goto`s |
| `strict`               | bool      | true    | (BDD) undefined / pending steps fail; `false` (or `--no-strict`) reports them without failing |
| `order`                | enum      | `defined` | `defined` / `random[:SEED]` (BDD) |
| `language`             | string?   | none    | Default Gherkin keyword language |
| `worldParameters`      | JSON      | `{}`    | Passed to JS `this.parameters` (BDD) |
| `features`             | `Vec<String>` | `[]` | Feature file globs (BDD) |
| `steps`                | `Vec<String>` | `[]` | JS / TS step file globs (BDD) |
| `tsconfig`             | path?     | none    | Pins the tsconfig whose `paths` / `baseUrl` govern bundling for the whole graph. Unset leaves rolldown's per-module upward discovery of `tsconfig.json`, so this is what selects a config discovery would not find (`tsconfig.test.json`). A path naming no file fails the bundle. |

## Snapshot paths

`snapshotPathTemplate` decides where a baseline lives; the default is
Playwright's legacy layout, a `-snapshots` directory beside the spec:

```
{snapshotDir}/{testFileDir}/{testFileName}-snapshots/{arg}{-projectName}{-snapshotSuffix}{ext}
```

`expect.toHaveScreenshot.pathTemplate` and
`expect.toMatchAriaSnapshot.pathTemplate` override it for those two
matchers. A relative template resolves against the directory of the
config file that declared it.

| Token | Value |
|---|---|
| `{testDir}` | the project's `testDir` |
| `{snapshotDir}` | the project's `snapshotDir` |
| `{snapshotSuffix}` | `testInfo.snapshotSuffix` |
| `{testFileDir}` | the spec's directory, relative to `testDir` |
| `{testFilePath}` | the spec's path relative to `testDir`, with extension |
| `{testFileName}` | the spec's file name, with extension |
| `{testFileBaseName}` | the spec's file name without extension |
| `{testName}` | the test's title path, sanitized |
| `{projectName}` | the project name, sanitized |
| `{platform}` | `darwin` / `linux` / `win32` |
| `{arg}` | the name passed to the matcher, without extension |
| `{ext}` | the extension, including the dot |

A token may carry a separator INSIDE its braces — `{-projectName}` emits
`-chromium` for a named project and nothing at all for an unnamed one.
That is why `{arg}{-projectName}{ext}` gives `button.png` in a
single-project run and `button-chromium.png` in a matrix.

`testInfo.snapshotPath(...name, { kind })` answers with the same path
the matcher of that `kind` (`'snapshot'`, `'screenshot'`, `'aria'`)
would write, without consuming a snapshot index.

## Expect block

`[test.expect]` carries the defaults every assertion starts from, and
each matcher's own sub-table carries its options. A per-call option bag
layers on top: a key the call names wins, a key it leaves out comes from
here.

```toml
[test.expect]
timeout = 5000

[test.expect.toHaveScreenshot]
threshold         = 0.2
maxDiffPixels     = 100
maxDiffPixelRatio = 0.01
animations        = "disabled"
caret             = "hide"
scale             = "css"
stylePath         = ["screenshot.css"]   # relative to this config file
pathTemplate      = "{testDir}/__screenshots__/{testFilePath}/{arg}{ext}"
timeout           = 10000

[test.expect.toMatchSnapshot]
threshold         = 0.2
maxDiffPixels     = 100
maxDiffPixelRatio = 0.01

[test.expect.toMatchAriaSnapshot]
pathTemplate = "{testDir}/__aria__/{testFilePath}/{arg}{ext}"
children     = "equal"          # contain | equal | deep-equal

[test.expect.toPass]
timeout   = 10000
intervals = [100, 250, 500, 1000]
```

A project may carry its own `expect` block, and it **replaces** the
config's whole object rather than merging into it — Playwright's
`takeFirst(projectConfig.expect, config.expect, {})`. A project setting
only `expect.timeout` therefore starts from the defaults for everything
else, including the screenshot budget:

```toml
[test.expect]
timeout = 2500
[test.expect.toHaveScreenshot]
maxDiffPixelRatio = 0.4

[[test.projects]]
name = "fast"
[test.projects.expect]
timeout = 600                    # and NO inherited maxDiffPixelRatio
```

## Bundler

`[bundler]` governs how JS / TS imports are resolved and transformed for
every bundle ferridriver produces — spec files, BDD step files,
extensions and `ferridriver run` scripts.

```toml
[bundler]
conditions = ["browser"]
mainFields = ["module", "main"]
aliasFields = [["browser"]]

[bundler.alias]
"@wdio/utils" = "./shims/wdio-utils.ts"

[bundler.virtualModules]
"acme:env" = "export const env = 'staging';"
```

| Field            | Type          | Default | Notes |
|------------------|---------------|---------|-------|
| `alias`          | map           | `{}`    | Bare import specifier -> shim file (`.js`/`.ts`), bundled and transpiled like any other source |
| `virtualModules` | map           | `{}`    | Import specifier -> inline ES-module source; never touches the filesystem |
| `conditions`     | `Vec<String>` | `[]`    | Extra `exports` / `imports` condition names, APPENDED to the base set (`default`, plus `import` or `require` per import kind). A package's `browser` branch is taken only when `"browser"` is listed |
| `mainFields`     | `Vec<String>` | `["module", "main"]` | `package.json` fields consulted when no `exports` entry matches. An empty list disables main-field resolution, which leaves such a package unresolvable |
| `aliasFields`    | `Vec<Vec<String>>` | `[]` | `package.json` fields holding a legacy path-remapping OBJECT (`{"./node.js": "./browser.js"}`) — a different mechanism from a `browser` condition inside `exports`. `[["browser"]]` selects the top-level `browser` field |

Every one of these participates in the bytecode cache key, and the
governing tsconfig is tracked as a bundle input, so changing a condition
or a `paths` mapping rebuilds instead of serving stale bytecode.

Plus per-project `ProjectConfig` and per-context `ContextConfig`
(viewport, locale, timezone, geolocation, permissions, etc.). See the
rustdoc for `ferridriver-config` for the full struct.
