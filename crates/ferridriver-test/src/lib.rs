//! ferridriver-test -- High-performance E2E test runner for browser automation.
//!
//! Provides a Playwright Test-compatible API for writing and running browser tests
//! with automatic fixture injection, parallel execution, and rich reporting.
//!
//! # Quick Start (Rust)
//!
//! ```ignore
//! use ferridriver_test::prelude::*;
//!
//! #[ferritest]
//! async fn basic_navigation(page: Page) {
//!     page.goto("https://example.com", None).await.unwrap();
//!     expect(&page).to_have_title("Example Domain").await.unwrap();
//! }
//!
//! #[ferritest(retries = 2, tag = "smoke")]
//! async fn login_test(page: Page) {
//!     page.goto("https://app.example.com/login", None).await.unwrap();
//!     page.locator("#email").fill("user@example.com").await.unwrap();
//!     page.locator("#password").fill("password").await.unwrap();
//!     page.locator("button[type=submit]").click().await.unwrap();
//!     expect(&page).to_have_url("https://app.example.com/dashboard").await.unwrap();
//! }
//! ```

// ── Core modules ──
pub mod config;
pub mod discovery;
pub mod dispatcher;
pub mod expect;
pub mod fixture;
pub mod model;
pub mod reporter;
pub mod retry;
pub mod runner;
pub mod shard;
pub mod snapshot;
pub mod tracing;
pub mod worker;

// ── Re-exports ──
pub use config::{CliOverrides, TestConfig};
pub use discovery::TestRegistration;
pub use expect::{expect, expect_configured, expect_poll, to_pass};
pub use fixture::FixturePool;
pub use model::{
  SuiteMode, TestAnnotation, TestCase, TestFailure, TestFn, TestId, TestInfo, TestOutcome, TestPlan,
  TestStatus, TestStep,
};
pub use reporter::{Reporter, ReporterEvent, ReporterSet};
pub use runner::TestRunner;

// Re-export proc macros.
pub use ferridriver_test_macros::ferritest;

// Re-export inventory for the proc macro expansion.
pub use inventory;

/// Prelude for convenient imports in test files.
pub mod prelude {
  pub use ferridriver::{Browser, ContextRef as BrowserContext, Locator, Page};

  pub use crate::expect::{expect, expect_configured, expect_poll, to_pass};
  pub use crate::fixture::FixturePool;
  pub use crate::model::{TestFailure, TestInfo};
  pub use ferridriver_test_macros::ferritest;
}
