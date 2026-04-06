#![allow(
  clippy::too_many_lines,
  clippy::doc_markdown,
  clippy::uninlined_format_args,
  clippy::struct_excessive_bools,
  clippy::cast_possible_truncation,
  clippy::cast_sign_loss,
  clippy::unused_async,
)]
//! ferridriver -- High-performance browser automation CLI.

mod cli;

use clap::Parser;
use tracing_subscriber::{self, EnvFilter};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
  tracing_subscriber::fmt()
    .with_env_filter(EnvFilter::from_default_env().add_directive(tracing::Level::WARN.into()))
    .with_writer(std::io::stderr)
    .with_ansi(false)
    .init();

  let cli = cli::Cli::parse();

  match cli.command {
    cli::Command::Mcp { browser, transport } => {
      let backend = browser.backend_kind();
      let mode = browser.connect_mode();
      let headless = browser.headless;

      match transport.transport {
        cli::Transport::Stdio => ferridriver_mcp::mcp::serve_stdio(mode, backend, headless).await,
        cli::Transport::Http => ferridriver_mcp::mcp::serve_http(mode, backend, transport.port, headless).await,
      }
    }
    cli::Command::Test { files, test_args } => {
      run_tests(files, test_args).await
    }
    cli::Command::Bdd { features, bdd_args } => {
      run_bdd(features, bdd_args).await
    }
  }
}

#[allow(clippy::too_many_lines)]
async fn run_bdd(features: Vec<String>, args: cli::BddArgs) -> anyhow::Result<()> {
  use std::sync::Arc;
  use ferridriver_test::config::{CliOverrides, ShardArg};
  use ferridriver_test::runner::TestRunner;
  use ferridriver_bdd::feature::FeatureSet;
  use ferridriver_bdd::filter::TagExpression;
  use ferridriver_bdd::registry::StepRegistry;
  use ferridriver_bdd::scenario;
  use ferridriver_bdd::translate;

  // Resolve config (same config file, same system).
  let overrides = CliOverrides {
    workers: args.workers,
    retries: args.retries,
    reporter: args.reporter.clone(),
    grep: args.grep.clone(),
    grep_invert: args.grep_invert.clone(),
    tag: None, // BDD tag filtering done via tag expression below
    headed: args.headed,
    shard: args
      .shard
      .as_deref()
      .map(ShardArg::parse)
      .transpose()
      .map_err(|e| anyhow::anyhow!(e))?,
    config_path: args.config.clone(),
    output_dir: args.output.clone(),
    test_files: Vec::new(),
    list_only: args.list,
    update_snapshots: false,
    profile: args.profile.clone(),
    forbid_only: args.forbid_only,
    last_failed: args.last_failed,
  };

  let mut config = ferridriver_test::config::resolve_config(&overrides)
    .map_err(|e| anyhow::anyhow!(e))?;

  // Apply BDD-specific CLI overrides.
  if !features.is_empty() {
    config.features = features;
  }
  if config.features.is_empty() {
    config.features = vec!["features/**/*.feature".to_string()];
  }
  if let Some(tags) = &args.tags {
    config.tags = Some(tags.clone());
  }
  if args.dry_run {
    config.dry_run = true;
  }
  if args.fail_fast {
    config.fail_fast = true;
  }
  if let Some(t) = args.step_timeout {
    config.timeout = t;
  }
  if args.strict {
    config.strict = true;
  }
  if let Some(order) = &args.order {
    config.order = order.clone();
  }
  if args.language.is_some() {
    config.language = args.language.clone();
  }

  // Discover and parse .feature files (with optional i18n language).
  let files = FeatureSet::discover(&config.features, &config.test_ignore)
    .map_err(|e| anyhow::anyhow!(e))?;
  let feature_set = FeatureSet::parse_with_language(files, config.language.as_deref())
    .map_err(|e| anyhow::anyhow!(e))?;

  if feature_set.features.is_empty() {
    println!("  No feature files found matching: {:?}", config.features);
    return Ok(());
  }

  // Expand scenarios.
  let mut all_scenarios: Vec<scenario::ScenarioExecution> = feature_set
    .features
    .iter()
    .flat_map(scenario::expand_feature)
    .collect();

  // @only filtering: if any scenario has @only, keep only those.
  let has_only = all_scenarios.iter().any(|s| s.tags.iter().any(|t| t == "@only"));
  if has_only {
    all_scenarios.retain(|s| s.tags.iter().any(|t| t == "@only"));
  }

  // Tag filtering.
  if let Some(tag_expr) = &config.tags {
    let expr = TagExpression::parse(tag_expr)
      .map_err(|e| anyhow::anyhow!("invalid tag expression: {e}"))?;
    ferridriver_bdd::filter::filter_scenarios(&mut all_scenarios, &expr);
  }

  // Grep filtering.
  if let Some(grep) = &args.grep {
    ferridriver_bdd::filter::filter_by_grep(&mut all_scenarios, grep, false);
  }
  if let Some(grep_inv) = &args.grep_invert {
    ferridriver_bdd::filter::filter_by_grep(&mut all_scenarios, grep_inv, true);
  }

  let total = all_scenarios.len();
  if total == 0 {
    println!("  No scenarios matched filters");
    return Ok(());
  }

  // List mode.
  if args.list {
    println!("\n  Scenarios ({total}):\n");
    for s in &all_scenarios {
      let tags = if s.tags.is_empty() {
        String::new()
      } else {
        format!(" {}", s.tags.join(" "))
      };
      println!("  {} -- {}{}", s.location, s.name, tags);
    }
    println!();
    return Ok(());
  }

  // Dry run mode.
  if config.dry_run {
    let registry = StepRegistry::build();
    println!("\n  Dry run -- validating step definitions ({total} scenarios):\n");
    let mut undefined = 0;
    for s in &all_scenarios {
      println!("  Scenario: {}", s.name);
      for step in &s.steps {
        if let Ok(m) = registry.find_match(&step.text) {
          println!("    {} {} -> {}", step.keyword, step.text, m.def.expression);
        } else {
          println!("    {} {} -> UNDEFINED", step.keyword, step.text);
          undefined += 1;
        }
      }
    }
    if undefined > 0 {
      println!("\n  {undefined} undefined step(s)");
      std::process::exit(1);
    }
    println!("\n  All steps defined");
    return Ok(());
  }

  // Build step registry and translate to TestPlan.
  let registry = Arc::new(StepRegistry::build());
  let plan = translate::translate_features(&feature_set, registry, &config);

  // Create reporters -- BDD terminal by default, or from config.
  let reporters = {
    let mut reps: Vec<Box<dyn ferridriver_test::reporter::Reporter>> = Vec::new();
    let mut has_terminal = false;
    for rc in &config.reporter {
      match rc.name.as_str() {
        "terminal" | "bdd" | "default" | "" => {
          if !has_terminal {
            reps.push(Box::new(ferridriver_bdd::reporter::terminal::BddTerminalReporter::new()));
            has_terminal = true;
          }
        }
        "json" => {
          reps.push(Box::new(ferridriver_bdd::reporter::json::BddJsonReporter::new(
            config.output_dir.join("bdd-results.json"),
          )));
        }
        "junit" => {
          reps.push(Box::new(ferridriver_bdd::reporter::junit::BddJunitReporter::new(
            config.output_dir.join("bdd-junit.xml"),
          )));
        }
        "cucumber-json" | "cucumber" => {
          reps.push(Box::new(
            ferridriver_bdd::reporter::cucumber_json::CucumberJsonReporter::new(
              config.output_dir.join("cucumber.json"),
            ),
          ));
        }
        "usage" => {
          reps.push(Box::new(ferridriver_bdd::reporter::usage::UsageReporter::new()));
        }
        "rerun" => {
          reps.push(Box::new(ferridriver_bdd::reporter::rerun::BddRerunReporter::new(
            config.output_dir.join("@rerun.txt"),
          )));
        }
        "messages" | "ndjson" => {
          reps.push(Box::new(ferridriver_bdd::reporter::messages::CucumberMessagesReporter::new(
            config.output_dir.join("cucumber-messages.ndjson"),
          )));
        }
        "progress" => {
          reps.push(Box::new(ferridriver_test::reporter::progress::ProgressReporter::new()));
        }
        "html" => {
          reps.push(Box::new(ferridriver_test::reporter::html::HtmlReporter::new(
            config.output_dir.join("report.html"),
          )));
        }
        other => tracing::warn!("unknown reporter: {other}"),
      }
    }
    if reps.is_empty() {
      reps.push(Box::new(ferridriver_bdd::reporter::terminal::BddTerminalReporter::new()));
    }
    // Always add the rerun reporter so @rerun.txt is available for --last-failed.
    let has_rerun = config.reporter.iter().any(|r| r.name == "rerun");
    if !has_rerun {
      reps.push(Box::new(ferridriver_bdd::reporter::rerun::BddRerunReporter::new(
        config.output_dir.join("@rerun.txt"),
      )));
    }
    ferridriver_test::reporter::ReporterSet::new(reps)
  };

  // Run via core TestRunner.
  let mut runner = TestRunner::new(config, reporters, overrides);
  let exit_code = runner.run(plan).await;

  std::process::exit(exit_code);
}

async fn run_tests(files: Vec<String>, args: cli::TestArgs) -> anyhow::Result<()> {
  use ferridriver_test::{
    config::{CliOverrides, ShardArg},
    discovery::collect_rust_tests,
    reporter::create_reporters,
    runner::TestRunner,
  };

  let overrides = CliOverrides {
    workers: args.workers,
    retries: args.retries,
    reporter: args.reporter,
    grep: args.grep,
    grep_invert: args.grep_invert,
    tag: args.tag,
    headed: args.headed,
    shard: args
      .shard
      .as_deref()
      .map(ShardArg::parse)
      .transpose()
      .map_err(|e| anyhow::anyhow!(e))?,
    config_path: args.config,
    output_dir: args.output,
    test_files: files,
    list_only: args.list,
    update_snapshots: false,
    profile: args.profile,
    forbid_only: args.forbid_only,
    last_failed: args.last_failed,
  };

  let config = ferridriver_test::config::resolve_config(&overrides).map_err(|e| anyhow::anyhow!(e))?;
  let reporters = create_reporters(&config.reporter, &config.output_dir);
  let plan = collect_rust_tests(&config);

  let mut runner = TestRunner::new(config, reporters, overrides);
  let exit_code = runner.run(plan).await;

  std::process::exit(exit_code);
}
