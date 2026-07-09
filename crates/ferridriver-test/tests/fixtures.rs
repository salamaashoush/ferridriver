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

/// Depends on `seeded_users` via a typed parameter (name = fixture name).
#[fixture(scope = "test")]
async fn user_count(seeded_users: Arc<Vec<String>>) -> ferridriver_test::Result<usize> {
  Ok(seeded_users.len())
}

/// A `#[ferritest]` with typed fixture parameters, exercised without a
/// browser by invoking its registered `test_fn` directly.
#[ferritest]
async fn typed_params_test(seeded_users: Arc<Vec<String>>, user_count: Arc<usize>, ctx: TestContext) {
  assert_eq!(seeded_users.first().map(String::as_str), Some("alice"));
  assert_eq!(*user_count, 2);
  let via_ctx = ctx.get::<Vec<String>>("seeded_users").await?;
  assert!(Arc::ptr_eq(&via_ctx, &seeded_users));
}

#[ferritest_each(data = [("alice", 0), ("bob", 1)], tag = "roster")]
async fn typed_each_test(seeded_users: Arc<Vec<String>>, expected: &str, index: usize) {
  assert_eq!(seeded_users[index], expected);
}

#[ferritest_each(data = [(1, 2), (2, 4)], names = ["one doubles", "two doubles"])]
async fn named_each_test(input: u32, output: u32) {
  assert_eq!(input * 2, output);
}

static TORN_DOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A fixture with cleanup: returns `Fixture<T>` with an `on_teardown`.
#[fixture(scope = "test")]
async fn tracked_resource(_ctx: TestContext) -> ferridriver_test::Result<Fixture<String>> {
  Ok(Fixture::new("resource".to_string()).on_teardown(|value| async move {
    assert_eq!(&*value, "resource");
    TORN_DOWN.store(true, std::sync::atomic::Ordering::SeqCst);
  }))
}

fn pool_with_custom_fixtures(scope: FixtureScope) -> FixturePool {
  FixturePool::new(ferridriver_test::collect_rust_fixtures(), scope)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fixture_guard_registers_teardown() {
  let pool = pool_with_custom_fixtures(FixtureScope::Test);
  let value = pool
    .get::<String>("tracked_resource")
    .await
    .expect("resolve guard fixture");
  assert_eq!(&*value, "resource");
  assert!(
    !TORN_DOWN.load(std::sync::atomic::Ordering::SeqCst),
    "teardown must not run while the scope is alive"
  );
  drop(value);
  pool.teardown_all().await;
  assert!(
    TORN_DOWN.load(std::sync::atomic::Ordering::SeqCst),
    "teardown_all must invoke the fixture's on_teardown"
  );
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typed_fixture_params_declare_dependencies() {
  let defs = ferridriver_test::collect_rust_fixtures();
  assert_eq!(defs["user_count"].dependencies, vec!["seeded_users".to_string()]);
  assert!(defs["seeded_users"].dependencies.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typed_test_params_resolve_and_register() {
  let reg = ferridriver_test::inventory::iter::<ferridriver_test::TestRegistration>()
    .find(|r| r.name == "typed_params_test")
    .expect("typed_params_test registered");
  assert_eq!(reg.fixture_requests, &["seeded_users", "user_count"]);

  let pool = pool_with_custom_fixtures(FixtureScope::Test);
  (reg.test_fn)(pool).await.expect("typed params test body passes");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ferritest_each_names_label_rows() {
  let names: Vec<_> = ferridriver_test::inventory::iter::<ferridriver_test::TestRegistration>()
    .filter(|r| r.name.starts_with("named_each_test"))
    .map(|r| r.name)
    .collect();
  assert_eq!(names.len(), 2);
  assert!(names.contains(&"named_each_test (one doubles)"));
  assert!(names.contains(&"named_each_test (two doubles)"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ferritest_each_rows_with_fixtures_and_args() {
  let regs: Vec<_> = ferridriver_test::inventory::iter::<ferridriver_test::TestRegistration>()
    .filter(|r| r.name.starts_with("typed_each_test"))
    .collect();
  assert_eq!(regs.len(), 2, "one registration per data row");
  for reg in &regs {
    assert_eq!(reg.fixture_requests, &["seeded_users"]);
    assert!(
      (reg.annotations)()
        .iter()
        .any(|a| matches!(a, ferridriver_test::TestAnnotation::Tag(t) if t == "roster")),
      "shared tag applies to every row"
    );
    let pool = pool_with_custom_fixtures(FixtureScope::Test);
    (reg.test_fn)(pool).await.expect("each-row body passes");
  }
}
