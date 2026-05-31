//! BDD test harness example.
//!
//! This demonstrates how Rust developers add custom BDD steps and run .feature files
//! using ferridriver's full test infrastructure (worker pool, parallel execution,
//! retries, reporters).
//!
//! Run with:
//!   cargo test -p bdd-example                      # run all scenarios (headed)
//!   cargo test -p bdd-example -- --headless        # hide the browser
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
// alongside the 145 built-in steps.

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
  let locator = world.page().locator("h1", None);
  ferridriver_test::expect::expect(&locator)
    .to_have_text("Example Domain")
    .await
    .map_err(|e| StepError::from(e.message))?;
}

#[when("I store the page info")]
async fn store_info(world: &mut BrowserWorld) {
  let title = world.page().title().await.map_err(|e| step_err!("{e}"))?;
  world.set_var("page_title", title);

  let url = world.page().url();
  world.set_var("page_url", url);
}

// ── Rust value matchers (Jest-compatible) ───────────────────────────────
// Demonstrate `expect_value` + asymmetric matchers from a Rust BDD step
// body. These do not touch the browser — they assert against synthetic
// JSON to prove the new matcher API is callable from the test-runner
// surface that ferridriver-bdd / ferridriver-test expose.

#[given("a synthetic JSON document")]
async fn store_synthetic_json(world: &mut BrowserWorld) {
  let doc = serde_json::json!({
    "id": 42,
    "name": "Ada",
    "tags": ["admin", "user"],
    "address": { "city": "London", "zip": "EC1A" },
  });
  world.set_var("doc", doc.to_string());
}

#[then("the document equals the expected shape")]
async fn doc_equals_expected(world: &mut BrowserWorld) {
  let raw = world.var("doc").ok_or_else(|| step_err!("missing 'doc'"))?;
  let actual: serde_json::Value = serde_json::from_str(raw).map_err(|e| step_err!("{e}"))?;
  ferridriver_test::expect::expect_value(actual)
    .to_equal(&serde_json::json!({
      "id": 42,
      "name": "Ada",
      "tags": ["admin", "user"],
      "address": { "city": "London", "zip": "EC1A" },
    }))
    .map_err(|e| StepError::from(e.message))?;
}

#[then("the document matches an asymmetric expected shape")]
async fn doc_matches_asymmetric(world: &mut BrowserWorld) {
  let raw = world.var("doc").ok_or_else(|| step_err!("missing 'doc'"))?;
  let actual: serde_json::Value = serde_json::from_str(raw).map_err(|e| step_err!("{e}"))?;
  // `expect.any(Number)` + `expect.objectContaining({...})` +
  // `expect.arrayContaining([...])` encoded as the wire-form tagged
  // objects that the asymmetric decoder accepts. Rust callers
  // typically build them through helper constructors; here we use
  // the raw tagged form to keep the example self-contained.
  let expected = serde_json::json!({
    "id": { "@@asym": "any", "name": "Number" },
    "name": { "@@asym": "stringContaining", "substring": "Ad" },
    "tags": { "@@asym": "arrayContaining", "items": ["admin"] },
    "address": { "@@asym": "objectContaining", "subset": { "city": "London" } },
  });
  ferridriver_test::expect::expect_value(actual)
    .to_equal(&expected)
    .map_err(|e| StepError::from(e.message))?;
}

#[then("a closure that throws is caught by toThrow")]
async fn closure_throws_caught(_world: &mut BrowserWorld) {
  // Rust analogue: build a `ThrownError` manually (the JS-side
  // toThrow invokes the function for the user; the Rust-side
  // `expect_fn` consumes the already-captured outcome). This proves
  // the matcher logic itself is reachable from Rust.
  use ferridriver_test::expect::{ThrowMatcher, ThrownError, expect_fn};
  let caught = Some(ThrownError {
    message: "boom: out of range".into(),
    class_name: Some("RangeError".into()),
  });
  expect_fn(caught.clone())
    .to_throw(Some(&ThrowMatcher::Substring("out of range".into())))
    .map_err(|e| StepError::from(e.message))?;
  expect_fn(caught)
    .to_throw(Some(&ThrowMatcher::ClassName("RangeError".into())))
    .map_err(|e| StepError::from(e.message))?;
  expect_fn(None)
    .not()
    .to_throw(None)
    .map_err(|e| StepError::from(e.message))?;
}

// ── Entry point ──
// Uses the same TestRunner as E2E and component tests:
// parallel workers, retry/flaky detection, reporters, sharding.
ferridriver_bdd::bdd_main!();
