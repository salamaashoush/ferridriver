#![allow(clippy::doc_markdown)]
//! ferridriver -- MCP server for browser automation.
//!
//! Test running is handled by the TS CLI (`ferridriver-test`) or Rust
//! macros (`main!()`, `bdd_main!()`) via `cargo test`.

mod cli;

use std::sync::Arc;

use clap::Parser;
use ferridriver::state::ConnectMode;
use ferridriver_mcp::McpServer;
use ferridriver_mcp::config::FileConfig;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
  let cli = cli::Cli::parse();

  // Centralized tracing setup -- respects RUST_LOG, FERRIDRIVER_DEBUG, and --verbose.
  let filter = match cli.verbose {
    0 => "warn",
    1 => "info,ferridriver=debug",
    _ => "trace",
  };
  tracing_subscriber::fmt()
    .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| filter.into()))
    .init();

  // Try config-driven mode first, then fall back to CLI args.
  let file_config = if let Some(ref path) = cli.config {
    Some(FileConfig::load(path)?)
  } else {
    FileConfig::load_with_search()
  };

  if let Some(config) = file_config {
    let backend = config.backend_kind();
    let headless = config.headless();
    let server = McpServer::with_options(ConnectMode::Launch, backend, headless, Arc::new(config));
    match cli.transport.transport {
      cli::Transport::Stdio => ferridriver_mcp::mcp::serve_stdio_with(server).await,
      cli::Transport::Http => ferridriver_mcp::mcp::serve_http_with(server, cli.transport.port).await,
    }
  } else {
    // Default: no config, use CLI args directly
    let backend = cli.browser.backend_kind();
    let mode = cli.browser.connect_mode();
    let headless = cli.browser.headless;

    match cli.transport.transport {
      cli::Transport::Stdio => ferridriver_mcp::mcp::serve_stdio(mode, backend, headless).await,
      cli::Transport::Http => ferridriver_mcp::mcp::serve_http(mode, backend, cli.transport.port, headless).await,
    }
  }
}
