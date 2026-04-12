//! Test context: the single object passed to every `#[ferritest]` function.
//!
//! Wraps `FixturePool` and provides typed getters for built-in fixtures.
//! This is the Rust equivalent of Playwright's destructured `{ page, browser, context, testInfo }`.
//!
//! ```ignore
//! #[ferritest]
//! async fn my_test(ctx: TestContext) {
//!     let page = ctx.page().await?;
//!     page.goto("https://example.com", None).await?;
//! }
//! ```

use std::sync::Arc;

use crate::fixture::FixturePool;
use crate::model::{TestFailure, TestInfo};

/// Context object passed to every `#[ferritest]` test function.
///
/// Provides typed access to all built-in fixtures (page, browser, context, test_info)
/// and raw access to the underlying `FixturePool` for custom fixtures.
#[derive(Clone)]
pub struct TestContext {
  pool: FixturePool,
}

impl TestContext {
  /// Create a new `TestContext` wrapping a `FixturePool`.
  pub fn new(pool: FixturePool) -> Self {
    Self { pool }
  }

  /// Get the `Page` fixture (test-scoped, fresh per test).
  pub async fn page(&self) -> Result<Arc<ferridriver::Page>, TestFailure> {
    self
      .pool
      .get::<ferridriver::Page>("page")
      .await
      .map_err(TestFailure::from)
  }

  /// Get the `Browser` fixture (worker-scoped, shared across tests in a worker).
  pub async fn browser(&self) -> Result<Arc<ferridriver::Browser>, TestFailure> {
    self
      .pool
      .get::<ferridriver::Browser>("browser")
      .await
      .map_err(TestFailure::from)
  }

  /// Get the `BrowserContext` fixture (test-scoped).
  pub async fn browser_context(&self) -> Result<Arc<ferridriver::ContextRef>, TestFailure> {
    self
      .pool
      .get::<ferridriver::ContextRef>("context")
      .await
      .map_err(TestFailure::from)
  }

  /// Get the `TestInfo` fixture (test-scoped runtime context).
  pub async fn test_info(&self) -> Result<Arc<TestInfo>, TestFailure> {
    self.pool.get::<TestInfo>("test_info").await.map_err(TestFailure::from)
  }

  /// Access the underlying `FixturePool` directly (for custom fixtures).
  pub fn pool(&self) -> &FixturePool {
    &self.pool
  }
}
