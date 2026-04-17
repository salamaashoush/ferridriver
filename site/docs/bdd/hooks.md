# Hooks

Lifecycle hooks attach to specific points in a BDD run. Available points: `BeforeAll`, `AfterAll`, `BeforeFeature`, `AfterFeature`, `BeforeScenario`, `AfterScenario`, `BeforeStep`, `AfterStep`.

Hooks support optional tag filters and an ordering value.

```rust
use ferridriver_bdd::prelude::*;

#[before(scenario)]
async fn fresh_world(world: &mut BrowserWorld) {
    world.set_var("started_at", &chrono::Utc::now().to_rfc3339());
}

#[before(scenario, tags = "@auth", order = 10)]
async fn login(world: &mut BrowserWorld) {
    world.page().goto("https://app.example.com/login", None).await.unwrap();
    // ... sign in
}

#[after(scenario)]
async fn screenshot_on_fail(world: &mut BrowserWorld) {
    // Runs on every scenario, even when earlier hooks or steps failed.
}

#[before(all)]
async fn start_database() { /* once */ }
```

## Tag expressions

`tags` accepts the same boolean expression grammar as the `--tags` CLI flag:

```
@smoke
not @wip
@smoke and not @wip
@fast or @critical
(@smoke or @regression) and not @wip
```
