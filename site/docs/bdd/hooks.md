# Hooks

Lifecycle hooks attach to specific points in a BDD run. Available
points: `BeforeAll`, `AfterAll`, `BeforeFeature`, `AfterFeature`,
`BeforeScenario`, `AfterScenario`, `BeforeStep`, `AfterStep`.

Hooks support optional tag filters and an ordering value. `After*` hooks
run even when earlier hooks or steps failed — they are the cleanup
guarantee.

## Rust

```rust
use ferridriver_bdd::prelude::*;

#[before(scenario)]
async fn fresh_world(world: &mut BrowserWorld) {
    world.set_var("started_at", &chrono::Utc::now().to_rfc3339());
}

#[before(scenario, tags = "@auth", order = 10)]
async fn login(world: &mut BrowserWorld) {
    world.page().goto("https://app.example.com/login", None).await.unwrap();
    // ...
}

#[after(scenario)]
async fn screenshot_on_fail(world: &mut BrowserWorld) {
    // Runs on every scenario, even when earlier hooks or steps failed.
}

#[before(all)]
async fn start_database() { /* once */ }
```

## JavaScript / TypeScript

```ts
Before(async function () {
  await this.context.clearCookies();
});

Before({ tags: '@auth' }, async function () {
  await this.page.goto('https://app.example.com/login');
});

After(async function (result) {
  if (result?.result?.status === 'FAILED') {
    this.attach(await this.page.screenshot(), 'image/png');
  }
});

BeforeAll(async () => { /* once per run */ });
AfterAll(async () => { /* once per run */ });
```

## Tag expressions

`tags` accepts the same boolean expression grammar as the `--tags` CLI
flag:

```
@smoke
not @wip
@smoke and not @wip
@fast or @critical
(@smoke or @regression) and not @wip
```

## Ordering

`order` is an `i32` — lower runs first. `Before` hooks run in ascending
order; `After` hooks run in descending order (cleanup mirrors setup).

## Hook failure semantics

- A failing `Before` hook: the scenario is marked failed; steps are not
  executed; `After` hooks still run.
- A failing `After` hook: the scenario is marked failed even if the
  steps passed; subsequent `After` hooks still run.
- `BeforeAll` / `AfterAll` failures do not cascade to other suites.
