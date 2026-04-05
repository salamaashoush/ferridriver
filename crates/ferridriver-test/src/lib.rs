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
pub mod ct;
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

/// Run all `#[ferritest]` tests in this binary.
///
/// Reads config from `ferridriver.config.toml` (auto-discovered),
/// applies CLI args (`-- --headed --backend webkit --workers 1`),
/// and runs all registered tests through the parallel runner.
///
/// ```ignore
/// use ferridriver_test::prelude::*;
///
/// #[ferritest]
/// async fn my_test(page: Page) {
///     page.goto("https://example.com", None).await.unwrap();
/// }
///
/// ferridriver_test::main!();
/// ```
#[macro_export]
macro_rules! main {
  () => {
    fn main() {
      $crate::run_harness();
    }
  };
}

/// Entry point called by `main!()`. Parses CLI args, loads config,
/// discovers tests, and runs them.
pub fn run_harness() {
  let rt = tokio::runtime::Builder::new_multi_thread()
    .enable_all()
    .build()
    .expect("failed to build tokio runtime");

  let exit_code = rt.block_on(async {
    let overrides = parse_cli_args();
    let config = config::resolve_config(&overrides).unwrap_or_else(|e| {
      eprintln!("config error: {e}");
      std::process::exit(1);
    });
    let reporters = reporter::create_reporters(&config.reporter, &config.output_dir);
    let plan = discovery::collect_rust_tests(&config);
    let mut runner = runner::TestRunner::new(config, reporters, overrides);
    runner.run(plan).await
  });

  std::process::exit(exit_code);
}

fn parse_cli_args() -> CliOverrides {
  let args: Vec<String> = std::env::args().collect();
  let mut overrides = CliOverrides::default();
  let mut i = 1;
  while i < args.len() {
    match args[i].as_str() {
      "--headed" => overrides.headed = true,
      "--workers" | "-j" => {
        i += 1;
        overrides.workers = args.get(i).and_then(|v| v.parse().ok());
      },
      "--retries" => {
        i += 1;
        overrides.retries = args.get(i).and_then(|v| v.parse().ok());
      },
      "--timeout" => {
        i += 1;
        // Stored as env var since CliOverrides doesn't have timeout field
        if let Some(v) = args.get(i) {
          // SAFETY: single-threaded before runner starts
          #[allow(unused_unsafe)]
          unsafe { std::env::set_var("FERRIDRIVER_TIMEOUT", v); }
        }
      },
      "--backend" => {
        i += 1;
        if let Some(v) = args.get(i) {
          // SAFETY: single-threaded before runner starts
          #[allow(unused_unsafe)]
          unsafe { std::env::set_var("FERRIDRIVER_BACKEND", v); }
        }
      },
      "--grep" | "-g" => {
        i += 1;
        overrides.grep = args.get(i).cloned();
      },
      "--tag" => {
        i += 1;
        overrides.tag = args.get(i).cloned();
      },
      "--list" => overrides.list_only = true,
      "--update-snapshots" | "-u" => overrides.update_snapshots = true,
      _ => {},
    }
    i += 1;
  }
  overrides
}

/// Prelude for convenient imports in test files.
pub mod prelude {
  pub use ferridriver::{Browser, ContextRef as BrowserContext, Locator, Page};

  pub use crate::expect::{expect, expect_configured, expect_poll, to_pass};
  pub use crate::fixture::FixturePool;
  pub use crate::model::{TestFailure, TestInfo};
  pub use ferridriver_test_macros::ferritest;
}
