# BDD

Cucumber / Gherkin framework for ferridriver. Translates `.feature` files into parallel test execution via the core `TestRunner` — same worker pool, retries, reporters, and fixtures as E2E tests.

**144 built-in steps** backed by the Page / Locator API (not raw JS `evaluate`). All selectors support Playwright engine syntax (`role=`, `text=`, `label=`).

## Rust

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
async fn contains(world: &mut BrowserWorld, text: String) -> Result<(), StepError> {
  let body = world.page().locator("body")
    .text_content().await
    .map_err(|e| step_err!("{e}"))?
    .unwrap_or_default();
  if !body.contains(&text) {
    return Err(step_err!("text '{}' not found on page", text));
  }
  Ok(())
}
```

```rust
// tests/bdd.rs
ferridriver_bdd::bdd_main!();
```

```bash
cargo test --test bdd
```

## TypeScript

```ts
// steps/login.ts
import { Given, When, Then } from '@ferridriver/test/bdd';

Given('I navigate to {string}', async (page, url) => {
  await page.goto(url);
});

When('I click {string}', async (page, selector) => {
  await page.locator(selector).click();
});
```

```bash
npx @ferridriver/test test tests/features/ --steps 'steps/**/*.ts'
```

## Learn more

- [Built-in steps](/bdd/steps) — all 144 steps grouped by category
- [Hooks](/bdd/hooks) — `before` / `after`, scoped by scenario / feature / step
- [Running](/bdd/running) — CLI, tag filters, reporters
