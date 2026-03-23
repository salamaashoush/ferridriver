//! ferridriver -- High-performance browser automation.

mod cli;
mod mcp;
mod params;
mod server;
mod tools;

use clap::Parser;
use tracing_subscriber::{self, EnvFilter};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    if ferridriver::backend::webkit::ipc::is_webkit_host() {
        ferridriver::backend::webkit::ipc::run_webkit_host();
    }

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(tracing::Level::WARN.into()))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let cli = cli::Cli::parse();

    match cli.command {
        cli::Command::Mcp { browser, transport } => mcp::run(browser, transport).await,
    }
}
