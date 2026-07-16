//! Config-driven web servers for the test runner.
//!
//! `TestServer` serves a `webServer.staticDir` directory (with optional
//! SPA fallback); `WebServerManager` owns the lifecycle of every
//! configured server — static directories and `command`-launched dev
//! servers alike.

use std::net::SocketAddr;
use std::path::PathBuf;

use axum::Router;
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};

/// Static-directory web server backing `webServer.staticDir`.
pub struct TestServer {
  addr: SocketAddr,
  shutdown_tx: tokio::sync::oneshot::Sender<()>,
  handle: tokio::task::JoinHandle<()>,
}

impl TestServer {
  /// Start from a `WebServerConfig`.
  ///
  /// # Errors
  ///
  /// Returns an error if the server fails to bind.
  pub async fn from_config(config: &crate::config::WebServerConfig) -> ferridriver::error::Result<Self> {
    let dir = config.static_dir.as_deref().unwrap_or(".");
    Self::start_with_options(PathBuf::from(dir), config.port, config.spa).await
  }

  async fn start_with_options(assets_dir: PathBuf, port: u16, spa: bool) -> ferridriver::error::Result<Self> {
    let serve_dir = ServeDir::new(&assets_dir).append_index_html_on_directories(true);
    let app = if spa {
      // SPA fallback: unmatched routes serve `index.html` (client-side routing).
      let index = ServeFile::new(assets_dir.join("index.html"));
      Router::new().fallback_service(serve_dir.fallback(index))
    } else {
      Router::new().fallback_service(serve_dir)
    };
    let app = app.layer(CorsLayer::permissive());

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
      shutdown_tx,
      handle,
    })
  }

  /// The base URL, e.g. `http://127.0.0.1:39201`.
  #[must_use]
  pub fn url(&self) -> String {
    format!("http://{}", self.addr)
  }

  /// Stop the server.
  pub async fn stop(self) {
    let _ = self.shutdown_tx.send(());
    let _ = self.handle.await;
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
