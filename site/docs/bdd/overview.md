# BDD

Cucumber / Gherkin framework for ferridriver. Translates `.feature` files
into parallel test execution via the core `TestRunner` — same worker
pool, retries, reporters, and fixtures as Rust tests.

**145 built-in steps** backed by the Page / Locator API (not raw JS
`evaluate`). All selectors support the Playwright engine syntax
(`role=`, `text=`, `label=`, …).

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

#[then("the page body should contain text {string}")]
async fn contains(
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

```bash
cargo test --test bdd
# or via the CLI:
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
  if (!this.page.url().includes(fragment)) {
    throw new Error(`URL ${this.page.url()} does not contain ${fragment}`);
  }
});
```

```bash
ferridriver bdd --steps 'steps/**/*.{js,ts}' tests/features/
```

`Given` / `When` / `Then` / `defineStep` / `And` / `But` / `Before` /
`After` / `BeforeAll` / `AfterAll` / `BeforeStep` / `AfterStep` /
`defineParameterType` / `setWorldConstructor` / `setDefaultTimeout` /
`setDefinitionFunctionWrapper` are globals. `this` is the World,
carrying `page` / `context` / `request` / `browser` / `parameters` /
`attach` / `log` / `skip`.

`DataTable` exposes `raw`/`rows`/`hashes`/`rowsHash`/`transpose`.
Returning `'pending'` or `'skipped'` (or calling `this.skip()`) marks the
step as such.

### Fixtures in a scenario

`bindSteps(test)` binds the registrars to a `test.extend` chain, and a
step destructures what it needs from its first parameter:

```ts
import { mergeTests, test } from '@ferridriver/test';

const shop = test.extend<{ cart: Cart }>({
  cart: async ({ page }, use) => { await use(await Cart.open(page)); },
});

const { Given, Then } = bindSteps(shop);

Given('a cart with {int} items', async function ({ cart }, n: number) {
  await cart.fill(n);
});
```

The object a step receives is the scenario's World AND its fixture bag:
`page` / `context` / `request` / `browser` / `parameters` / `attach` /
`log` / `skip` are on it either way, and a `setWorldConstructor`
instance becomes its prototype. Fixtures resolve through the same graph
a Playwright spec resolves through — dependency order, `auto`,
`{ scope: 'worker' }` shared across the scenarios one worker runs, and
LIFO teardown after the last step. Steps bound to unrelated `test`
objects fail the scenario up front, pointing at `mergeTests(...)`.

Files are bundled with rolldown (TypeScript, imports, tree-shake),
compiled to QuickJS bytecode once, and `Module::load`ed per worker. The
bytecode cache is content-hashed, in-memory within a run and persisted
to a cross-process disk cache so an unchanged source tree skips both
rolldown and the QuickJS compile on the next start. **No Node, no Bun,
no `package.json`, no `node_modules`.**

## Hybrid (Rust + JS / TS)

The Rust step registry and the JS / TS registry merge — a single
feature can mix steps defined in both, and `Before` / `After` hooks from
either side run together.

## Gherkin coverage

Full Gherkin 6+: Features, Rules, Backgrounds, Scenarios, Scenario
Outlines (with named Examples blocks), tags (boolean expressions: `and`,
`or`, `not`, parens), data tables, doc strings (with media-type hints
like `"""json`), the asterisk (`*`) keyword, and i18n keywords via
`--language` or `# language: xx` (70+ languages).

## Learn more

- [Built-in steps](/bdd/steps) — all 145 steps grouped by category
- [Hooks](/bdd/hooks) — lifecycle points and tag filters
- [Running](/bdd/running) — CLI, reporters, profiles
