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
    },
  }
}
