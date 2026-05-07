#![allow(clippy::doc_markdown)]
//! ferridriver -- single-binary CLI for browser automation.
//!
//! Subcommands: `mcp`, `bdd`, `test`, `codegen`, `config`.
//!
//! The unified `FerridriverConfig` is loaded exactly once per invocation and
//! its sections are passed to the selected subcommand.

// mimalloc as the global allocator. ~10–20% faster than system malloc
// on small thread-local allocs (the dominant per-RTT pattern in CDP dispatch).
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod cli;

use std::sync::Arc;

use clap::Parser;
use ferridriver_config::FerridriverConfig;
use ferridriver_mcp::McpServer;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
  let args = cli::Cli::parse();

  let filter = match args.verbose {
    0 => "warn",
    1 => "info,ferridriver=debug",
    _ => "trace",
  };
  tracing_subscriber::fmt()
    .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| filter.into()))
    .init();

  // Load the unified config exactly once. Each subcommand reads the
  // section it cares about from this single document.
  let config = FerridriverConfig::load(args.config.as_deref())?;

  match args.command {
    cli::Command::Mcp(mcp_args) => Box::pin(run_mcp(config, mcp_args)).await,
    cli::Command::Bdd(bdd_args) => Box::pin(run_bdd(config, bdd_args)).await,
    cli::Command::Test(test_args) => run_test(&test_args),
    cli::Command::Codegen(_) => anyhow::bail!("`codegen` subcommand not yet implemented"),
    cli::Command::Config(_) => anyhow::bail!("`config` subcommand not yet implemented"),
  }
}

fn run_test(args: &cli::TestArgs) -> anyhow::Result<()> {
  use std::process::{Command, Stdio};

  let chosen_runner = args.runner.unwrap_or(detect_test_runner());

  let (program, base_args): (&str, Vec<String>) = match chosen_runner {
    cli::TestRunner::Nextest => {
      let mut a = vec!["nextest".into(), "run".into()];
      if let Some(profile) = args.profile.as_deref() {
        a.push("--profile".into());
        a.push(profile.to_string());
      }
      ("cargo", a)
    },
    cli::TestRunner::Cargo => ("cargo", vec!["test".into()]),
  };

  let mut cmd = Command::new(program);
  cmd.args(&base_args);
  for pkg in &args.packages {
    cmd.arg("-p").arg(pkg);
  }
  if let Some(filter) = args.filter.as_deref() {
    // For nextest, filter is a positional. For cargo test, filter is also positional.
    cmd.arg(filter);
  }
  if !args.passthrough.is_empty() {
    cmd.arg("--");
    for arg in &args.passthrough {
      cmd.arg(arg);
    }
  }

  cmd
    .stdout(Stdio::inherit())
    .stderr(Stdio::inherit())
    .stdin(Stdio::inherit());

  tracing::info!(
    runner = ?chosen_runner_name(chosen_runner),
    args = ?cmd.get_args().collect::<Vec<_>>(),
    "running cargo tests"
  );

  let status = cmd
    .status()
    .map_err(|e| anyhow::anyhow!("failed to spawn `{program}`: {e}"))?;
  if status.success() {
    Ok(())
  } else {
    std::process::exit(status.code().unwrap_or(1));
  }
}

fn detect_test_runner() -> cli::TestRunner {
  // Probe for nextest availability with `cargo nextest --version`. Cheap (~5ms).
  let probe = std::process::Command::new("cargo")
    .args(["nextest", "--version"])
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .status();
  match probe {
    Ok(s) if s.success() => cli::TestRunner::Nextest,
    _ => cli::TestRunner::Cargo,
  }
}

fn chosen_runner_name(r: cli::TestRunner) -> &'static str {
  match r {
    cli::TestRunner::Nextest => "nextest",
    cli::TestRunner::Cargo => "cargo",
  }
}

async fn run_bdd(config: FerridriverConfig, args: cli::BddArgs) -> anyhow::Result<()> {
  let mut overrides = ferridriver_test::config::CliOverrides {
    bdd_tags: args.tags,
    bdd_dry_run: args.dry_run,
    bdd_fail_fast: args.fail_fast,
    bdd_strict: args.strict,
    bdd_step_timeout: args.step_timeout,
    bdd_order: args.order,
    bdd_language: args.language,
    workers: args.workers.map(|n| u32::try_from(n).unwrap_or(u32::MAX)),
    reporter: args.reporter,
    ..Default::default()
  };
  // `--headless` opts into headless. Default config is headed, so leaving
  // the flag unset means visible windows -- matching the new CLI
  // convention where the user watches tests run by default.
  if args.browser.headless {
    overrides.headless = true;
  }
  // Likewise, only override backend / executable_path when the user supplied
  // a non-default value. clap fills in defaults for `--backend`, so use the
  // raw arg presence by checking the user-relevant flags.
  if !matches!(args.browser.backend, cli::Backend::CdpPipe) {
    overrides.backend = match args.browser.backend {
      cli::Backend::CdpPipe => Some("cdp-pipe".into()),
      cli::Backend::CdpRaw => Some("cdp-raw".into()),
      #[cfg(target_os = "macos")]
      cli::Backend::Webkit => Some("webkit".into()),
      cli::Backend::Bidi => Some("bidi".into()),
    };
  }
  overrides.executable_path = args.browser.executable_path;

  let mut test_config = ferridriver_test::config::resolve_config_from(config.test, &overrides)
    .map_err(|e| anyhow::anyhow!("config error: {e}"))?;

  // CLI-supplied feature globs override the [test].features list when provided.
  if !args.features.is_empty() {
    test_config.features = args.features;
  }

  let exit_code = Box::pin(ferridriver_bdd::run_bdd_with(test_config, overrides)).await;
  if exit_code == 0 {
    Ok(())
  } else {
    std::process::exit(exit_code);
  }
}

async fn run_mcp(config: FerridriverConfig, args: cli::McpArgs) -> anyhow::Result<()> {
  // The mcp section drives chrome args, instances, and server metadata.
  // CLI flags fall back when the [mcp] section is empty so the user can
  // launch the server with no config file at all.
  let mcp = config.mcp;
  let backend = if mcp.browser.backend.is_some() {
    mcp.backend_kind()
  } else {
    args.browser.backend_kind()
  };
  let headless = if mcp.browser.headless.is_some() {
    mcp.headless()
  } else {
    args.browser.headless
  };
  let connect_mode = args.browser.connect_mode();

  let server = McpServer::with_options(connect_mode, backend, headless, Arc::new(mcp));
  match args.transport.transport {
    cli::Transport::Stdio => ferridriver_mcp::mcp::serve_stdio_with(server).await,
    cli::Transport::Http => ferridriver_mcp::mcp::serve_http_with(server, args.transport.port).await,
  }
}
