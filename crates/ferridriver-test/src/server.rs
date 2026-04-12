//! TestServer: Playwright-style HTTP server for E2E test fixtures.
//!
//! Serves static files from `tests/assets/` and supports programmatic routes
//! for dynamic responses, request tracking, and interception.
//!
//! Usage:
//! ```ignore
//! let server = TestServer::start("tests/assets").await?;
//! page.goto(&format!("{}/input/button.html", server.url())).await?;
//! server.stop().await;
//! ```

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Response};
use axum::routing::any;
use tokio::sync::RwLock;
use tower_http::services::ServeDir;

type RouteHandlerFn = Arc<dyn Fn(&str, &HeaderMap) -> RouteResponse + Send + Sync>;

/// A programmatic route response.
pub struct RouteResponse {
  pub status: u16,
  pub content_type: String,
  pub body: Vec<u8>,
  pub headers: Vec<(String, String)>,
}

impl RouteResponse {
  /// HTML response.
  pub fn html(body: &str) -> Self {
    Self {
      status: 200,
      content_type: "text/html".into(),
      body: body.as_bytes().to_vec(),
      headers: vec![],
    }
  }

  /// JSON response.
  pub fn json(body: &str) -> Self {
    Self {
      status: 200,
      content_type: "application/json".into(),
      body: body.as_bytes().to_vec(),
      headers: vec![],
    }
  }

  /// Plain text response.
  pub fn text(body: &str) -> Self {
    Self {
      status: 200,
      content_type: "text/plain".into(),
      body: body.as_bytes().to_vec(),
      headers: vec![],
    }
  }

  /// Empty response with status code.
  pub fn status(code: u16) -> Self {
    Self {
      status: code,
      content_type: "text/plain".into(),
      body: vec![],
      headers: vec![],
    }
  }
}

/// Recorded request for assertion.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
  pub path: String,
  pub method: String,
  pub headers: HashMap<String, String>,
  pub body: Vec<u8>,
}

struct ServerState {
  routes: RwLock<HashMap<String, RouteHandlerFn>>,
  requests: RwLock<Vec<RecordedRequest>>,
  assets_dir: PathBuf,
  spa: bool,
}

/// Playwright-style test HTTP server.
///
/// Serves static files from an assets directory and supports
/// programmatic routes for dynamic responses.
pub struct TestServer {
  addr: SocketAddr,
  state: Arc<ServerState>,
  shutdown_tx: tokio::sync::oneshot::Sender<()>,
  handle: tokio::task::JoinHandle<()>,
}

impl TestServer {
  /// Start the test server, serving static files from `assets_dir`.
  ///
  /// # Errors
  ///
  /// Returns an error if the server fails to bind.
  pub async fn start(assets_dir: impl Into<PathBuf>) -> Result<Self, String> {
    Self::start_with_options(assets_dir.into(), 0, false).await
  }

  /// Start with SPA fallback: unmatched routes serve `index.html`.
  pub async fn start_spa(assets_dir: impl Into<PathBuf>) -> Result<Self, String> {
    Self::start_with_options(assets_dir.into(), 0, true).await
  }

  /// Start from a `WebServerConfig`.
  pub async fn from_config(config: &crate::config::WebServerConfig) -> Result<Self, String> {
    let dir = config.static_dir.as_deref().unwrap_or(".");
    Self::start_with_options(PathBuf::from(dir), config.port, config.spa).await
  }

  async fn start_with_options(assets_dir: PathBuf, port: u16, spa: bool) -> Result<Self, String> {
    let state = Arc::new(ServerState {
      routes: RwLock::new(HashMap::new()),
      requests: RwLock::new(Vec::new()),
      assets_dir: assets_dir.clone(),
      spa,
    });

    let state2 = state.clone();
    let fallback = ServeDir::new(&assets_dir).append_index_html_on_directories(true);

    let app = Router::new()
      .route("/{*path}", any(handle_request))
      .route("/", any(handle_request))
      .with_state(state2)
      .fallback_service(fallback);

    let bind_addr = format!("127.0.0.1:{port}");
    let listener = tokio::net::TcpListener::bind(&bind_addr)
      .await
      .map_err(|e| format!("bind {bind_addr}: {e}"))?;
    let addr = listener.local_addr().map_err(|e| format!("local_addr: {e}"))?;
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let handle = tokio::spawn(async move {
      axum::serve(listener, app)
        .with_graceful_shutdown(async {
          let _ = shutdown_rx.await;
        })
        .await
        .ok();
    });

    Ok(Self {
      addr,
      state,
      shutdown_tx,
      handle,
    })
  }

  /// The base URL, e.g. `http://127.0.0.1:39201`.
  #[must_use]
  pub fn url(&self) -> String {
    format!("http://{}", self.addr)
  }

  /// Shorthand: `{url}/path`.
  #[must_use]
  pub fn prefix(&self) -> String {
    self.url()
  }

  /// URL for the empty page.
  #[must_use]
  pub fn empty_page(&self) -> String {
    format!("{}/empty.html", self.url())
  }

  /// Register a programmatic route. The handler receives (path, headers) and returns a response.
  pub async fn set_route(&self, path: &str, handler: RouteHandlerFn) {
    self.state.routes.write().await.insert(path.to_string(), handler);
  }

  /// Register a simple static response for a path.
  pub async fn set_content(&self, path: &str, content_type: &str, body: &str) {
    let ct = content_type.to_string();
    let b = body.as_bytes().to_vec();
    self
      .set_route(
        path,
        Arc::new(move |_, _| RouteResponse {
          status: 200,
          content_type: ct.clone(),
          body: b.clone(),
          headers: vec![],
        }),
      )
      .await;
  }

  /// Get all recorded requests.
  pub async fn requests(&self) -> Vec<RecordedRequest> {
    self.state.requests.read().await.clone()
  }

  /// Get recorded requests matching a path prefix.
  pub async fn requests_for(&self, path: &str) -> Vec<RecordedRequest> {
    self
      .state
      .requests
      .read()
      .await
      .iter()
      .filter(|r| r.path.starts_with(path))
      .cloned()
      .collect()
  }

  /// Clear recorded requests.
  pub async fn clear_requests(&self) {
    self.state.requests.write().await.clear();
  }

  /// Stop the server.
  pub async fn stop(self) {
    let _ = self.shutdown_tx.send(());
    let _ = self.handle.await;
  }
}

async fn handle_request(
  State(state): State<Arc<ServerState>>,
  path: Option<Path<String>>,
  headers: HeaderMap,
  method: axum::http::Method,
  body: axum::body::Bytes,
) -> Response<Body> {
  let request_path = format!("/{}", path.as_ref().map(|p| p.as_str()).unwrap_or(""));

  // Record the request.
  let mut header_map = HashMap::new();
  for (name, value) in &headers {
    if let Ok(v) = value.to_str() {
      header_map.insert(name.to_string(), v.to_string());
    }
  }
  state.requests.write().await.push(RecordedRequest {
    path: request_path.clone(),
    method: method.to_string(),
    headers: header_map,
    body: body.to_vec(),
  });

  // Check programmatic routes.
  let routes = state.routes.read().await;
  if let Some(handler) = routes.get(&request_path) {
    let resp = handler(&request_path, &headers);
    let mut builder = Response::builder().status(resp.status);
    builder = builder.header("content-type", &resp.content_type);
    builder = builder.header("access-control-allow-origin", "*");
    for (k, v) in &resp.headers {
      builder = builder.header(k.as_str(), v.as_str());
    }
    return builder
      .body(Body::from(resp.body))
      .unwrap_or_else(|_| Response::builder().status(500).body(Body::empty()).unwrap());
  }
  drop(routes);

  // Fall through to static file serving — return 404 so the fallback layer handles it.
  // axum's fallback_service will serve static files if this handler returns 404.
  let file_path = state.assets_dir.join(request_path.trim_start_matches('/'));
  if file_path.exists() && file_path.is_file() {
    let content_type = mime_guess::from_path(&file_path).first_or_octet_stream().to_string();
    match tokio::fs::read(&file_path).await {
      Ok(contents) => Response::builder()
        .status(200)
        .header("content-type", content_type)
        .header("access-control-allow-origin", "*")
        .body(Body::from(contents))
        .unwrap(),
      Err(_) => Response::builder().status(500).body(Body::empty()).unwrap(),
    }
  } else if state.spa {
    // SPA fallback: serve index.html for any unmatched route (client-side routing).
    let index = state.assets_dir.join("index.html");
    if index.exists() {
      match tokio::fs::read(&index).await {
        Ok(contents) => Response::builder()
          .status(200)
          .header("content-type", "text/html")
          .header("access-control-allow-origin", "*")
          .body(Body::from(contents))
          .unwrap(),
        Err(_) => Response::builder().status(500).body(Body::empty()).unwrap(),
      }
    } else {
      Response::builder()
        .status(404)
        .header("content-type", "text/plain")
        .body(Body::from("Not Found (SPA: no index.html)"))
        .unwrap()
    }
  } else {
    Response::builder()
      .status(404)
      .header("content-type", "text/plain")
      .body(Body::from("Not Found"))
      .unwrap()
  }
}

// ── WebServerManager: lifecycle management for config-driven servers ──────

/// Manages one or more web servers started from `WebServerConfig` entries.
/// Handles both command-based dev servers and static file servers.
pub struct WebServerManager {
  servers: Vec<RunningServer>,
}

enum RunningServer {
  Static(TestServer),
  Command { child: tokio::process::Child, url: String },
}

impl WebServerManager {
  /// Start all configured web servers. Returns the URL of the first server
  /// (for use as `base_url`).
  ///
  /// # Errors
  ///
  /// Returns an error if any server fails to start or become ready.
  pub async fn start(configs: &[crate::config::WebServerConfig]) -> Result<Self, String> {
    let mut servers = Vec::with_capacity(configs.len());
    for config in configs {
      if let Some(ref dir) = config.static_dir {
        let server = TestServer::start_with_options(PathBuf::from(dir), config.port, config.spa).await?;
        tracing::info!("Static server ready at {} (serving {})", server.url(), dir);
        servers.push(RunningServer::Static(server));
      } else if let Some(ref command) = config.command {
        let url = config
          .url
          .as_deref()
          .ok_or_else(|| format!("webServer command requires 'url' to wait for: {command}"))?;

        // Check if server is already running (reuse).
        if config.reuse_existing_server && is_url_reachable(url).await {
          tracing::info!("Reusing existing server at {url}");
          servers.push(RunningServer::Command {
            child: tokio::process::Command::new("true")
              .spawn()
              .map_err(|e| e.to_string())?,
            url: url.to_string(),
          });
          continue;
        }

        let cwd = config.cwd.as_deref().unwrap_or(".");
        let child = spawn_command(command, cwd, &config.env)?;
        wait_for_url(url, config.timeout).await?;
        tracing::info!("Dev server ready at {url} (command: {command})");
        servers.push(RunningServer::Command {
          child,
          url: url.to_string(),
        });
      } else {
        return Err("webServer config must have either 'command' or 'staticDir'".into());
      }
    }
    Ok(Self { servers })
  }

  /// URL of the first server, or None if no servers.
  #[must_use]
  pub fn first_url(&self) -> Option<String> {
    self.servers.first().map(|s| match s {
      RunningServer::Static(ts) => ts.url(),
      RunningServer::Command { url, .. } => url.clone(),
    })
  }

  /// Get the TestServer instance (for programmatic routes), if the first server is static.
  pub fn test_server(&self) -> Option<&TestServer> {
    self.servers.first().and_then(|s| match s {
      RunningServer::Static(ts) => Some(ts),
      RunningServer::Command { .. } => None,
    })
  }

  /// Stop all servers.
  pub async fn stop(self) {
    for server in self.servers {
      match server {
        RunningServer::Static(ts) => ts.stop().await,
        RunningServer::Command { mut child, .. } => {
          let _ = child.kill().await;
        },
      }
    }
  }
}

fn spawn_command(
  command: &str,
  cwd: &str,
  env: &std::collections::BTreeMap<String, String>,
) -> Result<tokio::process::Child, String> {
  let mut cmd = if cfg!(target_os = "windows") {
    let mut c = tokio::process::Command::new("cmd");
    c.args(["/C", command]);
    c
  } else {
    let mut c = tokio::process::Command::new("sh");
    c.args(["-c", command]);
    c
  };
  cmd.current_dir(cwd);
  for (k, v) in env {
    cmd.env(k, v);
  }
  cmd
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped());
  cmd.spawn().map_err(|e| format!("spawn '{command}': {e}"))
}

async fn is_url_reachable(url: &str) -> bool {
  tokio::net::TcpStream::connect(
    url
      .trim_start_matches("http://")
      .trim_start_matches("https://")
      .split('/')
      .next()
      .unwrap_or(""),
  )
  .await
  .is_ok()
}

/// Wait for a URL to become reachable with logarithmic backoff (matching Playwright).
async fn wait_for_url(url: &str, timeout_ms: u64) -> Result<(), String> {
  let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
  let addr = url
    .trim_start_matches("http://")
    .trim_start_matches("https://")
    .split('/')
    .next()
    .unwrap_or("");

  // Logarithmic backoff: 100ms, 250ms, 500ms, then 1000ms thereafter.
  let mut delays = [100u64, 250, 500].iter().copied();

  loop {
    if tokio::time::Instant::now() >= deadline {
      return Err(format!("webServer timeout: {url} not reachable after {timeout_ms}ms"));
    }
    if tokio::net::TcpStream::connect(addr).await.is_ok() {
      return Ok(());
    }
    let delay = delays.next().unwrap_or(1000);
    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
  }
}
