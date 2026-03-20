//! chromey-mcp — High-performance browser automation MCP server.
//!
//! Transports:
//!   stdio  — Standard MCP stdio (default, for Claude Code / CLI)
//!   http   — Streamable HTTP + SSE (for remote clients, web UIs, multi-session)

mod params;
mod scenario;
mod server;
mod snapshot;
mod state;
#[macro_use]
mod steps;
mod tools;

use server::ChromeyMcp;
use rmcp::ServiceExt;
use tracing_subscriber::{self, EnvFilter};

fn parse_arg(args: &[String], flag: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone())
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env().add_directive(tracing::Level::WARN.into()),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let args: Vec<String> = std::env::args().collect();

    if has_flag(&args, "--help") || has_flag(&args, "-h") {
        eprintln!("chromey-mcp — High-performance browser automation MCP server");
        eprintln!();
        eprintln!("Usage: chromey-mcp [options]");
        eprintln!();
        eprintln!("Transport:");
        eprintln!("  --transport stdio     Standard IO transport (default)");
        eprintln!("  --transport http      Streamable HTTP + SSE transport");
        eprintln!("  --port <port>         HTTP port (default: 8080)");
        eprintln!();
        eprintln!("Connection modes:");
        eprintln!("  (none)                Launch a new headless browser (default)");
        eprintln!("  --autoConnect         Connect to running Chrome");
        eprintln!("  --connect <url>       Connect via ws:// or http:// URL");
        eprintln!();
        eprintln!("Options:");
        eprintln!("  --channel <ch>        Chrome channel: stable, beta, dev, canary");
        eprintln!("  --user-data-dir <dir> Chrome user data directory");
        eprintln!();
        eprintln!("Environment:");
        eprintln!("  CHROMIUM_PATH         Path to chromium/chrome binary");
        std::process::exit(0);
    }

    let mode = if has_flag(&args, "--autoConnect") {
        state::ConnectMode::AutoConnect {
            channel: parse_arg(&args, "--channel").unwrap_or_else(|| "stable".into()),
            user_data_dir: parse_arg(&args, "--user-data-dir"),
        }
    } else if let Some(url) = parse_arg(&args, "--connect") {
        state::ConnectMode::ConnectUrl(url)
    } else {
        state::ConnectMode::Launch
    };

    let transport = parse_arg(&args, "--transport").unwrap_or_else(|| "stdio".into());

    match transport.as_str() {
        "stdio" => run_stdio(mode).await,
        "http" => {
            let port: u16 = parse_arg(&args, "--port")
                .and_then(|p| p.parse().ok())
                .unwrap_or(8080);
            run_http(mode, port).await
        }
        other => anyhow::bail!("Unknown transport '{other}'. Use 'stdio' or 'http'."),
    }
}

async fn run_stdio(mode: state::ConnectMode) -> anyhow::Result<()> {
    let server = ChromeyMcp::new(mode);
    let service = server.serve(rmcp::transport::io::stdio()).await?;
    service.waiting().await?;
    Ok(())
}

async fn run_http(mode: state::ConnectMode, port: u16) -> anyhow::Result<()> {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService,
    };
    use std::sync::Arc;

    let ct = tokio_util::sync::CancellationToken::new();
    let config = StreamableHttpServerConfig {
        stateful_mode: true,
        cancellation_token: ct.child_token(),
        ..Default::default()
    };

    use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;

    let session_manager = Arc::new(LocalSessionManager::default());
    let service = StreamableHttpService::new(
        move || Ok(ChromeyMcp::new(mode.clone())),
        session_manager,
        config,
    );

    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    eprintln!("chromey-mcp HTTP server listening on http://0.0.0.0:{port}/mcp");

    axum::serve(listener, router)
        .with_graceful_shutdown(async move { ct.cancelled_owned().await })
        .await?;

    Ok(())
}
