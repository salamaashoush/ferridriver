//! Test context: the single object passed to every `#[ferritest]` function.
//!
//! Wraps `FixturePool` and provides typed getters for built-in fixtures.
//! This is the Rust equivalent of Playwright's destructured `{ page, browser, context, testInfo }`.
//!
//! ```ignore
//! #[ferritest]
//! async fn my_test(ctx: TestContext) {
//!     let page = ctx.page().await?;
//!     page.goto("https://example.com").await?;
//! }
//! ```

use std::any::Any;
use std::sync::Arc;

use crate::fixture::FixturePool;
use crate::model::{TestFailure, TestInfo};

/// Context object passed to every `#[ferritest]` test function.
///
/// Provides typed access to all built-in fixtures (page, browser, context, `test_info`)
/// and raw access to the underlying `FixturePool` for custom fixtures.
#[derive(Clone)]
pub struct TestContext {
  pool: FixturePool,
}

impl TestContext {
  /// Create a new `TestContext` wrapping a `FixturePool`.
  #[must_use]
  pub fn new(pool: FixturePool) -> Self {
    Self { pool }
  }

  /// Get the `Page` fixture (test-scoped, fresh per test).
  ///
  /// # Errors
  ///
  /// Returns a [`TestFailure`] when the `page` fixture cannot be set up:
  /// its setup or a dependency's failed or timed out.
  pub async fn page(&self) -> Result<Arc<ferridriver::Page>, TestFailure> {
    self
      .pool
      .get::<ferridriver::Page>("page")
      .await
      .map_err(TestFailure::from)
  }

  /// Get the `Browser` fixture (worker-scoped, shared across tests in a worker).
  ///
  /// # Errors
  ///
  /// Returns a [`TestFailure`] when the `browser` fixture cannot be set up:
  /// its setup or a dependency's failed or timed out.
  pub async fn browser(&self) -> Result<Arc<ferridriver::Browser>, TestFailure> {
    self
      .pool
      .get::<ferridriver::Browser>("browser")
      .await
      .map_err(TestFailure::from)
  }

  /// Get the `BrowserContext` fixture (test-scoped).
  ///
  /// # Errors
  ///
  /// Returns a [`TestFailure`] when the `context` fixture cannot be set up:
  /// its setup or a dependency's failed or timed out.
  pub async fn browser_context(&self) -> Result<Arc<ferridriver::ContextRef>, TestFailure> {
    self
      .pool
      .get::<ferridriver::ContextRef>("context")
      .await
      .map_err(TestFailure::from)
  }

  /// Get the `HttpClient` fixture (worker-scoped, pre-configured with the
  /// run's `base_url`).
  ///
  /// # Errors
  ///
  /// Returns a [`TestFailure`] when the `request` fixture cannot be set up:
  /// its setup or a dependency's failed or timed out.
  pub async fn request(&self) -> Result<Arc<ferridriver::http_client::HttpClient>, TestFailure> {
    self
      .pool
      .get::<ferridriver::http_client::HttpClient>("request")
      .await
      .map_err(TestFailure::from)
  }

  /// Get the `TestInfo` fixture (test-scoped runtime context).
  ///
  /// # Errors
  ///
  /// Returns a [`TestFailure`] when the `test_info` fixture cannot be set up:
  /// its setup or a dependency's failed or timed out.
  pub async fn test_info(&self) -> Result<Arc<TestInfo>, TestFailure> {
    self.pool.get::<TestInfo>("test_info").await.map_err(TestFailure::from)
  }

  /// Resolve a custom `#[fixture]` (or any fixture) by name, returning `Arc<T>`.
  ///
  /// Dependencies resolve lazily and cache for the fixture's scope:
  ///
  /// ```ignore
  /// let users = ctx.get::<Vec<User>>("seeded_users").await?;
  /// ```
  ///
  /// # Errors
  ///
  /// Returns a [`TestFailure`] when no fixture is registered as `name`,
  /// when its value is not a `T`, or when its setup or a dependency's
  /// failed or timed out.
  pub async fn get<T: Any + Send + Sync>(&self, name: &str) -> Result<Arc<T>, TestFailure> {
    self.pool.get::<T>(name).await.map_err(TestFailure::from)
  }

  /// Access the underlying `FixturePool` directly (for custom fixtures).
  #[must_use]
  pub fn pool(&self) -> &FixturePool {
    &self.pool
  }
}
