//! BDD test harness example.
//!
//! This demonstrates how Rust developers add custom BDD steps and run .feature files
//! using ferridriver's full test infrastructure (worker pool, parallel execution,
//! retries, reporters).
//!
//! Run with:
//!   cargo test -p bdd-example                     # run all scenarios
//!   cargo test -p bdd-example -- --headed          # show browser
//!   cargo test -p bdd-example -- --workers 2       # 2 parallel workers
//!   cargo test -p bdd-example -- --list            # list scenarios
//!   cargo test -p bdd-example -- --grep "TodoMVC"  # filter by name
//!
//! Environment variables:
//!   FERRIDRIVER_FEATURES="features/**/*.feature"   # feature file globs
//!   FERRIDRIVER_TAGS="@smoke and not @skip"         # tag filter

use ferridriver_bdd::prelude::*;

// ── Custom step definitions ──
// These are auto-registered via inventory and available in all .feature files
// alongside the 109 built-in steps.

#[given("I am on the example page")]
async fn navigate_example(world: &mut BrowserWorld) {
  world
    .page()
    .goto("https://example.com", None)
    .await
    .map_err(|e| step_err!("{e}"))?;
}

#[then("I should see the example heading")]
async fn check_heading(world: &mut BrowserWorld) {
  let locator = world.page().locator("h1");
  ferridriver_test::expect::expect(&locator)
    .to_have_text("Example Domain")
    .await
    .map_err(|e| StepError::from(e.message))?;
}

#[when("I store the page info")]
async fn store_info(world: &mut BrowserWorld) {
  let title = world
    .page()
    .title()
    .await
    .map_err(|e| step_err!("{e}"))?;
  world.set_var("page_title", title);

  let url = world
    .page()
    .url()
    .await
    .map_err(|e| step_err!("{e}"))?;
  world.set_var("page_url", url);
}

// ── Entry point ──
// Uses the same TestRunner as E2E and component tests:
// parallel workers, retry/flaky detection, reporters, sharding.
ferridriver_bdd::bdd_main!();
