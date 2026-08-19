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

`[test.browser.use]` is the Playwright `use` block: context options
(`locale`, `colorScheme`, `testIdAttribute`, …) plus any key of your
own. A key no built-in option claims is the value of a fixture
registered with `{ option: true }`.

```toml
[test.browser.use]
locale  = "de-DE"
profile = "guest"          # -> the `profile` option fixture

[[test.projects]]
name = "admin"
[test.projects.browser.use]
profile = "admin"          # -> only this project's tests
```

```ts
const test = base.extend<{ profile: string }>({
  profile: ['guest', { option: true }],
});

test('reads it', async ({ profile }) => { /* 'guest' or 'admin' */ });
```

Precedence, innermost first: a `test.use({ … })` in the spec, then the
project's block, then the config's, then the fixture's own default. Each
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

`--config` also takes a module. Its default export is the `[test]`
section, which is the shape a `playwright.config.ts` already has, so an
existing Playwright config runs unmodified:

```ts
import { defineConfig } from '@ferridriver/test';

export default defineConfig({
  testDir: './specs',
  use: { baseURL: 'http://localhost:3000' },
  projects: [{ name: 'chromium' }, { name: 'firefox' }],
});
```

```bash
ferridriver test --config playwright.config.ts
```

The module is bundled and evaluated through the same rolldown → QuickJS
pipeline every spec takes, so it can import helpers and be written in
TypeScript with no build step. It layers where an explicitly named
document would: above every discovered file, below `FERRIDRIVER_*` and
the CLI flags.

`defineConfig(...configs)` folds layers, rightmost winning. Scalars are
replaced; `use`, `expect` and `build` merge one level deep; `webServer`
normalizes each side to a list and concatenates; `projects` merge by
`name`, each match taking the incoming project's `use` on top, with new
names appended.

Four settings cannot come from a module, and each is refused by name
rather than ignored: `extensions`, `bundler`, `scripting` and
`[test].moduleAliases`. Every one of them had to be read before the
module could be compiled at all, so a value there would arrive after the
decision it advises on. Put them in a `ferridriver.toml` layer.

## Priority

Lowest to highest:

1. Extension `defineDefaults` contributions
2. Config file defaults
3. `main!()` / `HarnessConfig` macro arguments (Rust)
4. A `--config <file.ts>` module
5. Environment variables — `FERRIDRIVER_BACKEND`, `FERRIDRIVER_WORKERS`,
   `FERRIDRIVER_TIMEOUT`, `FERRIDRIVER_RETRIES`, …
6. CLI flags — `--headless`, `--backend`, `--workers`, `--timeout`, …

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
