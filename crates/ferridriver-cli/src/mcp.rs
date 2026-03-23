//! MCP server transport wiring.

use crate::cli::{BrowserArgs, Transport, TransportArgs};
use crate::server::McpServer;
use ferridriver::backend::BackendKind;
use ferridriver::state::ConnectMode;
use rmcp::ServiceExt;

pub async fn run(browser: BrowserArgs, transport: TransportArgs) -> anyhow::Result<()> {
    let backend = browser.backend_kind();
    let mode = browser.connect_mode();

    match transport.transport {
        Transport::Stdio => serve_stdio(mode, backend).await,
        Transport::Http => serve_http(mode, backend, transport.port).await,
    }
}

async fn serve_stdio(mode: ConnectMode, backend: BackendKind) -> anyhow::Result<()> {
    let svc = McpServer::new(mode, backend)
        .serve(rmcp::transport::io::stdio())
        .await?;
    svc.waiting().await?;
    Ok(())
}

async fn serve_http(mode: ConnectMode, backend: BackendKind, port: u16) -> anyhow::Result<()> {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService,
        session::local::LocalSessionManager,
    };
    use std::sync::Arc;

    let ct = tokio_util::sync::CancellationToken::new();
    let config = StreamableHttpServerConfig {
        stateful_mode: true,
        cancellation_token: ct.child_token(),
        ..Default::default()
    };

    let svc = StreamableHttpService::new(
        move || Ok(McpServer::new(mode.clone(), backend.clone())),
        Arc::new(LocalSessionManager::default()),
        config,
    );

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    eprintln!("ferridriver listening on http://0.0.0.0:{port}/mcp");

    axum::serve(listener, axum::Router::new().nest_service("/mcp", svc))
        .with_graceful_shutdown(async move { ct.cancelled_owned().await })
        .await?;

    Ok(())
}
