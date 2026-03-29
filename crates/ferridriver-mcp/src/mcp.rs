//! MCP server transport wiring.
//!
//! Provides ready-made functions to serve an `McpServer` over stdio or HTTP.

use crate::server::McpServer;
use ferridriver::backend::BackendKind;
use ferridriver::state::ConnectMode;
use rmcp::ServiceExt;
use std::sync::Arc;

/// Serve a default `McpServer` over stdio (for Claude Code, CLI clients).
///
/// # Errors
///
/// Returns an error if the MCP transport fails to initialize or the server
/// encounters a fatal communication error.
pub async fn serve_stdio(mode: ConnectMode, backend: BackendKind) -> anyhow::Result<()> {
    let svc = Box::pin(McpServer::new(mode, backend)
        .serve(rmcp::transport::io::stdio()))
        .await?;
    svc.waiting().await?;
    Ok(())
}

/// Serve a default `McpServer` over HTTP (for remote clients, web UIs).
///
/// # Errors
///
/// Returns an error if the TCP listener cannot bind to the requested port,
/// or if the HTTP server encounters a fatal error.
pub async fn serve_http(mode: ConnectMode, backend: BackendKind, port: u16) -> anyhow::Result<()> {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService,
        session::local::LocalSessionManager,
    };

    let ct = tokio_util::sync::CancellationToken::new();
    let config = StreamableHttpServerConfig {
        stateful_mode: true,
        cancellation_token: ct.child_token(),
        ..Default::default()
    };

    let svc = StreamableHttpService::new(
        move || Ok(McpServer::new(mode.clone(), backend)),
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

/// Serve a custom `McpServer` (with config/extensions) over stdio.
///
/// # Errors
///
/// Returns an error if the MCP transport fails to initialize or the server
/// encounters a fatal communication error.
pub async fn serve_stdio_with(server: McpServer) -> anyhow::Result<()> {
    let svc = Box::pin(server
        .serve(rmcp::transport::io::stdio()))
        .await?;
    svc.waiting().await?;
    Ok(())
}
