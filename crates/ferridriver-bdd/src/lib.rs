#![allow(
  clippy::missing_errors_doc,
  clippy::missing_panics_doc,
  clippy::must_use_candidate,
  clippy::doc_markdown,
  clippy::module_name_repetitions,
  clippy::cast_possible_truncation,
  clippy::cast_precision_loss,
  clippy::cast_sign_loss,
  clippy::redundant_closure_for_method_calls,
  clippy::implicit_clone,
  clippy::too_many_lines,
  clippy::uninlined_format_args,
  clippy::type_complexity,
  clippy::unnecessary_map_or,
  clippy::match_same_arms,
  clippy::should_implement_trait,
  clippy::unnecessary_wraps,
  clippy::unused_async,
  clippy::items_after_statements,
  clippy::needless_pass_by_value,
  clippy::single_match_else,
  clippy::vec_init_then_push,
  clippy::from_over_into,
  clippy::single_char_pattern,
  clippy::ptr_arg,
  clippy::unnecessary_sort_by,
  clippy::collapsible_match,
  clippy::if_same_then_else,
  clippy::single_match
)]
// BDD step proc macros use BrowserWorld, DataTable etc. in expanded code; clippy can't see through macros.
#![allow(unused_imports)]
//! ferridriver-bdd: BDD/Cucumber/Gherkin support for ferridriver.
//!
//! This crate provides:
//! - Gherkin `.feature` file parsing and scenario expansion
//! - Cucumber expression step matching
//! - Step registry with proc macro registration (`#[given]`, `#[when]`, `#[then]`)
//! - Hook system with tag filtering
//! - Translation of Gherkin features into `TestPlan` for the core `TestRunner`
//! - 109 built-in step definitions covering navigation, interaction, assertions, etc.
//! - BDD-specific reporters (Gherkin terminal, Cucumber JSON, JUnit, JSON)
//!
//! # Quick Start
//!
//! Create a binary crate with custom steps and call `bdd_main!()`:
//!
//! ```ignore
//! use ferridriver_bdd::prelude::*;
//!
//! // Custom step definitions -- auto-registered via inventory
//! #[given("I am logged in as {string}")]
//! async fn login(world: &mut BrowserWorld, username: String) {
//!     world.page().goto("https://app.example.com/login", None).await.map_err(|e| step_err!("{e}"))?;
//!     world.page().locator("#email").fill(&username).await.map_err(|e| step_err!("{e}"))?;
//!     world.page().locator("#password").fill("secret").await.map_err(|e| step_err!("{e}"))?;
//!     world.page().locator("button[type=submit]").click().await.map_err(|e| step_err!("{e}"))?;
//! }
//!
//! #[then("I should see the dashboard")]
//! async fn see_dashboard(world: &mut BrowserWorld) {
//!     let loc = world.page().locator("[data-testid=dashboard]");
//!     ferridriver_test::expect::expect(&loc).to_be_visible().await.map_err(|e| step_err!("{e}"))?;
//! }
//!
//! // Entry point -- discovers .feature files, collects all steps, runs via TestRunner
//! ferridriver_bdd::bdd_main!();
//! ```
//!
//! Then run: `cargo run -- features/**/*.feature --tags "@smoke"`

// Allow the proc macros to reference `ferridriver_bdd::` paths within this crate.
extern crate self as ferridriver_bdd;

// Re-export proc macros.
pub use ferridriver_bdd_macros::{after, before, given, param_type, step, then, when};

// Re-export inventory so proc macro expansions can find it in downstream crates.
pub use inventory;

pub mod data_table;
pub mod executor;
pub mod expression;
pub mod feature;
pub mod filter;
pub mod hook;
pub mod param_type;
pub mod registry;
// Reporters have been unified into ferridriver_test::reporter (including bdd/ submodule).
pub mod scenario;
pub mod snippet;
pub mod step;
pub mod steps;
pub mod translate;
pub mod world;

/// Prelude: commonly used types for step definition files.
pub mod prelude {
  pub use crate::step::{DataTable, StepError, StepParam};
  pub use crate::step_err;
  pub use crate::world::BrowserWorld;

  // Re-export proc macros.
  pub use ferridriver_bdd_macros::{after, before, given, param_type, step, then, when};

  // Re-export ferridriver types commonly used in steps.
  pub use ferridriver::Page;
}

/// Convenience macro for creating step errors.
#[macro_export]
macro_rules! step_err {
  ($($arg:tt)*) => {
    $crate::step::StepError::from(format!($($arg)*))
  };
}

/// BDD test harness entry point.
///
/// Generates a `main()` that:
/// 1. Collects all `#[given]`/`#[when]`/`#[then]` steps via `inventory`
/// 2. Discovers and parses `.feature` files
/// 3. Translates scenarios into a `TestPlan`
/// 4. Runs via the core `TestRunner` (same worker pool, parallel dispatch,
///    retries, reporters as E2E and component tests)
///
/// # Usage
///
/// ```ignore
/// use ferridriver_bdd::prelude::*;
///
/// #[given("I do something")]
/// async fn my_step(world: &mut BrowserWorld) { /* ... */ }
///
/// ferridriver_bdd::bdd_main!();
/// ```
///
/// Run with:
/// ```sh
/// cargo test -p my-bdd-tests                          # run all
/// cargo test -p my-bdd-tests -- --headed --workers 2  # headed, 2 workers
/// ```
///
/// Environment variables for BDD-specific config:
/// - `FERRIDRIVER_FEATURES` -- comma-separated feature file globs (default: `features/**/*.feature`)
/// - `FERRIDRIVER_TAGS` -- tag filter expression (e.g., `@smoke and not @wip`)
#[macro_export]
macro_rules! bdd_main {
  () => {
    fn main() {
      $crate::run_bdd_harness();
    }
  };
}

/// Entry point called by `bdd_main!()`.
///
/// Discovers features, builds step registry, translates to `TestPlan`,
/// and runs via the core `TestRunner` with full parallel execution,
/// retries, sharding, and reporter support.
pub fn run_bdd_harness() {
  use std::sync::Arc;

  ferridriver_test::logging::init_from_env();

  let rt = tokio::runtime::Builder::new_multi_thread()
    .enable_all()
    .build()
    .expect("failed to build tokio runtime");

  let exit_code = rt.block_on(async {
    // Parse CLI args (shared parser from ferridriver-test).
    let overrides = ferridriver_test::parse_common_cli_args();

    // Resolve config.
    let mut config = ferridriver_test::config::resolve_config(&overrides).unwrap_or_else(|e| {
      eprintln!("config error: {e}");
      std::process::exit(1);
    });

    // BDD-specific config from env vars.
    let feature_patterns = std::env::var("FERRIDRIVER_FEATURES")
      .ok()
      .map(|s| s.split(',').map(String::from).collect::<Vec<_>>())
      .unwrap_or_else(|| vec!["features/**/*.feature".to_string()]);

    if config.features.is_empty() {
      config.features = feature_patterns;
    }

    if let Ok(tags) = std::env::var("FERRIDRIVER_TAGS") {
      if config.tags.is_none() {
        config.tags = Some(tags);
      }
    }

    // Apply BDD-specific CLI overrides.
    if let Some(ref tags) = overrides.bdd_tags {
      config.tags = Some(tags.clone());
    }
    if overrides.bdd_dry_run {
      config.dry_run = true;
    }
    if overrides.bdd_fail_fast {
      config.fail_fast = true;
    }
    if let Some(t) = overrides.bdd_step_timeout {
      config.timeout = t;
    }
    if overrides.bdd_strict {
      config.strict = true;
    }
    if let Some(ref order) = overrides.bdd_order {
      config.order = order.clone();
    }
    if overrides.bdd_language.is_some() {
      config.language = overrides.bdd_language.clone();
    }

    // Discover and parse .feature files.
    let feature_set = match feature::FeatureSet::discover_and_parse(&config.features, &config.test_ignore) {
      Ok(fs) => fs,
      Err(e) => {
        eprintln!("feature discovery error: {e}");
        return 1;
      },
    };

    if feature_set.features.is_empty() {
      eprintln!("no feature files found matching: {:?}", config.features);
      return 0;
    }

    // Build step registry (collects all steps from this binary via inventory).
    let registry = Arc::new(registry::StepRegistry::build());

    // Translate features to TestPlan.
    let plan = translate::translate_features(&feature_set, registry, &config);

    if plan.total_tests == 0 {
      eprintln!("no scenarios found");
      return 0;
    }

    // Run via core TestRunner.
    config.has_bdd = true;
    let mut runner = ferridriver_test::runner::TestRunner::new(config, overrides);
    runner.run(plan).await
  });

  std::process::exit(exit_code);
}

