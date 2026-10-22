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

/// Run `servers` until the transport ends or the process is signalled,
/// then close every browser each of them launched.
async fn serve_until_shutdown<S>(servers: &[McpServer], serve: S) -> anyhow::Result<()>
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
  let shutdowns = futures_shutdown(servers);
  let cleanup = match tokio::time::timeout(SHUTDOWN_TIMEOUT, shutdowns).await {
    Ok(Ok(())) => Ok(()),
    Ok(Err(error)) => Err(anyhow::anyhow!("browser shutdown failed: {error}")),
    Err(_) => Err(anyhow::anyhow!("browser shutdown timed out")),
  };
  if let Err(error) = &cleanup {
    tracing::warn!(%error, "shutdown did not finish cleanup");
  }
  let outcome = match (outcome, cleanup) {
    (Ok(()), cleanup) => cleanup,
    (Err(error), Ok(())) => Err(error),
    (Err(error), Err(cleanup)) => Err(error.context(format!("{cleanup:#}"))),
  };
  if signalled {
    // Installing a handler took over the signal's default "terminate
    // now", and unwinding back through main does not get us there:
    // dropping the runtime waits on the blocking stdin read of the
    // stdio transport, which only returns when the client closes the
    // pipe. Exit after reporting cleanup rather than waiting for stdin.
    std::process::exit(i32::from(outcome.is_err()));
  }
  outcome
}

/// Shut every server down, all of them even when one fails, reporting the
/// failures together.
async fn futures_shutdown(servers: &[McpServer]) -> Result<(), String> {
  let mut failures = Vec::new();
  for server in servers {
    if let Err(error) = Box::pin(server.shutdown()).await {
      failures.push(error);
    }
  }
  if failures.is_empty() {
    Ok(())
  } else {
    Err(failures.join("; "))
  }
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
pub async fn serve_http(
  mode: ConnectMode,
  backend: BackendKind,
  options: HttpOptions,
  headless: bool,
) -> anyhow::Result<()> {
  // One server shared by every HTTP session, like `serve_http_with`.
  // Minting a fresh `McpServer` per session gave each one its own
  // browser state that nothing ever shut down, so a client that
  // reconnected left a browser behind on every session.
  Box::pin(serve_http_with(
    McpServer::new_headless(mode, backend, headless),
    options,
  ))
  .await
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
  serve_until_shutdown(std::slice::from_ref(&handle), async move {
    svc.waiting().await?;
    Ok(())
  })
  .await
}

/// Where the HTTP transport listens.
#[derive(Debug, Clone, Copy)]
pub struct HttpOptions {
  /// `127.0.0.1` unless the caller means to serve the network, which needs
  /// every client to present a token.
  pub host: std::net::IpAddr,
  pub port: u16,
}

impl Default for HttpOptions {
  fn default() -> Self {
    Self {
      host: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
      port: 8080,
    }
  }
}

/// A client of the HTTP transport: the bearer token it presents and the
/// server, with its own browsers, that answers it. Clients holding different
/// tokens never see each other's browsers; clients sharing one share them,
/// separated only by session key.
pub struct HttpClient {
  pub token: Option<String>,
  pub server: McpServer,
}

/// Serve a custom `McpServer` (with config/extensions) over HTTP to every
/// caller, with no token.
///
/// # Errors
///
/// Returns an error if the TCP listener cannot bind to the requested port,
/// if `options.host` is not a loopback address (an unauthenticated server
/// must not be reachable from the network), or if the HTTP server
/// encounters a fatal error.
pub async fn serve_http_with(server: McpServer, options: HttpOptions) -> anyhow::Result<()> {
  Box::pin(serve_http_clients(vec![HttpClient { token: None, server }], options)).await
}

/// Serve each of `clients` over HTTP at `/mcp`: a request is answered by the
/// client whose token its `Authorization: Bearer` header carries, and refused
/// with `401` when it carries none of them.
///
/// # Errors
///
/// Returns an error when `clients` is empty, two clients share a token, a
/// client without a token is mixed with others or served on a non-loopback
/// address, the listener cannot bind, or the HTTP server fails.
pub async fn serve_http_clients(clients: Vec<HttpClient>, options: HttpOptions) -> anyhow::Result<()> {
  use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
  };

  let tokens: Vec<Option<&str>> = clients.iter().map(|client| client.token.as_deref()).collect();
  let routes = tenant_routes(&tokens, options.host)?;
  reclaim_leaked_browsers().await;
  let ct = tokio_util::sync::CancellationToken::new();
  let mut services = Vec::with_capacity(clients.len());
  let mut owners = Vec::with_capacity(clients.len());
  for client in clients {
    // The 3.0 rename of `stateful_mode`, and it only governs peers older than
    // 2026-07-28 — SEP-2567 removed sessions, so a peer that negotiates
    // 2026-07-28 is served statelessly whatever this says. Those older peers
    // keep their session and standalone GET stream because the server pushes
    // messages at them: progress during navigate/run_script/run_bdd, and
    // `tools/list_changed` when an extension reloads.
    let config = StreamableHttpServerConfig::default()
      .with_cancellation_token(ct.child_token())
      .with_legacy_session_mode(true);
    owners.push(client.server.clone());
    let server = client.server;
    services.push(StreamableHttpService::new(
      move || Ok(server.clone()),
      Arc::new(LocalSessionManager::default()),
      config,
    ));
  }
  let services = Arc::new(services);
  let routes = Arc::new(routes);

  let listener = tokio::net::TcpListener::bind((options.host, options.port)).await?;
  let addr = listener.local_addr()?;
  // The bound address, not the requested one: with port 0 this line is how
  // a supervisor learns where the server is.
  eprintln!("ferridriver listening on http://{addr}/mcp");

  let handler = move |request: axum::extract::Request| {
    let services = Arc::clone(&services);
    let routes = Arc::clone(&routes);
    async move {
      use axum::response::IntoResponse as _;
      use tower_service::Service as _;
      let presented = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
      let Some(index) = routes.client_for(presented) else {
        return (
          axum::http::StatusCode::UNAUTHORIZED,
          [(axum::http::header::WWW_AUTHENTICATE, "Bearer")],
          "a valid bearer token is required",
        )
          .into_response();
      };
      let mut service = services[index].clone();
      match service.call(request).await {
        Ok(response) => response.into_response(),
        Err(never) => match never {},
      }
    }
  };

  serve_until_shutdown(&owners, async move {
    let app = axum::Router::new()
      .route("/mcp", axum::routing::any(handler.clone()))
      .route("/mcp/{*rest}", axum::routing::any(handler));
    axum::serve(listener, app)
      .with_graceful_shutdown(async move { ct.cancelled_owned().await })
      .await?;
    Ok(())
  })
  .await
}

/// Which client answers a request, by the SHA-256 of the token it presents.
struct TenantRoutes {
  /// `Some` for one client served without a token.
  open: Option<usize>,
  by_token: Vec<([u8; 32], usize)>,
}

impl TenantRoutes {
  fn client_for(&self, presented: Option<&str>) -> Option<usize> {
    if let Some(open) = self.open {
      return Some(open);
    }
    let presented = token_digest(presented?);
    self
      .by_token
      .iter()
      .find_map(|(digest, index)| (*digest == presented).then_some(*index))
  }
}

fn token_digest(token: &str) -> [u8; 32] {
  use sha2::Digest as _;
  sha2::Sha256::digest(token.as_bytes()).into()
}

fn tenant_routes(tokens: &[Option<&str>], host: std::net::IpAddr) -> anyhow::Result<TenantRoutes> {
  if tokens.is_empty() {
    anyhow::bail!("no MCP server to serve");
  }
  if tokens.iter().any(Option::is_none) {
    if tokens.len() > 1 {
      anyhow::bail!("every client needs its own token when more than one is served");
    }
    if !host.is_loopback() {
      anyhow::bail!(
        "refusing to serve MCP on {host} without a token: anyone who can reach it would drive the browser. \
         Pass --token (or FERRIDRIVER_MCP_TOKEN), or listen on a loopback address"
      );
    }
    return Ok(TenantRoutes {
      open: Some(0),
      by_token: Vec::new(),
    });
  }
  let mut by_token: Vec<([u8; 32], usize)> = Vec::with_capacity(tokens.len());
  for (index, token) in tokens.iter().enumerate() {
    let token = token.unwrap_or_default();
    if token.is_empty() {
      anyhow::bail!("an MCP token cannot be empty");
    }
    let digest = token_digest(token);
    if by_token.iter().any(|(seen, _)| *seen == digest) {
      anyhow::bail!("two MCP clients were given the same token");
    }
    by_token.push((digest, index));
  }
  Ok(TenantRoutes { open: None, by_token })
}

#[cfg(test)]
mod tenant_tests {
  use super::tenant_routes;

  const LOOPBACK: std::net::IpAddr = std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
  const ANY: std::net::IpAddr = std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED);

  #[test]
  fn each_token_reaches_its_own_server_and_nothing_else_gets_in() {
    let routes = tenant_routes(&[Some("token-a"), Some("token-b")], ANY).unwrap();
    assert_eq!(routes.client_for(Some("token-a")), Some(0));
    assert_eq!(routes.client_for(Some("token-b")), Some(1));
    assert_eq!(routes.client_for(Some("token-c")), None);
    assert_eq!(routes.client_for(None), None);
  }

  #[test]
  fn an_open_server_stays_on_loopback() {
    let routes = tenant_routes(&[None], LOOPBACK).unwrap();
    assert_eq!(routes.client_for(None), Some(0));
    assert!(tenant_routes(&[None], ANY).is_err());
  }

  #[test]
  fn tokens_must_be_present_distinct_and_non_empty() {
    assert!(tenant_routes(&[], LOOPBACK).is_err());
    assert!(tenant_routes(&[None, Some("token-a")], LOOPBACK).is_err());
    assert!(tenant_routes(&[Some("same"), Some("same")], LOOPBACK).is_err());
    assert!(tenant_routes(&[Some("")], LOOPBACK).is_err());
  }
}
