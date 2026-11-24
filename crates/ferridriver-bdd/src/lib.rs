//! ferridriver-bdd: BDD/Cucumber/Gherkin support for ferridriver.
//!
//! This crate provides:
//! - Gherkin `.feature` file parsing and scenario expansion
//! - Cucumber expression step matching
//! - Step registry with proc macro registration (`#[given]`, `#[when]`, `#[then]`)
//! - Hook system with tag filtering
//! - Translation of Gherkin features into `TestPlan` for the core `TestRunner`
//! - 109 built-in step definitions covering navigation, interaction, assertions, etc.
//! - BDD-specific reporters (Gherkin terminal, Cucumber JSON, `JUnit`, JSON)
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
//!     world.page().goto("https://app.example.com/login").await.map_err(|e| step_err!("{e}"))?;
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
pub mod dry_run;
pub mod executor;
pub mod expression;
pub mod feature;
pub mod filter;
pub mod hook;
pub mod js;
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
    fn main() -> ::std::process::ExitCode {
      $crate::run_bdd_harness()
    }
  };
}

/// Entry point called by `bdd_main!()`.
///
/// Discovers features, builds step registry, translates to `TestPlan`,
/// and runs via the core `TestRunner` with full parallel execution,
/// retries, sharding, and reporter support. Returns the run's exit status
/// for `main` to hand back to the process.
pub fn run_bdd_harness() -> std::process::ExitCode {
  ferridriver_test::logging::init_from_env();

  let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
    Ok(rt) => rt,
    Err(e) => {
      eprintln!("failed to build tokio runtime: {e}");
      return std::process::ExitCode::FAILURE;
    },
  };

  let exit_code = rt.block_on(async {
    let overrides = ferridriver_test::parse_common_cli_args();
    match ferridriver_test::config::resolve_config(&overrides) {
      Ok(config) => run_bdd_with(config, overrides).await,
      Err(e) => {
        eprintln!("config error: {e}");
        ferridriver_test::exit::FAILED
      },
    }
  });

  // Browser children are `kill_on_drop`; shutting the runtime down runs
  // those drops before the process exits.
  rt.shutdown_timeout(std::time::Duration::from_secs(5));

  u8::try_from(exit_code).map_or(std::process::ExitCode::FAILURE, std::process::ExitCode::from)
}

/// Run BDD scenarios against a pre-resolved `TestConfig` and `CliOverrides`.
///
/// Used by both [`run_bdd_harness`] (for `bdd_main!()` binaries) and the
/// unified `ferridriver bdd` CLI subcommand. The caller is responsible for
/// loading `FerridriverConfig` and applying any subcommand-specific overrides
/// before calling this function.
///
/// Returns the exit code for the run (0 = success).
pub fn run_bdd_with(
  config: ferridriver_test::config::TestConfig,
  overrides: ferridriver_test::config::CliOverrides,
) -> impl std::future::Future<Output = i32> {
  // Boxed before a future captures them: held by value, the two configs
  // would sit in the run's future state twice, for the whole run.
  run_bdd(Box::new(config), Box::new(overrides))
}

async fn run_bdd(
  mut config: Box<ferridriver_test::config::TestConfig>,
  overrides: Box<ferridriver_test::config::CliOverrides>,
) -> i32 {
  if let Err(code) = apply_bdd_overrides(&mut config, &overrides) {
    return code;
  }

  // JS step files take the QuickJS path; otherwise inventory-collected
  // Rust steps. `--steps` overrides `[test].steps`. Top-level
  // `extensions` (config or `FERRIDRIVER_EXTENSIONS`) are bundled in the
  // same module so one extension serves MCP tools AND BDD steps.
  let js_globs: Vec<String> = if overrides.bdd_steps.is_empty() {
    config.steps.clone()
  } else {
    overrides.bdd_steps.clone()
  };
  let extensions = extension_specs(&overrides);

  config.has_bdd = true;

  if overrides.ui || overrides.watch {
    return run_bdd_watch(config, overrides, js_globs, extensions).await;
  }

  let plans = match build_bdd_plans(&config, &js_globs, &extensions, None).await {
    Ok(built) => built,
    Err(e) => {
      eprintln!("{e}");
      return e.exit_code();
    },
  };
  let BddPlans {
    shared: plan,
    per_project: project_plans,
    issues,
  } = plans;
  if plan.total_tests == 0 && project_plans.values().all(|p| p.total_tests == 0) && !overrides.list_only {
    eprintln!("no scenarios found");
    return 0;
  }

  let strict = config.strict;
  let list_only = overrides.list_only;
  let list_json = overrides.list_json;
  // Boxed for the same reason as the configs it is built from.
  let mut runner = Box::new(ferridriver_test::runner::TestRunner::new(*config, *overrides));
  runner.set_project_plans(project_plans);
  if list_only {
    return list_scenarios(&runner, &plan, &issues, strict, list_json);
  }
  let code = runner.run_with_signals(plan).await;
  js::teardown_worker_sessions().await;
  code
}

/// Fold the BDD-only environment variables and flags into `config`.
///
/// Fails with the exit status to return when `--world-parameters` is not
/// JSON.
fn apply_bdd_overrides(
  config: &mut ferridriver_test::config::TestConfig,
  overrides: &ferridriver_test::config::CliOverrides,
) -> Result<(), i32> {
  // BDD-specific feature glob defaults from env vars (used by the macro entry
  // point; the CLI subcommand sets `config.features` explicitly).
  let feature_patterns = std::env::var("FERRIDRIVER_FEATURES").ok().map_or_else(
    || vec!["features/**/*.feature".to_string()],
    |s| s.split(',').map(String::from).collect::<Vec<_>>(),
  );

  if config.features.is_empty() {
    config.features = feature_patterns;
  }

  if let Ok(tags) = std::env::var("FERRIDRIVER_TAGS")
    && config.tags.is_none()
  {
    config.tags = Some(tags);
  }

  // BDD-specific CLI overrides.
  if let Some(ref tags) = overrides.bdd_tags {
    config.tags = Some(tags.clone());
  }
  // Listing matches steps exactly as a dry run does, and like it never
  // needs a browser.
  if overrides.bdd_dry_run || overrides.list_only {
    config.dry_run = true;
  }
  if config.dry_run {
    // Nothing a dry run does reaches a server.
    config.web_server.clear();
  }
  if overrides.bdd_fail_fast {
    config.fail_fast = true;
  }
  if let Some(t) = overrides.bdd_step_timeout {
    config.timeout = t;
  }
  if let Some(strict) = overrides.bdd_strict {
    config.strict = strict;
  }
  if let Some(ref order) = overrides.bdd_order {
    config.order.clone_from(order);
  }
  if overrides.bdd_language.is_some() {
    config.language.clone_from(&overrides.bdd_language);
  }
  if let Some(s) = overrides.world_parameters.as_deref() {
    match serde_json::from_str::<serde_json::Value>(s) {
      Ok(v) => config.world_parameters = v,
      Err(e) => {
        eprintln!("--world-parameters: invalid JSON: {e}");
        return Err(1);
      },
    }
  }

  Ok(())
}

/// The `--extension` flags, else the `FERRIDRIVER_EXTENSIONS` fallback.
fn extension_specs(overrides: &ferridriver_test::config::CliOverrides) -> Vec<ferridriver_script::ExtensionSpec> {
  if overrides.extensions.is_empty() {
    // The env fallback has no declaring file, so its entries resolve
    // against the process cwd.
    let base = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    std::env::var("FERRIDRIVER_EXTENSIONS")
      .ok()
      .map(|s| {
        s.split(',')
          .map(str::trim)
          .filter(|s| !s.is_empty())
          .map(|spec| ferridriver_script::ExtensionSpec {
            spec: spec.to_string(),
            base_dir: base.clone(),
          })
          .collect()
      })
      .unwrap_or_default()
  } else {
    overrides.extensions.clone()
  }
}

/// Watch/UI mode: every cycle rebuilds the plan from disk — features
/// re-discovered (narrowed to the changed `.feature` files when the
/// watcher hands them over) and the JS/TS step graph re-bundled so step
/// edits take effect without restarting. UI mode wins when both flags
/// are passed.
async fn run_bdd_watch(
  config: Box<ferridriver_test::config::TestConfig>,
  overrides: Box<ferridriver_test::config::CliOverrides>,
  js_globs: Vec<String>,
  extensions: Vec<ferridriver_script::ExtensionSpec>,
) -> i32 {
  let ui_mode = overrides.ui;
  let ui_port = overrides.ui_port;
  let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
  let factory_config = (*config).clone();
  let factory: ferridriver_test::runner::WatchPlanFactory = Box::new(move |changed| {
    let config = factory_config.clone();
    let js_globs = js_globs.clone();
    let extensions = extensions.clone();
    Box::pin(async move {
      match build_bdd_plans(&config, &js_globs, &extensions, changed.as_deref()).await {
        // Watch and UI rebuild the shared plan only: a per-project
        // rebuild would have to reach the runner between cycles, and
        // the factory hands back one plan. A project-scoped BDD suite
        // under `--watch` re-runs the union, which is a superset —
        // never a silently narrower corpus.
        Ok(plans) => ferridriver_test::runner::PlanBuild::ok(plans.shared),
        Err(e) => {
          ferridriver_test::runner::PlanBuild::failed(ferridriver_test::model::TestPlan::default(), e.to_string())
        },
      }
    })
  });
  let mut runner = Box::new(ferridriver_test::runner::TestRunner::new(*config, *overrides));
  let code = if ui_mode {
    runner.run_test_server(factory, cwd, None, ui_port).await
  } else {
    runner.run_watch(factory, cwd).await
  };
  // Worker VMs outlive the individual cycles, so their `AfterAll`
  // hooks and worker-scoped fixtures are torn down once, here.
  js::teardown_worker_sessions().await;
  code
}

/// `bdd --list`: the scenarios a run would execute, each with the steps no
/// definition matches (or several do). Exits non-zero when a run would
/// fail on them: any ambiguous step, or an undefined one under `--strict`.
fn list_scenarios(
  runner: &ferridriver_test::runner::TestRunner,
  plan: &ferridriver_test::model::TestPlan,
  issues: &dry_run::StepIssues,
  strict: bool,
  json: bool,
) -> i32 {
  use ferridriver_test::exit;
  let mut listed = match runner.validate_selection(plan).and_then(|()| runner.list(plan)) {
    Ok(listed) => listed,
    Err(message) => {
      eprintln!("Error: {message}");
      return exit::USAGE;
    },
  };
  let mut errors: Vec<ferridriver_test::listing::ListError> = Vec::new();
  let mut fatal = false;
  for test in &mut listed {
    let Some(found) = issues.get(&dry_run::issue_key(&test.id)) else {
      continue;
    };
    for issue in found {
      fatal |= issue.kind == "ambiguous" || strict;
      errors.push(ferridriver_test::listing::ListError {
        message: format!("{} step: {}{}", issue.kind, issue.keyword, issue.text),
        stable_id: Some(test.record.stable_id.clone()),
        file: Some(test.record.file.clone()),
        line: Some(issue.line),
      });
    }
    test.record.step_issues.clone_from(found);
  }
  if let Err(e) = ferridriver_test::listing::print(&listed, &errors, json) {
    eprintln!("Error: could not write the listing: {e}");
    return exit::INFRASTRUCTURE;
  }
  if fatal { exit::FAILED } else { exit::PASSED }
}

/// Why a BDD plan could not be built, with the status that says so.
#[derive(Debug)]
pub enum PlanError {
  /// The configuration or a flag is wrong (a tag expression that does
  /// not parse): nothing could run.
  Usage(String),
  /// The suite itself is broken (a step bundle that does not compile).
  Suite(String),
}

impl PlanError {
  /// The process exit status for this failure.
  #[must_use]
  pub fn exit_code(&self) -> i32 {
    match self {
      Self::Usage(_) => ferridriver_test::exit::USAGE,
      Self::Suite(_) => ferridriver_test::exit::FAILED,
    }
  }
}

impl std::fmt::Display for PlanError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Usage(message) | Self::Suite(message) => f.write_str(message),
    }
  }
}

impl std::error::Error for PlanError {}

/// The plans of one BDD run.
pub struct BddPlans {
  /// The plan every project without its own discovery narrows.
  pub shared: ferridriver_test::model::TestPlan,
  /// A plan per project that declares its own features, steps or tags.
  pub per_project: rustc_hash::FxHashMap<String, ferridriver_test::model::TestPlan>,
  /// Undefined and ambiguous steps, found when the config asks for a dry
  /// run (and so for `--list`); empty otherwise.
  pub issues: dry_run::StepIssues,
}

/// One corpus: its plan and, in a dry run, its step issues.
pub struct BddPlan {
  pub plan: ferridriver_test::model::TestPlan,
  pub issues: dry_run::StepIssues,
}

/// What decides a project's BDD corpus: its features, steps and tags.
type CorpusKey = (Vec<String>, Vec<String>, Option<String>);

/// Build the plan every project shares, plus a plan of its own for each
/// project that declares one.
///
/// A project's BDD corpus is a DISCOVERY, not a narrowing: `features`
/// chooses different files, `steps` a different registry, `tags` a
/// different selection made before the outline rows even exist. So a
/// project that names any of the three is discovered separately, and the
/// runner is handed its plan directly.
///
/// A config whose projects declare none of them takes exactly the path
/// it took before — one discovery, one bundle — and two projects that
/// resolve to the same three inputs share a single build.
///
/// # Errors
///
/// Returns the message to print when a feature glob cannot be walked, a
/// tag expression does not parse, or the step graph does not bundle.
pub async fn build_bdd_plans(
  config: &ferridriver_test::config::TestConfig,
  js_globs: &[String],
  extensions: &[ferridriver_script::ExtensionSpec],
  only_features: Option<&[std::path::PathBuf]>,
) -> Result<BddPlans, PlanError> {
  let BddPlan {
    plan: shared,
    mut issues,
  } = build_bdd_plan(config, js_globs, extensions, only_features).await?;
  let mut per_project = rustc_hash::FxHashMap::default();
  if config.projects.is_empty() {
    return Ok(BddPlans {
      shared,
      per_project,
      issues,
    });
  }

  // (features, steps, tags) -> the plan they produce. Two projects that
  // differ only in browser share one discovery and one step bundle.
  let mut built: Vec<(CorpusKey, ferridriver_test::model::TestPlan)> = Vec::new();
  for project in &config.projects {
    if project.features.is_none() && project.steps.is_none() && project.tags.is_none() {
      continue;
    }
    // Boxed: a whole `TestConfig` would otherwise sit in this future's
    // state beside the nested plan build.
    let merged = Box::new(config.merge_project(project));
    let globs: Vec<String> = project.steps.clone().unwrap_or_else(|| js_globs.to_vec());
    let key = (merged.features.clone(), globs.clone(), merged.tags.clone());
    let plan = if let Some((_, hit)) = built.iter().find(|(k, _)| *k == key) {
      hit.clone()
    } else {
      let corpus = build_bdd_plan(&merged, &globs, extensions, only_features).await?;
      issues.extend(corpus.issues);
      built.push((key, corpus.plan.clone()));
      corpus.plan
    };
    per_project.insert(project.name.clone(), plan);
  }
  Ok(BddPlans {
    shared,
    per_project,
    issues,
  })
}

/// Build a BDD test plan from disk: discover + parse features (narrowed
/// to `only_features` when given) and translate through the Rust step
/// registry or the bundled JS/TS step graph.
///
/// # Errors
///
/// As [`build_bdd_plans`], of which this is the single-corpus half.
pub async fn build_bdd_plan(
  config: &ferridriver_test::config::TestConfig,
  js_globs: &[String],
  extensions: &[ferridriver_script::ExtensionSpec],
  only_features: Option<&[std::path::PathBuf]>,
) -> Result<BddPlan, PlanError> {
  use std::sync::Arc;

  let patterns: Vec<String> = match only_features {
    Some(paths) if !paths.is_empty() => paths.iter().map(|p| p.to_string_lossy().into_owned()).collect(),
    _ => config.features.clone(),
  };
  // Parsed once per plan, and the tag expression with it: an invalid
  // `--tags` / `[test].tags` / project `tags` fails the run naming the
  // problem rather than quietly selecting everything.
  let options = crate::scenario::ExpandOptions {
    examples_title_format: config.examples_title_format.clone(),
    tag_filter: match config.tags.as_deref() {
      Some(expr) => {
        Some(filter::TagExpression::parse(expr).map_err(|e| PlanError::Usage(format!("invalid tag expression: {e}")))?)
      },
      None => None,
    },
  };
  let feature_set = feature::FeatureSet::discover_and_parse(&patterns, &config.test_ignore)
    .map_err(|e| PlanError::Usage(format!("feature discovery error: {e}")))?;
  if feature_set.features.is_empty() {
    // Not an error: one-shot runs report "no scenarios found" and exit
    // 0 (pre-watch behavior); watch cycles simply idle until the next
    // change.
    eprintln!("no feature files found matching: {patterns:?}");
    return Ok(BddPlan {
      plan: ferridriver_test::model::TestPlan::default(),
      issues: dry_run::StepIssues::default(),
    });
  }
  let root = ferridriver_test::identity::root(config);
  if js_globs.is_empty() && extensions.is_empty() {
    let registry = Arc::new(registry::StepRegistry::build());
    let (mut plan, issues) = if config.dry_run {
      dry_run::translate(&feature_set, &registry, config, &options, true)
    } else {
      (
        translate::translate_features(&feature_set, &registry, config, &options),
        dry_run::StepIssues::default(),
      )
    };
    plan.anchor(&root);
    Ok(BddPlan { plan, issues })
  } else {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    // Extensions load once per plan build, through the gate every host
    // shares, and every worker VM installs the same bytecode. Before
    // this they were bundled into the step module as SOURCE: never
    // gated, never manifest-extracted, and out of reach of the
    // `[extensions.policy]` ceiling.
    //
    // This runs BEFORE the step bundle, and has to: loading is what
    // installs the provided-module table, so a step file importing a
    // specifier a package claims (`playwright-bdd`) fails to resolve if
    // the bundle is built first.
    let caps = js::bdd_script_caps();
    let sidecar_names: Vec<String> = js::bdd_sidecars().iter().map(|s| s.name.clone()).collect();
    let env = ferridriver_script::RequirementEnv::from_caps(&caps, &sidecar_names);
    let bindings = ferridriver_script::load_bindings(
      extensions,
      &env,
      &caps.extension_policy,
      ferridriver_script::ExtensionHost::Bdd,
    )
    .await;
    // rolldown-bundle + tree-shake + transpile the whole step +
    // extension graph to one module, compiled to bytecode once per
    // plan build.
    let bundle = js::bundle_steps_with(js_globs, extensions, &cwd)
      .await
      .map_err(|e| PlanError::Suite(format!("step bundle error: {e}")))?;
    let bindings: Arc<[ferridriver_script::ExtensionBinding]> = Arc::from(bindings);
    let (mut plan, issues) = if config.dry_run {
      let setup = js::BddSessionSetup {
        world_parameters: config.world_parameters.clone(),
        open_use_keys: Arc::from([]),
        extensions: Arc::clone(&bindings),
        builtin_steps: config.builtin_steps,
      };
      let registry = js::collect_step_registry(bundle, &cwd, &setup)
        .await
        .map_err(|e| PlanError::Suite(format!("step bundle error: {e}")))?;
      dry_run::translate(&feature_set, &registry, config, &options, false)
    } else {
      (
        js::translate_features_js(&feature_set, config, &bundle, &cwd, bindings, &options),
        dry_run::StepIssues::default(),
      )
    };
    plan.anchor(&root);
    Ok(BddPlan { plan, issues })
  }
}
