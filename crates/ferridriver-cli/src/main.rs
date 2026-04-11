#![allow(
  clippy::too_many_lines,
  clippy::doc_markdown,
  clippy::uninlined_format_args,
  clippy::struct_excessive_bools,
  clippy::cast_possible_truncation,
  clippy::cast_sign_loss,
  clippy::unused_async
)]
//! ferridriver -- High-performance browser automation CLI.

mod cli;

use clap::Parser;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
  let cli = cli::Cli::parse();

  // Centralized tracing setup — respects RUST_LOG, FERRIDRIVER_DEBUG, and --verbose.
  ferridriver_test::logging::init(cli.verbose);

  match cli.command {
    cli::Command::Mcp { browser, transport } => {
      let backend = browser.backend_kind();
      let mode = browser.connect_mode();
      let headless = browser.headless;

      match transport.transport {
        cli::Transport::Stdio => ferridriver_mcp::mcp::serve_stdio(mode, backend, headless).await,
        cli::Transport::Http => ferridriver_mcp::mcp::serve_http(mode, backend, transport.port, headless).await,
      }
    },
    cli::Command::Install { browser, with_deps } => install_browser(&browser, with_deps).await,
    cli::Command::Test { files, common } => run_tests(files, common).await,
    cli::Command::Bdd { features, common, bdd } => run_bdd(features, common, bdd).await,
    cli::Command::Codegen {
      url,
      language,
      output,
      viewport,
    } => {
      let vp = viewport.and_then(|s| {
        let parts: Vec<&str> = s.split('x').collect();
        if parts.len() == 2 {
          Some((parts[0].parse::<u32>().ok()?, parts[1].parse::<u32>().ok()?))
        } else {
          None
        }
      });
      let options = ferridriver::codegen::recorder::RecorderOptions {
        url,
        language: ferridriver::codegen::OutputLanguage::from_str(&language),
        output_file: output,
        viewport: vp,
      };
      ferridriver::codegen::recorder::Recorder::new(options)
        .start()
        .await
        .map_err(|e| anyhow::anyhow!(e))
    },
  }
}

#[allow(clippy::too_many_lines)]
async fn run_bdd(features: Vec<String>, args: cli::CommonRunArgs, bdd: cli::BddOnlyArgs) -> anyhow::Result<()> {
  use ferridriver_bdd::feature::FeatureSet;
  use ferridriver_bdd::filter::TagExpression;
  use ferridriver_bdd::registry::StepRegistry;
  use ferridriver_bdd::scenario;
  use ferridriver_bdd::translate;
  use ferridriver_test::runner::TestRunner;
  use std::sync::Arc;

  let overrides = args.to_overrides().map_err(|e| anyhow::anyhow!(e))?;
  let mut config = resolve_and_apply_common(&args, &overrides)?;

  // Apply BDD-specific CLI overrides.
  if !features.is_empty() {
    config.features = features;
  }
  if config.features.is_empty() {
    config.features = vec!["features/**/*.feature".to_string()];
  }
  if let Some(tags) = &bdd.tags {
    config.tags = Some(tags.clone());
  }
  if bdd.dry_run {
    config.dry_run = true;
  }
  if bdd.fail_fast {
    config.fail_fast = true;
  }
  if let Some(t) = bdd.step_timeout {
    config.timeout = t;
  }
  if bdd.strict {
    config.strict = true;
  }
  if let Some(order) = &bdd.order {
    config.order = order.clone();
  }
  if bdd.language.is_some() {
    config.language = bdd.language.clone();
  }

  // Discover and parse .feature files (with optional i18n language).
  let files = FeatureSet::discover(&config.features, &config.test_ignore).map_err(|e| anyhow::anyhow!(e))?;
  let feature_set =
    FeatureSet::parse_with_language(files, config.language.as_deref()).map_err(|e| anyhow::anyhow!(e))?;

  if feature_set.features.is_empty() {
    println!("  No feature files found matching: {:?}", config.features);
    return Ok(());
  }

  // Expand scenarios.
  let mut all_scenarios: Vec<scenario::ScenarioExecution> =
    feature_set.features.iter().flat_map(scenario::expand_feature).collect();

  // @only filtering is handled by the runner's execute() via filter_by_only().
  // Scenarios with @only get TestAnnotation::Only in the translate step.

  // Tag filtering (BDD-specific Gherkin expressions like "@smoke and not @wip").
  if let Some(tag_expr) = &config.tags {
    let expr = TagExpression::parse(tag_expr).map_err(|e| anyhow::anyhow!("invalid tag expression: {e}"))?;
    ferridriver_bdd::filter::filter_scenarios(&mut all_scenarios, &expr);
  }

  // Grep filtering is handled by the runner's execute() via CliOverrides.grep.
  // No duplicate filtering here — the runner applies it to the TestPlan.

  let total = all_scenarios.len();
  if total == 0 {
    println!("  No scenarios matched filters");
    return Ok(());
  }

  // List mode.
  if overrides.list_only {
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

  // Run via core TestRunner.
  config.mode = ferridriver_test::config::RunMode::Bdd;

  if args.watch {
    let features_patterns = config.features.clone();
    let test_ignore = config.test_ignore.clone();
    let language = config.language.clone();
    let config_for_translate = config.clone();
    let registry_clone = Arc::clone(&registry);
    let mut runner = TestRunner::new(config, overrides);
    let cwd = std::env::current_dir().unwrap_or_default();
    let exit_code = runner
      .run_watch(
        move |changed_files: Option<&[::std::path::PathBuf]>| {
          // When specific files changed, only discover/parse those.
          // Otherwise (None = full run), discover all features.
          let files = if let Some(changed) = changed_files {
            // Filter to only .feature files from the changed set.
            let feature_files: Vec<std::path::PathBuf> = changed
              .iter()
              .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("feature"))
              .cloned()
              .collect();
            if feature_files.is_empty() {
              // Changed files were not features (e.g., step files) — re-discover all.
              match FeatureSet::discover(&features_patterns, &test_ignore) {
                Ok(f) => f,
                Err(e) => {
                  eprintln!("Feature discovery error: {e}");
                  return empty_plan();
                },
              }
            } else {
              feature_files
            }
          } else {
            match FeatureSet::discover(&features_patterns, &test_ignore) {
              Ok(f) => f,
              Err(e) => {
                eprintln!("Feature discovery error: {e}");
                return empty_plan();
              },
            }
          };

          let fset = match FeatureSet::parse_with_language(files, language.as_deref()) {
            Ok(f) => f,
            Err(e) => {
              eprintln!("Feature parse error: {e}");
              return empty_plan();
            },
          };
          translate::translate_features(&fset, Arc::clone(&registry_clone), &config_for_translate)
        },
        cwd,
      )
      .await;
    std::process::exit(exit_code);
  }

  let plan = translate::translate_features(&feature_set, registry, &config);
  let mut runner = TestRunner::new(config, overrides);
  let exit_code = runner.run(plan).await;

  std::process::exit(exit_code);
}

async fn run_tests(files: Vec<String>, args: cli::CommonRunArgs) -> anyhow::Result<()> {
  use ferridriver_test::{discovery::collect_rust_tests, runner::TestRunner};

  let mut overrides = args.to_overrides().map_err(|e| anyhow::anyhow!(e))?;
  overrides.test_files = files;
  let config = resolve_and_apply_common(&args, &overrides)?;

  if args.watch {
    let config_clone = config.clone();
    let mut runner = TestRunner::new(config, overrides);
    let cwd = std::env::current_dir().unwrap_or_default();
    let exit_code = runner
      .run_watch(
        move |_changed: Option<&[std::path::PathBuf]>| collect_rust_tests(&config_clone),
        cwd,
      )
      .await;
    std::process::exit(exit_code);
  }

  let plan = collect_rust_tests(&config);
  let mut runner = TestRunner::new(config, overrides);
  let exit_code = runner.run(plan).await;

  std::process::exit(exit_code);
}

/// Shared config resolution: resolve config file, apply common CLI overrides, merge webServer configs.
fn resolve_and_apply_common(
  args: &cli::CommonRunArgs,
  overrides: &ferridriver_test::config::CliOverrides,
) -> anyhow::Result<ferridriver_test::config::TestConfig> {
  let mut config = ferridriver_test::config::resolve_config(overrides).map_err(|e| anyhow::anyhow!(e))?;

  if let Some(backend) = &args.backend {
    config.browser.backend = cli::backend_to_string(backend);
  }
  if let Some(ref browser) = args.browser {
    cli::apply_browser_defaults(&mut config.browser, browser);
  }

  // Merge CLI webServer flags into config (CLI takes precedence).
  let cli_servers = args.web_server_configs();
  if !cli_servers.is_empty() {
    config.web_server = cli_servers;
  }

  Ok(config)
}

async fn install_browser(browser: &str, with_deps: bool) -> anyhow::Result<()> {
  use ferridriver::install::{BrowserInstaller, InstallProgress};

  let installer = BrowserInstaller::new();
  println!("Browser cache: {}", installer.cache_dir().display());

  // Install system dependencies first if requested (chromium only for now)
  if with_deps && matches!(browser, "chromium" | "chrome") {
    installer
      .install_system_deps(|p| match p {
        InstallProgress::InstallingDeps { distro } => {
          println!("Installing system dependencies for {distro}...");
        },
        InstallProgress::DepsInstalled => {
          println!("System dependencies installed.");
        },
        _ => {},
      })
      .await
      .map_err(|e| anyhow::anyhow!(e))?;
  }

  let browser_label = match browser {
    "chromium" | "chrome" => "Chromium",
    "firefox" => "Firefox",
    other => anyhow::bail!("unsupported browser: {other}. Supported: chromium, firefox."),
  };

  let progress_fn = |p: InstallProgress| match p {
    InstallProgress::Resolving => {
      println!("Resolving latest stable {browser_label}...");
    },
    InstallProgress::Downloading {
      bytes_downloaded,
      total_bytes,
    } => {
      if let Some(total) = total_bytes {
        let pct = (bytes_downloaded as f64 / total as f64 * 100.0) as u32;
        let mb = bytes_downloaded as f64 / 1_048_576.0;
        let total_mb = total as f64 / 1_048_576.0;
        eprint!("\rDownloading... {mb:.1}/{total_mb:.1} MB ({pct}%)");
      } else {
        let mb = bytes_downloaded as f64 / 1_048_576.0;
        eprint!("\rDownloading... {mb:.1} MB");
      }
    },
    InstallProgress::Extracting => {
      eprintln!();
      println!("Extracting...");
    },
    InstallProgress::Complete { version, path } => {
      println!("{browser_label} {version} installed: {path}");
    },
    InstallProgress::AlreadyInstalled { version, path } => {
      println!("{browser_label} {version} already installed: {path}");
    },
    _ => {},
  };

  let path = match browser {
    "chromium" | "chrome" => installer
      .install_chromium(&progress_fn)
      .await
      .map_err(|e| anyhow::anyhow!(e))?,
    "firefox" => installer
      .install_firefox(&progress_fn)
      .await
      .map_err(|e| anyhow::anyhow!(e))?,
    _ => unreachable!(),
  };

  // Verify it works
  let output = std::process::Command::new(&path).arg("--version").output();
  match output {
    Ok(o) if o.status.success() => {
      let ver = String::from_utf8_lossy(&o.stdout);
      println!("Verified: {}", ver.trim());
    },
    _ => {
      eprintln!("Warning: could not verify browser at {path}");
    },
  }

  Ok(())
}

fn empty_plan() -> ferridriver_test::model::TestPlan {
  ferridriver_test::model::TestPlan {
    suites: Vec::new(),
    total_tests: 0,
    shard: None,
  }
}
