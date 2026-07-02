//! TestServer: Playwright-style HTTP server for E2E test fixtures.
//!
//! Serves static files from `tests/assets/` and supports programmatic routes
//! for dynamic responses, request tracking, and interception.
//!
//! Usage:
//! ```ignore
//! let server = TestServer::start("tests/assets").await?;
//! page.goto(&format!("{}/input/button.html", server.url()), None).await?;
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
  pub async fn start(assets_dir: impl Into<PathBuf>) -> ferridriver::error::Result<Self> {
    Self::start_with_options(assets_dir.into(), 0, false).await
  }

  /// Start with SPA fallback: unmatched routes serve `index.html`.
  pub async fn start_spa(assets_dir: impl Into<PathBuf>) -> ferridriver::error::Result<Self> {
    Self::start_with_options(assets_dir.into(), 0, true).await
  }

  /// Start from a `WebServerConfig`.
  pub async fn from_config(config: &crate::config::WebServerConfig) -> ferridriver::error::Result<Self> {
    let dir = config.static_dir.as_deref().unwrap_or(".");
    Self::start_with_options(PathBuf::from(dir), config.port, config.spa).await
  }

  async fn start_with_options(assets_dir: PathBuf, port: u16, spa: bool) -> ferridriver::error::Result<Self> {
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
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    let addr = listener.local_addr()?;
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
    headers: header_map.clone(),
    body: body.to_vec(),
  });

  // Built-in echo endpoints under `/_api/` — an httpbin-shaped JSON echo
  // of the request (url, method, headers, raw body, parsed JSON body) so
  // HTTP-client fixtures can assert round-trips without depending on an
  // external service being up.
  if request_path.starts_with("/_api/") {
    let body_text = String::from_utf8_lossy(&body).to_string();
    let parsed_json: serde_json::Value = serde_json::from_str(&body_text).unwrap_or(serde_json::Value::Null);
    let echo = serde_json::json!({
      "url": request_path,
      "method": method.to_string(),
      "headers": header_map,
      "data": body_text,
      "json": parsed_json,
    });
    return Response::builder()
      .status(200)
      .header("content-type", "application/json")
      .header("access-control-allow-origin", "*")
      .body(Body::from(echo.to_string()))
      .unwrap_or_else(|_| {
        Response::builder()
          .status(500)
          .body(Body::empty())
          .expect("empty 500 response")
      });
  }

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
    return builder.body(Body::from(resp.body)).unwrap_or_else(|_| {
      Response::builder()
        .status(500)
        .body(Body::empty())
        .expect("empty 500 response")
    });
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
        .expect("static file response"),
      Err(_) => Response::builder()
        .status(500)
        .body(Body::empty())
        .expect("empty 500 response"),
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
          .expect("SPA index.html response"),
        Err(_) => Response::builder()
          .status(500)
          .body(Body::empty())
          .expect("empty 500 response"),
      }
    } else {
      Response::builder()
        .status(404)
        .header("content-type", "text/plain")
        .body(Body::from("Not Found (SPA: no index.html)"))
        .expect("404 response")
    }
  } else {
    Response::builder()
      .status(404)
      .header("content-type", "text/plain")
      .body(Body::from("Not Found"))
      .expect("404 response")
  }
}

// ── WebServerManager: lifecycle management for config-driven servers ──────

/// Manages one or more web servers started from `WebServerConfig` entries.
/// Handles both command-based dev servers and static file servers.
pub struct WebServerManager {
  servers: Vec<RunningServer>,
}

enum RunningServer {
  Static(Box<StaticEntry>),
  Command(Box<CommandEntry>),
}

struct StaticEntry {
  server: TestServer,
  name: String,
}

struct CommandEntry {
  child: tokio::process::Child,
  url: String,
  name: String,
  graceful: Option<crate::config::GracefulShutdown>,
}

impl WebServerManager {
  /// Start all configured web servers. Returns the URL of the first server
  /// (for use as `base_url`).
  ///
  /// # Errors
  ///
  /// Returns an error if any server fails to start or become ready.
  pub async fn start(configs: &[crate::config::WebServerConfig]) -> ferridriver::error::Result<Self> {
    let mut servers = Vec::with_capacity(configs.len());
    for config in configs {
      let display_name = config.name.clone().unwrap_or_else(|| "WebServer".to_string());
      if let Some(ref dir) = config.static_dir {
        let server = TestServer::start_with_options(PathBuf::from(dir), config.port, config.spa).await?;
        tracing::info!(name = %display_name, "[{display_name}] Static server ready at {} (serving {})", server.url(), dir);
        servers.push(RunningServer::Static(Box::new(StaticEntry {
          server,
          name: display_name,
        })));
      } else if let Some(ref command) = config.command {
        let url = config.url.as_deref().ok_or_else(|| {
          ferridriver::FerriError::invalid_argument(
            "webServer.url",
            format!("webServer command requires 'url' to wait for: {command}"),
          )
        })?;

        // Check if server is already running (reuse). The reuse probe
        // honours `ignore_https_errors` so that a self-signed dev
        // server registers as up.
        if config.reuse_existing_server && http_probe(url, config.ignore_https_errors).await {
          tracing::info!(name = %display_name, "[{display_name}] Reusing existing server at {url}");
          // Spawn a no-op placeholder so that stop()'s child handle
          // path can run uniformly across reuse/launch — this matches
          // the prior behaviour but tags the entry with the name and
          // configured graceful-shutdown so logs stay informative.
          servers.push(RunningServer::Command(Box::new(CommandEntry {
            child: tokio::process::Command::new("true").spawn()?,
            url: url.to_string(),
            name: display_name,
            graceful: config.graceful_shutdown.clone(),
          })));
          continue;
        }

        let cwd = config.cwd.as_deref().unwrap_or(".");
        let child = spawn_command(command, cwd, &config.env)?;
        wait_for_url(url, config.timeout, config.ignore_https_errors, &display_name).await?;
        tracing::info!(name = %display_name, "[{display_name}] Dev server ready at {url} (command: {command})");
        servers.push(RunningServer::Command(Box::new(CommandEntry {
          child,
          url: url.to_string(),
          name: display_name,
          graceful: config.graceful_shutdown.clone(),
        })));
      } else {
        return Err(ferridriver::FerriError::invalid_argument(
          "webServer",
          "webServer config must have either 'command' or 'staticDir'",
        ));
      }
    }
    Ok(Self { servers })
  }

  /// URL of the first server, or None if no servers.
  #[must_use]
  pub fn first_url(&self) -> Option<String> {
    self.servers.first().map(|s| match s {
      RunningServer::Static(entry) => entry.server.url(),
      RunningServer::Command(entry) => entry.url.clone(),
    })
  }

  /// Get the TestServer instance (for programmatic routes), if the first server is static.
  pub fn test_server(&self) -> Option<&TestServer> {
    self.servers.first().and_then(|s| match s {
      RunningServer::Static(entry) => Some(&entry.server),
      RunningServer::Command(_) => None,
    })
  }

  /// Stop all servers. When a `Command`-mode server has
  /// `graceful_shutdown` configured, the manager sends the soft signal
  /// (`SIGINT` or `SIGTERM`) first and waits up to `timeout` ms before
  /// escalating to `SIGKILL`. Without `graceful_shutdown`, the child
  /// is killed immediately (preserving prior behaviour).
  pub async fn stop(self) {
    for server in self.servers {
      match server {
        RunningServer::Static(entry) => {
          let StaticEntry { server, name } = *entry;
          tracing::info!(name = %name, "[{name}] Stopping static server");
          server.stop().await;
        },
        RunningServer::Command(entry) => {
          let CommandEntry {
            mut child,
            name,
            graceful,
            ..
          } = *entry;
          stop_child(&mut child, &name, graceful.as_ref()).await;
        },
      }
    }
  }
}

async fn stop_child(child: &mut tokio::process::Child, name: &str, graceful: Option<&crate::config::GracefulShutdown>) {
  let Some(g) = graceful else {
    tracing::info!(name = %name, "[{name}] Hard-killing child process");
    let _ = child.kill().await;
    return;
  };

  let Some(pid) = child.id() else {
    // Child already exited (or never started). Fall through to wait.
    let _ = child.wait().await;
    return;
  };

  let signum = parse_signal(&g.signal);
  tracing::info!(
    name = %name,
    "[{name}] Sending {} (graceful_shutdown), waiting up to {}ms before SIGKILL",
    g.signal,
    g.timeout
  );
  #[cfg(unix)]
  send_signal(pid, signum);
  #[cfg(not(unix))]
  {
    let _ = (pid, signum);
    let _ = child.kill().await;
    return;
  }

  let timeout = std::time::Duration::from_millis(g.timeout);
  if tokio::time::timeout(timeout, child.wait()).await.is_ok() {
    tracing::info!(name = %name, "[{name}] Process exited gracefully");
  } else {
    tracing::warn!(
      name = %name,
      "[{name}] Process did not exit within {}ms — escalating to SIGKILL",
      g.timeout
    );
    let _ = child.kill().await;
  }
}

fn parse_signal(name: &str) -> libc::c_int {
  match name.trim().to_ascii_uppercase().as_str() {
    "SIGINT" => libc::SIGINT,
    "SIGKILL" => libc::SIGKILL,
    _ => libc::SIGTERM,
  }
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn send_signal(pid: u32, signum: libc::c_int) {
  // Cast is safe: child PIDs from `tokio::process::Child::id()` fit in
  // i32 on every Unix we target.
  #[allow(clippy::cast_possible_wrap)]
  let pid_i = pid as libc::pid_t;
  // SAFETY: `kill` is async-signal-safe. The PID came from
  // `Child::id()` for a process we own, so the call has no effect on
  // any process we don't own even if the PID has been reused by the
  // time the signal lands (we'd just no-op via EPERM).
  unsafe {
    libc::kill(pid_i, signum);
  }
}

fn spawn_command(
  command: &str,
  cwd: &str,
  env: &std::collections::BTreeMap<String, String>,
) -> ferridriver::error::Result<tokio::process::Child> {
  let mut cmd = if cfg!(target_os = "windows") {
    let mut c = tokio::process::Command::new("cmd");
    c.args(["/C", command]);
    c
  } else {
    // `exec` replaces the sh process with the user's command so signals
    // sent to the child PID land on the real process (e.g. node) rather
    // than dying in the sh wrapper. Without it, SIGTERM kills sh and
    // leaves node orphaned with no chance to run its trap handler — the
    // graceful_shutdown contract becomes a silent SIGKILL.
    let mut c = tokio::process::Command::new("sh");
    c.args(["-c", &format!("exec {command}")]);
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
  cmd
    .spawn()
    .map_err(|e| ferridriver::FerriError::backend(format!("spawn '{command}': {e}")))
}

/// Build the readiness-probe HTTP client, optionally accepting
/// invalid TLS certs when the user passed `ignoreHTTPSErrors: true`
/// on the `webServer` entry. A short request timeout keeps the probe
/// non-blocking under the logarithmic backoff loop.
#[must_use]
pub fn build_probe_client(ignore_https_errors: bool) -> reqwest::Client {
  reqwest::Client::builder()
    .danger_accept_invalid_certs(ignore_https_errors)
    .timeout(std::time::Duration::from_secs(5))
    .build()
    .unwrap_or_else(|_| reqwest::Client::new())
}

/// Single readiness check via HTTP GET. Mirrors Playwright's
/// `isURLAvailable`: any 2xx/3xx status counts as up; 404 falls back
/// to `/index.html` (consistent with serving a static SPA).
pub async fn http_probe(url: &str, ignore_https_errors: bool) -> bool {
  let client = build_probe_client(ignore_https_errors);
  match probe_status(&client, url).await {
    Some(s) if (200..404).contains(&s) => true,
    Some(404) => {
      // Retry against /index.html if the URL is a bare host root.
      let index_url = if url.ends_with('/') {
        format!("{url}index.html")
      } else {
        format!("{url}/index.html")
      };
      matches!(probe_status(&client, &index_url).await, Some(s) if (200..404).contains(&s))
    },
    _ => false,
  }
}

async fn probe_status(client: &reqwest::Client, url: &str) -> Option<u16> {
  match client.get(url).send().await {
    Ok(resp) => Some(resp.status().as_u16()),
    Err(_) => None,
  }
}

/// Wait for a URL to become reachable with logarithmic backoff (matching Playwright).
async fn wait_for_url(
  url: &str,
  timeout_ms: u64,
  ignore_https_errors: bool,
  name: &str,
) -> ferridriver::error::Result<()> {
  let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);

  // Logarithmic backoff: 100ms, 250ms, 500ms, then 1000ms thereafter.
  let mut delays = [100u64, 250, 500].iter().copied();

  loop {
    if tokio::time::Instant::now() >= deadline {
      return Err(ferridriver::FerriError::timeout(
        format!("[{name}] webServer {url}"),
        timeout_ms,
      ));
    }
    if http_probe(url, ignore_https_errors).await {
      return Ok(());
    }
    let delay = delays.next().unwrap_or(1000);
    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
  }
}
