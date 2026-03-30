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

      match transport.transport {
        cli::Transport::Stdio => ferridriver_mcp::mcp::serve_stdio(mode, backend).await,
        cli::Transport::Http => ferridriver_mcp::mcp::serve_http(mode, backend, transport.port).await,
      }
    }
    cli::Command::Test { files, test_args } => {
      run_tests(files, test_args).await
    }
  }
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
  };

  let config = ferridriver_test::config::resolve_config(&overrides).map_err(|e| anyhow::anyhow!(e))?;
  let reporters = create_reporters(&config.reporter, &config.output_dir);
  let plan = collect_rust_tests(&config);

  let mut runner = TestRunner::new(config, reporters, overrides);
  let exit_code = runner.run(plan).await;

  std::process::exit(exit_code);
}
