//! End-to-end test for the `#[fixture]` custom-fixture macro.
//!
//! Exercises the full path with no browser: macro expansion ->
//! `inventory` registration -> `collect_rust_fixtures` -> `FixturePool`
//! resolution, including one fixture depending on another via `ctx.get`,
//! and per-scope caching.

use std::sync::Arc;

use ferridriver_test::fixture::{FixturePool, FixtureScope};
use ferridriver_test::prelude::*;

/// A plain data fixture — no browser needed.
#[fixture(scope = "test")]
async fn seeded_users(_ctx: TestContext) -> ferridriver_test::Result<Vec<String>> {
  Ok(vec!["alice".to_string(), "bob".to_string()])
}

/// Depends on `seeded_users`, resolved lazily through `ctx.get`.
#[fixture(scope = "test")]
async fn first_user(ctx: TestContext) -> ferridriver_test::Result<String> {
  let users = ctx.get::<Vec<String>>("seeded_users").await?;
  Ok(users.first().cloned().unwrap_or_default())
}

/// A worker-scoped fixture, to prove the scope argument round-trips.
#[fixture(scope = "worker")]
async fn worker_token(_ctx: TestContext) -> ferridriver_test::Result<String> {
  Ok("worker-secret".to_string())
}

fn pool_with_custom_fixtures(scope: FixtureScope) -> FixturePool {
  FixturePool::new(ferridriver_test::collect_rust_fixtures(), scope)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn collects_registered_fixtures() {
  let defs = ferridriver_test::collect_rust_fixtures();
  assert!(defs.contains_key("seeded_users"), "seeded_users should be registered");
  assert!(defs.contains_key("first_user"), "first_user should be registered");
  assert!(defs.contains_key("worker_token"), "worker_token should be registered");
  assert_eq!(defs["worker_token"].scope, FixtureScope::Worker);
  assert_eq!(defs["seeded_users"].scope, FixtureScope::Test);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolves_value_fixture() {
  let pool = pool_with_custom_fixtures(FixtureScope::Test);
  let users = pool
    .get::<Vec<String>>("seeded_users")
    .await
    .expect("resolve seeded_users");
  assert_eq!(&*users, &["alice".to_string(), "bob".to_string()]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolves_dependent_fixture() {
  let pool = pool_with_custom_fixtures(FixtureScope::Test);
  // `first_user` internally `ctx.get`s `seeded_users`.
  let first = pool.get::<String>("first_user").await.expect("resolve first_user");
  assert_eq!(&*first, "alice");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn caches_within_scope() {
  let pool = pool_with_custom_fixtures(FixtureScope::Test);
  let a = pool.get::<Vec<String>>("seeded_users").await.expect("first resolve");
  let b = pool.get::<Vec<String>>("seeded_users").await.expect("second resolve");
  assert!(Arc::ptr_eq(&a, &b), "same scope should return the cached Arc");
}
