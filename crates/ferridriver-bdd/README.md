# ferridriver-bdd

Cucumber/Gherkin BDD framework for ferridriver. Translates `.feature` files into parallel test execution via the core `TestRunner` -- same worker pool, retries, reporters, and fixtures as E2E tests.

## Quick Start (Rust)

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
async fn check_text(world: &mut BrowserWorld, text: String) {
  let content = world.page().text_content().await.unwrap();
  if !content.contains(&text) {
    return Err(step_err!("expected text '{}' not found", text));
  }
  Ok(())
}
```

```gherkin
# features/login.feature
Feature: Login

  @smoke
  Scenario: Successful login
    Given I navigate to "https://app.example.com/login"
    When I fill "#email" with "user@test.com"
    And I fill "#password" with "secret"
    And I click "button[type=submit]"
    Then the page should contain text "Dashboard"
```

```rust
// tests/bdd.rs
ferridriver_bdd::bdd_main!();
```

```sh
cargo test --test bdd
# or
ferridriver bdd -- features/
```

## Quick Start (TypeScript)

```typescript
// steps/login.ts
import { Given, When, Then } from '@ferridriver/test/bdd';

Given('I navigate to {string}', async (page, url) => {
  await page.goto(url);
});

When('I click {string}', async (page, selector) => {
  await page.locator(selector).click();
});
```

```sh
ferridriver-test --bdd --features features/ --steps steps/
```

## Architecture

```
.feature files
    |
FeatureSet::discover() + parse()     -- glob + gherkin crate
    |
expand_feature()                      -- Background, Scenario Outline, Rules
    |
filter (tags, grep)                   -- tag expression parser
    |
translate_features()                  -- Feature -> TestSuite, Scenario -> TestCase
    |
TestRunner.run(TestPlan)              -- core ferridriver-test runner
    |
Workers execute tests:
  - create BrowserWorld (Page + Context + variables + state)
  - run BeforeScenario hooks
  - for each step:
      interpolate variables
      registry.find_match(text) -> StepDef + params
      handler(world, params, table, docstring)
      emit StepStarted/StepFinished events
  - run AfterScenario hooks
  - screenshot on failure
```

The BDD crate is a thin translation layer. It does not duplicate any execution logic -- the core TestRunner handles parallelism, retries, fixtures, and reporting.

## Modules

### expression.rs -- Cucumber Expression Compiler

Compiles cucumber expressions to regex with typed parameter extraction.

```
"I have {int} item(s) in my {string}" -> regex with ParamType::Int, ParamType::String
```

Parameter types: `{string}`, `{int}`, `{float}`, `{word}`, `{}` (anonymous).

String parameters use named capture groups (`__N_0` for double-quoted, `__N_1` for single-quoted), consuming 2 positional indices per string param.

### feature.rs -- Feature Discovery and Parsing

- `FeatureSet::discover(patterns, ignore)`: Glob-based `.feature` file discovery
- `FeatureSet::parse()`: Gherkin parsing via the `gherkin` crate (v0.15)
- Output: `ParsedFeature { path, gherkin::Feature }`

### scenario.rs -- Scenario Expansion

`expand_feature()` transforms parsed Gherkin into flat `ScenarioExecution` structs:

- Background steps prepended to every scenario
- Scenario Outlines expanded: each Examples row produces a concrete scenario with `<placeholder>` substitution in steps, tables, and docstrings
- Tags merged: feature tags + scenario tags + example tags
- Rules handled: nested Background + scenarios within Rule blocks

### filter.rs -- Tag Expression Parser

Recursive descent parser for boolean tag expressions:

```
@smoke                     -- single tag
not @wip                   -- negation
@smoke and not @wip        -- conjunction
@fast or @critical         -- disjunction
(@smoke or @regression) and not @wip  -- grouping
```

Also: `filter_by_grep(scenarios, pattern, invert)` for regex name filtering.

### registry.rs -- Step and Hook Registry

Central registry built from `inventory::iter` (proc macro submissions).

- `find_match(text)`: O(n) scan of all step definitions
  - 1 match: returns `StepMatch { def, params }`
  - 0 matches: `MatchError::Undefined` with word-overlap suggestions
  - 2+ matches: `MatchError::Ambiguous`
- `register_step()`: Runtime registration for NAPI/external steps
- `reference()`: Generate markdown step documentation grouped by kind

Matching is keyword-agnostic: a `Given` definition matches `When`/`Then`/`And`/`But` too (per Cucumber spec).

### translate.rs -- Gherkin to TestPlan

The bridge between BDD and the core test runner:

- Each Feature becomes a `TestSuite`
- Each Scenario becomes a `TestCase` requesting fixtures: browser, context, page, test_info
- `@serial` tag on any scenario forces the entire feature to run serially
- Tags mapped to annotations: `@skip`/`@wip` -> Skip, `@slow` -> Slow

**Step execution inside TestCase::test_fn:**

1. Get Page, Context, TestInfo from FixturePool
2. Construct `BrowserWorld` with Page + Context
3. Run BeforeScenario hooks
4. For each step:
   - Interpolate `$variables`
   - `registry.find_match(text)` -> StepDef + params
   - `test_info.begin_step()` with metadata `{bdd_keyword, bdd_text, bdd_line}`
   - `tokio::time::timeout(step_timeout, handler(world, params, table, docstring))`
   - `handle.end(error)`
   - On failure: skip remaining steps
5. Run AfterScenario hooks
6. Screenshot on failure if configured

### world.rs -- BrowserWorld

Shared state passed to every step handler within a scenario.

```rust
world.page()              // &Page
world.context()           // &ContextRef (cookies, permissions)
world.set_var("key", "value")
world.var("key")          // Option<&String>
world.interpolate("$key") // variable substitution
world.set_state(my_data)  // type-safe state store (TypeId-based)
world.get_state::<T>()    // Option<&T>
```

### step.rs -- Step Definition Types

```rust
StepKind: Given | When | Then | Step (keyword-agnostic)

StepParam: String(String) | Int(i64) | Float(f64) | Word(String)

StepHandler: Arc<dyn Fn(&BrowserWorld, Vec<StepParam>, Option<&DataTable>, Option<&str>) -> BoxFuture<Result<()>>>

StepDef { kind, expression, regex, handler, source_file, source_line }
```

### hook.rs -- Lifecycle Hooks

Hook points: `BeforeAll`, `AfterAll`, `BeforeFeature`, `AfterFeature`, `BeforeScenario`, `AfterScenario`, `BeforeStep`, `AfterStep`.

Hooks have optional tag filters and ordering:

```rust
#[before(scenario, tags = "@auth", order = 10)]
async fn setup_auth(world: &mut BrowserWorld) {
  // runs before scenarios tagged @auth, ordered by priority
}
```

## Proc Macros (ferridriver-bdd-macros)

### Step Macros

```rust
#[given("pattern")]   // Given steps
#[when("pattern")]    // When steps
#[then("pattern")]    // Then steps
#[step("pattern")]    // Any keyword
```

Function signature auto-detection:
- First param `world: &mut BrowserWorld` (required)
- Subsequent params extracted from cucumber expression by type:
  - `String` -> `{string}` capture
  - `i64` -> `{int}` capture
  - `f64` -> `{float}` capture
- Optional `table: &DataTable` or `data_table: &DataTable` for Gherkin tables
- Optional `docstring: &str` or `doc_string: &str` for docstrings

Registration via `inventory::submit!(StepRegistration { ... })`.

### Hook Macros

```rust
#[before(scenario)]                          // before each scenario
#[before(scenario, tags = "@smoke")]         // filtered by tag
#[before(scenario, order = 10)]             // execution order
#[after(scenario)]                           // after each scenario
#[before(all)]                               // before all scenarios
#[after(all)]                                // after all scenarios
```

## Built-in Steps (109)

### Navigation (6)
- I navigate to {string}
- I go back / I go forward / I reload the page
- the URL should contain {string} / the URL should be {string}

### Interaction (14)
- I click/double-click/right-click {string}
- I fill {string} with {string}
- I clear {string} / I type {string} into {string}
- I hover over {string} / I focus {string}
- I drag {string} to {string}
- I scroll to {string} / I scroll down/up {int} pixels
- I select {string} in {string} / I check/uncheck {string}

### Assertion (20)
- {string} should be visible/hidden/enabled/disabled/checked/unchecked
- {string} should have text/value {string}
- {string} should contain text {string}
- {string} should have attribute {string} with value {string}
- {string} should have class/role {string}
- the page should contain text {string}
- there should be {int} {string} elements

### Keyboard (4)
- I press {string} / I press {string} on {string}
- I press {string} {int} times
- I type slowly {string} into {string}

### Mouse (2)
- I move mouse to coordinates {int},{int}
- I scroll within {string} by {int},{int}

### Wait (4)
- I wait for {string} to appear/disappear
- I wait for navigation
- I wait {int} seconds

### Screenshot (2)
- I take a screenshot / I take a screenshot named {string}

### Variable (6)
- I set variable {string} to {string}
- I store the text of {string} as {string}
- I store the attribute {string} of {string} as {string}
- I store the property {string} of {string} as {string}
- I store the count of {string} as {string}

### Storage (8)
- I set localStorage/sessionStorage {string} to {string}
- I get localStorage/sessionStorage {string}
- I clear localStorage/sessionStorage
- I remove localStorage/sessionStorage item {string}

### Cookie (3)
- I add a cookie {string} with value {string}
- I delete cookie {string} / I clear all cookies

### JavaScript (3)
- I execute {string} / I evaluate {string}
- I inject script {string}

### Dialog (3)
- I accept/dismiss the dialog
- the dialog message should be {string}

### Frame (3)
- I switch to frame {string} / I switch to frame {int}
- I switch to the main frame

### Window (5)
- I set window size to {int}x{int}
- I maximize/minimize the window
- I switch to tab {int} / I switch to window {string}

### File (2)
- I upload {string} to {string}
- I should have downloaded {string}

## Reporters

| Reporter | Format | Constructor |
|----------|--------|-------------|
| BddTerminalReporter | Gherkin-formatted stdout | Feature > Scenario > Step hierarchy |
| BddJsonReporter | JSON file | Full results with step details |
| BddJunitReporter | JUnit XML | CI-compatible |
| CucumberJsonReporter | Cucumber JSON | Compatible with Cucumber reporting tools |

All implement `ferridriver_test::reporter::Reporter` and receive the same event stream as E2E reporters. BDD step events carry metadata (`bdd_keyword`, `bdd_text`, `bdd_line`) for Gherkin-aware rendering.

## CLI

```sh
ferridriver bdd [OPTIONS] -- [FEATURES...]

Options:
  -t, --tags <EXPR>        Tag filter expression (@smoke and not @wip)
  -j, --workers <N>        Parallel workers (0 = auto)
      --retries <N>        Retry failed scenarios
      --reporter <NAME>    Reporter (terminal, json, junit, cucumber-json)
  -g, --grep <PATTERN>     Filter scenarios by name
      --grep-invert <PAT>  Exclude matching scenarios
      --dry-run            Validate steps without executing
      --list               List scenarios without running
      --headed             Show browser window
      --fail-fast          Stop after first failure
      --step-timeout <MS>  Per-step timeout in milliseconds
      --shard <CUR/TOTAL>  Shard for distributed runs
  -c, --config <PATH>      Config file path
```

Environment variables:
- `FERRIDRIVER_FEATURES`: Comma-separated glob patterns (default: `features/**/*.feature`)
- `FERRIDRIVER_TAGS`: Tag filter expression

## Design Decisions

1. **Translation layer, not a runner.** The BDD crate translates Features to `TestPlan` and delegates everything to the core `TestRunner`. No duplicate execution logic, worker management, or reporter infrastructure.

2. **Keyword-agnostic matching.** Step definitions match by pattern only, not by keyword (Given/When/Then). A `#[given]` step can match a `When` or `And` line. This follows the Cucumber specification.

3. **Inventory-based registration.** Steps and hooks auto-register via proc macros + `inventory` crate. Binary crates just need `bdd_main!()` -- no manual registry setup.

4. **BrowserWorld as step context.** Each scenario gets a fresh `BrowserWorld` with Page, Context, variables, and type-safe state. Steps share state within a scenario but are isolated across scenarios.

5. **Domain metadata in generic events.** Step events carry BDD-specific info (keyword, line number) in `metadata: Option<serde_json::Value>` rather than domain-specific enum variants. Keeps the core test engine generic.

6. **Cucumber expressions over raw regex.** Type-safe parameter extraction (`{string}`, `{int}`, `{float}`) with auto-generated regex, instead of forcing users to write regex patterns.
