//! MCP server transport wiring.
//!
//! Provides ready-made functions to serve an `McpServer` over stdio or HTTP.

use crate::server::McpServer;
use ferridriver::backend::BackendKind;
use ferridriver::state::ConnectMode;
use rmcp::ServiceExt;
use std::sync::Arc;

/// How long a shutdown may spend killing browsers before the process
/// gives up and exits anyway. Teardown is a `killpg` per browser, so
/// this only bounds a pathological case (an unresponsive `WebKit` child
/// polled for its graceful exit).
const SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Resolve when the process is asked to terminate.
///
/// Without this the server dies on the signal with no teardown, and any
/// browser whose transport is not a pipe (`cdp-raw` Chrome, `bidi`
/// Firefox) is reparented to pid 1 and runs forever — headed, that is a
/// macOS dock tile with no window and no owner.
#[cfg(unix)]
async fn terminate_signal() {
  use tokio::signal::unix::{SignalKind, signal};
  let mut term = match signal(SignalKind::terminate()) {
    Ok(s) => s,
    Err(e) => {
      tracing::warn!(error = %e, "cannot install SIGTERM handler; browsers may outlive this process");
      return std::future::pending().await;
    },
  };
  let mut hup = match signal(SignalKind::hangup()) {
    Ok(s) => s,
    Err(e) => {
      tracing::warn!(error = %e, "cannot install SIGHUP handler");
      return std::future::pending().await;
    },
  };
  let signal_name = tokio::select! {
    _ = term.recv() => "SIGTERM",
    _ = hup.recv() => "SIGHUP",
    r = tokio::signal::ctrl_c() => {
      if let Err(e) = r {
        tracing::warn!(error = %e, "ctrl_c listener failed");
        return std::future::pending().await;
      }
      "SIGINT"
    },
  };
  tracing::info!(signal = signal_name, "shutting down; closing browsers");
}

#[cfg(not(unix))]
async fn terminate_signal() {
  if tokio::signal::ctrl_c().await.is_err() {
    std::future::pending().await
  }
}

/// Reclaim browsers leaked by ferridriver processes that died without
/// teardown. Runs once per server start, off the async worker (it stats
/// the registry and may shell out to `ps`). The CLI sweeps on every
/// subcommand; this covers embedders that build an `McpServer` directly.
async fn reclaim_leaked_browsers() {
  let reclaimed = tokio::task::spawn_blocking(ferridriver::backend::process::sweep_stale_browsers)
    .await
    .unwrap_or(0);
  if reclaimed > 0 {
    tracing::info!(count = reclaimed, "reclaimed browsers leaked by earlier runs");
  }
}

/// Run `server` until its transport ends or the process is signalled,
/// then close every browser it launched.
async fn serve_until_shutdown<S>(server: &McpServer, serve: S) -> anyhow::Result<()>
where
  S: std::future::Future<Output = anyhow::Result<()>>,
{
  let (outcome, signalled) = {
    let serve = std::pin::pin!(serve);
    let signal = std::pin::pin!(terminate_signal());
    tokio::select! {
      r = serve => (r, false),
      () = signal => (Ok(()), true),
    }
  };
  if tokio::time::timeout(SHUTDOWN_TIMEOUT, server.shutdown_browsers())
    .await
    .is_err()
  {
    tracing::warn!("browser shutdown timed out; leaving reclamation to the next start");
  }
  if signalled {
    // Installing a handler took over the signal's default "terminate
    // now", and unwinding back through main does not get us there:
    // dropping the runtime waits on the blocking stdin read of the
    // stdio transport, which only returns when the client closes the
    // pipe. The browsers are already down, so exit deliberately rather
    // than sit there until someone sends SIGKILL.
    std::process::exit(0);
  }
  outcome
}

/// Serve a default `McpServer` over stdio (for Claude Code, CLI clients).
///
/// # Errors
///
/// Returns an error if the MCP transport fails to initialize or the server
/// encounters a fatal communication error.
pub async fn serve_stdio(mode: ConnectMode, backend: BackendKind, headless: bool) -> anyhow::Result<()> {
  Box::pin(serve_stdio_with(McpServer::new_headless(mode, backend, headless))).await
}

/// Serve a default `McpServer` over HTTP (for remote clients, web UIs).
///
/// # Errors
///
/// Returns an error if the TCP listener cannot bind to the requested port,
/// or if the HTTP server encounters a fatal error.
pub async fn serve_http(mode: ConnectMode, backend: BackendKind, port: u16, headless: bool) -> anyhow::Result<()> {
  // One server shared by every HTTP session, like `serve_http_with`.
  // Minting a fresh `McpServer` per session gave each one its own
  // browser state that nothing ever shut down, so a client that
  // reconnected left a browser behind on every session.
  Box::pin(serve_http_with(McpServer::new_headless(mode, backend, headless), port)).await
}

/// Serve a custom `McpServer` (with config/extensions) over stdio.
///
/// # Errors
///
/// Returns an error if the MCP transport fails to initialize or the server
/// encounters a fatal communication error.
pub async fn serve_stdio_with(server: McpServer) -> anyhow::Result<()> {
  reclaim_leaked_browsers().await;
  let handle = server.clone();
  let svc = Box::pin(server.serve(rmcp::transport::io::stdio())).await?;
  serve_until_shutdown(&handle, async move {
    svc.waiting().await?;
    Ok(())
  })
  .await
}

/// Serve a custom `McpServer` (with config/extensions) over HTTP.
///
/// # Errors
///
/// Returns an error if the TCP listener cannot bind to the requested port,
/// or if the HTTP server encounters a fatal error.
pub async fn serve_http_with(server: McpServer, port: u16) -> anyhow::Result<()> {
  use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
  };

  reclaim_leaked_browsers().await;
  let handle = server.clone();
  let ct = tokio_util::sync::CancellationToken::new();
  // The 3.0 rename of `stateful_mode`, and it only governs peers older than
  // 2026-07-28 — SEP-2567 removed sessions, so a peer that negotiates
  // 2026-07-28 is served statelessly whatever this says. Those older peers
  // keep their session and standalone GET stream because the server pushes
  // messages at them: progress during navigate/run_script/run_bdd, and
  // `tools/list_changed` when an extension reloads.
  let config = StreamableHttpServerConfig::default()
    .with_cancellation_token(ct.child_token())
    .with_legacy_session_mode(true);

  let svc = StreamableHttpService::new(
    move || Ok(server.clone()),
    Arc::new(LocalSessionManager::default()),
    config,
  );

  let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
  eprintln!("ferridriver listening on http://0.0.0.0:{port}/mcp");

  serve_until_shutdown(&handle, async move {
    axum::serve(listener, axum::Router::new().nest_service("/mcp", svc))
      .with_graceful_shutdown(async move { ct.cancelled_owned().await })
      .await?;
    Ok(())
  })
  .await
}
