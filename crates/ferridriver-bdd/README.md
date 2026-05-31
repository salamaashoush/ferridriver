# ferridriver-bdd

[![crates.io](https://img.shields.io/crates/v/ferridriver-bdd.svg?logo=rust&color=c97b4a)](https://crates.io/crates/ferridriver-bdd)
[![docs.rs](https://img.shields.io/docsrs/ferridriver-bdd?logo=docs.rs&color=c97b4a)](https://docs.rs/ferridriver-bdd)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-c97b4a.svg)](https://github.com/salamaashoush/ferridriver)

Cucumber / Gherkin BDD framework for ferridriver. Translates `.feature`
files into the same `TestPlan` the `#[ferritest]` runner executes — same
parallel workers, retries, fixtures, reporters. Step bodies are written in
Rust or in JavaScript / TypeScript; both share one registry.

## Rust step bodies

```rust
use ferridriver_bdd::prelude::*;

#[given("I navigate to {string}")]
async fn navigate(world: &mut BrowserWorld, url: String) {
    world.page().goto(&url, None).await.unwrap();
}

#[when("I click {string}")]
async fn click(world: &mut BrowserWorld, selector: String) {
    world.page().locator(&selector).click().await.unwrap();
}

#[then("the page should contain text {string}")]
async fn check_text(
    world: &mut BrowserWorld,
    text: String,
) -> Result<(), StepError> {
    let body = world
        .page()
        .locator("body")
        .text_content()
        .await
        .map_err(|e| step_err!("{e}"))?
        .unwrap_or_default();
    if !body.contains(&text) {
        return Err(step_err!("text {text:?} not found"));
    }
    Ok(())
}
```

Wire a binary entry point:

```rust
// tests/bdd.rs
ferridriver_bdd::bdd_main!();
```

```toml
[[test]]
name = "bdd"
path = "tests/bdd.rs"
harness = false

[dev-dependencies]
ferridriver-bdd = "0.3"
ferridriver-test = "0.3"
```

```bash
cargo test --test bdd
# or
ferridriver bdd tests/features/
```

## JavaScript / TypeScript step bodies

```ts
// steps/login.ts
Given('I navigate to {string}', async function (url: string) {
  await this.page.goto(url);
});

When('I click {string}', async function (selector: string) {
  await this.page.locator(selector).click();
});

Then('the URL contains {string}', async function (fragment: string) {
  if (!this.page.url().includes(fragment)) throw new Error('mismatch');
});
```

```bash
ferridriver bdd --steps 'steps/**/*.{js,ts}' tests/features/
```

`Given` / `When` / `Then` / `defineStep` (with `And` / `But` aliases) /
`Before` / `After` / `BeforeAll` / `AfterAll` / `BeforeStep` / `AfterStep` /
`defineParameterType` / `setWorldConstructor` / `setDefaultTimeout` /
`setDefinitionFunctionWrapper` are globals. `this` is the World — `page`,
`context`, `browser`, `request`, `parameters`, `attach`, `log`, `skip`.

Files are bundled with rolldown (TypeScript + `node_modules` + tree-shake),
compiled to QuickJS bytecode once at startup, and `Module::load`ed per
worker. The bytecode cache is content-hashed and in-memory. **No Node, no
Bun in the run path.**

## Macros

```
#[given(EXPR)]           #[when(EXPR)]              #[then(EXPR)]
#[step(EXPR)]            #[given(regex = PATTERN)]  // …same for when/then
#[before(scenario)]      #[after(scenario)]
#[before(scenario, tags = "@auth", order = 10)]
#[before(all)]           #[after(all)]
#[before(feature)]       #[before(step)]            // and matching afters
#[param_type(name = "color", regex = "red|green|blue")]
```

Parameter extraction is type-directed:
- `String` → `{string}`
- `i64` → `{int}`
- `f64` → `{float}`
- Custom `{name}` → registered regex (extract as `String`)

`table: &DataTable` / `data_table: &DataTable` and `docstring: &str` /
`doc_string: &str` are recognised by name and pulled in after positional
parameters.

## Hooks

Eight hook points, both Rust and JS:

`BeforeAll`, `AfterAll`, `BeforeFeature`, `AfterFeature`, `BeforeScenario`,
`AfterScenario`, `BeforeStep`, `AfterStep`. Tag filters and explicit order
are supported on every point. `After*` hooks run even on failure (cleanup
guarantee).

## Gherkin coverage

Full Gherkin 6+: Features, Rules, Backgrounds, Scenarios, Scenario
Outlines (with named Examples blocks), tags (boolean expressions: `and`,
`or`, `not`, parens), data tables (with `.hashes()`, `.rows_hash()`,
`.transpose()`, `.as_type::<T>()`), doc strings (with media-type hints
like `"""json`), the asterisk (`*`) keyword, and i18n keywords via
`--language` or `# language: xx`.

Scenario Outline placeholders (`<key>`) substitute into step text, table
cells, and doc strings recursively. At runtime, `$key` interpolation
reaches into `world.vars()` / `world.set_var(name, value)`.

## Built-in steps (145)

Source: `crates/ferridriver-bdd/src/steps/`. Counts reflect actual
`#[given]` / `#[when]` / `#[then]` / `#[step]` registrations.

| Module       | Count | Coverage |
|--------------|-------|----------|
| `assertion`  | 34    | Text, visibility, value, attribute, class, state, count, role, ARIA |
| `interaction`| 20    | Click / double-click / right-click, fill, clear, type, hover, focus, blur, drag, scroll, select, check, uncheck |
| `network`    | 14    | Route, fulfill, continue, abort, request / response waits, HAR |
| `api`        | 11    | API request context: GET/POST/PUT/DELETE/PATCH, headers, body, status / JSON assertions |
| `storage`    | 8     | localStorage / sessionStorage get / set / clear / remove |
| `wait`       | 7     | Wait for selector / text / navigation / seconds / load state |
| `navigation` | 6     | Navigate, back, forward, reload, URL assertions |
| `frame`      | 6     | Switch frames by name / index, main frame, frame queries |
| `dialog`     | 5     | Accept / dismiss, prompt text, assert message |
| `emulation`  | 6     | Viewport, user agent, geolocation, color scheme, timezone, locale |
| `mouse`      | 5     | Move to coordinates, scroll by delta, wheel, button holds |
| `window`     | 5     | Window size, maximize / minimize, tab / window switching |
| `keyboard`   | 4     | Press key, press on selector, repeat N times, type slowly |
| `javascript` | 3     | Execute, evaluate, inject script |
| `cookie`     | 3     | Add, delete, clear all |
| `screenshot` | 3     | Full page, named file, element-scoped |
| `variable`   | 3     | Set, store text / attribute / property / count of selector |
| `file`       | 2     | Upload to input, assert download |

Call `StepRegistry::reference()` from a `bdd_main!()` binary for the live
expression list with parameter types.

## Reporters

Same reporter family as `ferridriver-test` plus BDD-specific renderers:

`terminal` (Feature → Scenario → Step hierarchy with colours), `json`,
`junit`, `html`, `cucumber-json`, `messages` / `ndjson` (Cucumber Messages
NDJSON), `usage`, `rerun`, `progress`, `dot`.

## Public API (programmatic use)

Bypass the CLI / macros and drive the executor directly when embedding:

```rust
use ferridriver_bdd::{registry::StepRegistry, executor::ScenarioExecutor};
use std::sync::Arc;
use std::time::Duration;

let registry = StepRegistry::build();
let executor = ScenarioExecutor::new(
    Arc::new(registry),
    Duration::from_millis(5000),
    /* strict */ false,
    /* screenshot_on_failure */ true,
);
let result = executor
    .run_scenario_observed(&mut world, &scenario, &observer)
    .await;
```

`StepRegistry::register()` / `register_regex()` accept handler closures —
useful when registering steps from a host other than the macros (an MCP
plugin, an external test driver).

## License

MIT OR Apache-2.0
