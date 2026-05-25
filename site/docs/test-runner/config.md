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

## Web server

```toml
[[test.webServer]]
command            = "npm run preview"
url                = "http://localhost:4173"
reuseExistingServer = true
timeout            = 60000
```

Multiple `[[test.webServer]]` blocks can run in parallel.

## Priority

Lowest to highest:

1. Config file defaults
2. `main!()` / `HarnessConfig` macro arguments (Rust)
3. Environment variables — `FERRIDRIVER_BACKEND`, `FERRIDRIVER_WORKERS`,
   `FERRIDRIVER_TIMEOUT`, `FERRIDRIVER_RETRIES`, …
4. CLI flags — `--headed`, `--backend`, `--workers`, `--timeout`, …

## Profiles

Named presets that merge into the base config via `--profile NAME`:

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
ferridriver bdd --profile ci tests/features/
```

## Full schema

The `TestConfig` Rust type is the canonical reference. Notable fields:

| Field                  | Type      | Default | Notes |
|------------------------|-----------|---------|-------|
| `testMatch`            | `Vec<String>` | `[]` | Glob patterns for test files (JS / TS path) |
| `timeout`              | `u64`     | 30000   | Per-test timeout (ms) |
| `expectTimeout`        | `u64`     | 5000    | Assertion polling timeout (ms) |
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
| `snapshotDir`          | path?     | none    | Snapshot baseline directory |
| `updateSnapshots`      | enum      | `none`  | `all` / `changed` / `missing` / `none` |
| `storageState`         | path?     | none    | Saved auth state JSON |
| `baseUrl`              | string?   | none    | Base URL for relative `page.goto`s |
| `strict`               | bool      | false   | (BDD) undefined / pending steps fail |
| `order`                | enum      | `defined` | `defined` / `random[:SEED]` (BDD) |
| `language`             | string?   | none    | Default Gherkin keyword language |
| `worldParameters`      | JSON      | `{}`    | Passed to JS `this.parameters` (BDD) |
| `features`             | `Vec<String>` | `[]` | Feature file globs (BDD) |
| `steps`                | `Vec<String>` | `[]` | JS / TS step file globs (BDD) |

Plus per-project `ProjectConfig` and per-context `ContextConfig`
(viewport, locale, timezone, geolocation, permissions, etc.). See the
rustdoc for `ferridriver-config` for the full struct.
