//! ferridriver-test -- Rust-based E2E test runner for browser automation.
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
//! async fn basic_navigation(page: Arc<Page>) {
//!     page.goto("https://example.com").await?;
//!     expect(&page).to_have_title("Example Domain").await?;
//! }
//!
//! #[ferritest(retries = 2, tag = "smoke")]
//! async fn login_test(page: Arc<Page>) {
//!     page.goto("https://app.example.com/login").await?;
//!     page.locator("#email").fill("user@example.com").await?;
//!     page.locator("#password").fill("password").await?;
//!     page.locator("button[type=submit]").click().await?;
//!     expect(&page).to_have_url("https://app.example.com/dashboard").await?;
//! }
//! ```

/// Result type for `#[fixture]` bodies and other test helpers: a
/// `std::result::Result` whose error is [`TestFailure`].
pub type Result<T> = std::result::Result<T, model::TestFailure>;

// -- Core modules --
pub mod api_steps;
pub mod artifacts;
pub mod comparator;
pub mod config;
pub mod context;
pub(crate) mod context_pool;
pub mod ct;
pub mod debug;
pub mod discovery;
pub mod dispatcher;
pub mod error_context;
pub mod exit;
pub mod expect;
pub mod fixture;
pub mod fixture_graph;
pub mod git_info;
pub mod host;
pub mod identity;
pub mod interactive;
pub mod listing;
pub mod locks;
pub mod logging;
pub mod model;
pub mod reporter;
pub mod retry;
pub mod runner;
pub mod server;
pub mod shard;
pub mod snapshot;
pub mod snapshot_path;
pub mod snapshot_record;
pub mod step;
pub mod test_server;
pub(crate) mod timings;
pub mod tracing;
pub mod tui;
pub mod tui_reporter;
pub mod ui_server;
pub mod ui_wire;
pub mod watch;
pub mod worker;

// -- Re-exports --
pub use config::{CliOverrides, TestConfig, parse_common_cli_args};
pub use context::TestContext;
pub use discovery::{
  FixtureRegistration, HookKindTag, HookRegistration as InventoryHookRegistration, SuiteModeRegistration,
  TestRegistration, collect_rust_fixtures,
};
pub use expect::{ToPassOptions, expect, expect_configured, expect_poll, to_pass, to_pass_with_options};
pub use fixture::{Fixture, FixtureDef, FixturePool, FixtureScope};
pub use model::{
  HookDef, HookKind, HookOwner, HookPhase, HookRegistration, HookScope, SuiteDef, SuiteMode, TestAnnotation, TestCase,
  TestFailure, TestFixtures, TestFn, TestId, TestInfo, TestModifiers, TestOutcome, TestPlan, TestPlanBuilder,
  TestStatus, TestStep,
};
pub use reporter::{EventBus, EventBusBuilder, Reporter, ReporterDriver, ReporterEvent, ReporterSet, Subscription};
pub use runner::TestRunner;

// Re-export proc macros.
pub use ferridriver_test_macros::{
  after_all, after_each, before_all, before_each, ferritest, ferritest_each, ferritest_suite, fixture,
};

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
/// async fn my_test(ctx: TestContext) {
///     let page = ctx.page().await?;
///     page.goto("https://example.com").await?;
/// }
///
/// ferridriver_test::main!();
/// ```
#[macro_export]
macro_rules! main {
  () => {
    fn main() -> ::std::process::ExitCode {
      $crate::run_harness()
    }
  };
}

/// Publish stopped tests as sessions, when this binary links a host that
/// can (`ferridriver-script` registers one).
///
/// Without a publisher the flag is a clear error rather than a silent
/// no-op: a run that quietly ignored `--debug` and finished normally
/// would look like the debugger was broken.
fn install_harness_debug(
  mode: debug::DebugMode,
  overrides: &mut ferridriver_config::test::CliOverrides,
) -> std::result::Result<(), String> {
  let Some(publisher) = debug::debug_publisher() else {
    return Err(
      "--debug needs a session publisher: stopping a test and handing it to a\n\
       debugger means publishing it as a scriptable session, which pulls in the\n\
       scripting engine a harness that never debugs should not pay to build.\n\
       Add it to the dev-dependency:\n\
       \n    ferridriver-script = \"…\"\n"
        .to_string(),
    );
  };
  (publisher.install_default)(mode, overrides).map_err(|e| format!("--debug: {e}"))
}

/// Entry point called by `main!()`. Parses CLI args, loads config,
/// discovers tests, and runs them, and returns the run's exit status
/// for `main` to hand back to the process.
pub fn run_harness() -> std::process::ExitCode {
  logging::init_from_env();

  let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
    Ok(rt) => rt,
    Err(e) => {
      eprintln!("failed to build tokio runtime: {e}");
      return to_exit_code(exit::INFRASTRUCTURE);
    },
  };

  let exit_code = rt.block_on(async {
    let mut overrides = config::parse_common_cli_args();
    let debug_mode = match config::parse_debug_mode() {
      Ok(mode) => mode,
      Err(e) => {
        eprintln!("--debug: {e}");
        return exit::USAGE;
      },
    };
    if let Some(mode) = debug_mode
      && let Err(message) = install_harness_debug(mode, &mut overrides)
    {
      eprintln!("{message}");
      return exit::USAGE;
    }
    let overrides = overrides;
    let config = match config::resolve_config(&overrides) {
      Ok(config) => config,
      Err(e) => {
        eprintln!("config error: {e}");
        return exit::USAGE;
      },
    };
    // Spawned by `ferridriver test --ui`: stream the plan + reporter
    // events back to the CLI's UI server over its unix socket.
    if let Some(sock_path) = std::env::var_os(ui_wire::UI_SOCK_ENV).filter(|v| !v.is_empty()) {
      return Box::pin(ui_wire::run_harness_ui(sock_path.into(), config, overrides)).await;
    }
    let plan = discovery::collect_rust_tests(&config);
    let mut runner = runner::TestRunner::new(config, overrides);
    Box::pin(runner.run_with_signals(plan)).await
  });

  // Drain any still-running tasks so that child processes spawned via
  // `tokio::process::Command::kill_on_drop(true)` actually get their `Drop`
  // impls run before the process exits, leaving no browser zombies.
  rt.shutdown_timeout(std::time::Duration::from_secs(5));

  to_exit_code(exit_code)
}

fn to_exit_code(code: i32) -> std::process::ExitCode {
  u8::try_from(code).map_or(std::process::ExitCode::FAILURE, std::process::ExitCode::from)
}

/// Prelude for convenient imports in test files.
pub mod prelude {
  pub use std::sync::Arc;
  pub use std::time::Duration;

  pub use ferridriver::http_client::{HttpClient, HttpResponse};
  pub use ferridriver::options::{
    LoadState, Modifier, MouseButton, Role, ScreenshotFormat, StringOrRegex, ViewportConfig, WaitState,
  };
  pub use ferridriver::{Browser, ContextRef as BrowserContext, FerriError, Frame, FrameLocator, Locator, Page};

  pub use crate::context::TestContext;
  pub use crate::expect::{
    LocatorSnapshotMatchers, PageSnapshotMatchers, ScreenshotMatcherOptions, expect, expect_configured, expect_poll,
    to_pass,
  };
  pub use crate::fixture::{Fixture, FixturePool};
  pub use crate::model::{Context, TestFailure, TestInfo};
  pub use ferridriver_test_macros::{
    after_all, after_each, before_all, before_each, ferritest, ferritest_each, ferritest_suite, fixture,
  };
}
