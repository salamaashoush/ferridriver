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
  Reused { url: String },
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
    let mut manager = Self {
      servers: Vec::with_capacity(configs.len()),
    };
    for config in configs {
      match start_server(config).await {
        Ok(server) => manager.servers.push(server),
        Err(error) => {
          manager.stop().await;
          return Err(error);
        },
      }
    }
    Ok(manager)
  }

  /// URL of the first server, or None if no servers.
  #[must_use]
  pub fn first_url(&self) -> Option<String> {
    self.servers.first().map(|s| match s {
      RunningServer::Static(entry) => entry.server.url(),
      RunningServer::Command(entry) => entry.url.clone(),
      RunningServer::Reused { url } => url.clone(),
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
        RunningServer::Reused { .. } => {},
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

async fn start_server(config: &crate::config::WebServerConfig) -> ferridriver::error::Result<RunningServer> {
  let name = config.name.clone().unwrap_or_else(|| "WebServer".to_string());
  if let Some(dir) = &config.static_dir {
    let server = TestServer::start_with_options(PathBuf::from(dir), config.port, config.spa).await?;
    tracing::info!(name = %name, "[{name}] Static server ready at {} (serving {})", server.url(), dir);
    return Ok(RunningServer::Static(Box::new(StaticEntry { server, name })));
  }
  let command = config.command.as_deref().ok_or_else(|| {
    ferridriver::FerriError::invalid_argument(
      "webServer",
      "webServer config must have either 'command' or 'staticDir'",
    )
  })?;
  let url = config.url.as_deref().ok_or_else(|| {
    ferridriver::FerriError::invalid_argument(
      "webServer.url",
      format!("webServer command requires 'url' to wait for: {command}"),
    )
  })?;
  let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(config.timeout);
  let client = build_probe_client(config.ignore_https_errors);
  if config.reuse_existing_server
    && tokio::time::timeout_at(deadline, http_probe_with_client(&client, url))
      .await
      .map_err(|_| readiness_timeout(url, config.timeout, &name))?
  {
    tracing::info!(name = %name, "[{name}] Reusing existing server at {url}");
    return Ok(RunningServer::Reused { url: url.to_string() });
  }
  let output = OutputPolicy {
    name: name.clone(),
    stdout: config.stdout.as_deref() == Some("pipe"),
    stderr: config.stderr.as_deref() != Some("ignore"),
  };
  let (mut child, tail) = spawn_command(command, config.cwd.as_deref().unwrap_or("."), &config.env, &output)?;
  let ready = tokio::select! {
    biased;
    status = child.wait() => {
      let status = status?;
      // The reader ends at the pipe's end of file, which a child the
      // server left running would hold open; past a second, report what
      // has arrived.
      let _ = tokio::time::timeout(std::time::Duration::from_secs(1), tail.finished()).await;
      Err(ferridriver::FerriError::backend(exit_message(&name, status, &tail.lines())))
    },
    result = wait_for_url(&client, url, deadline, config.timeout, &name) => result,
  };
  if let Err(error) = ready {
    stop_child(&mut child, &name, config.graceful_shutdown.as_ref()).await;
    return Err(error);
  }
  tracing::info!(name = %name, "[{name}] Dev server ready at {url} (command: {command})");
  Ok(RunningServer::Command(Box::new(CommandEntry {
    child,
    url: url.to_string(),
    name,
    graceful: config.graceful_shutdown.clone(),
  })))
}

/// Kill the command and everything it started. On Unix the shell leads its
/// own process group, so a dev server's children (`bun` then `vite`, `npm`
/// then `node`) go with it rather than outliving the run, as in
/// Playwright's processLauncher.
async fn kill_tree(child: &mut tokio::process::Child) {
  if let Some(pid) = child.id() {
    #[cfg(unix)]
    send_signal_to_group(pid, libc::SIGKILL);
    #[cfg(windows)]
    ferridriver::backend::process::kill_process_group(pid);
  }
  let _ = child.kill().await;
}

/// Windows has no signal to ask a process tree to stop, so Playwright does
/// not support graceful shutdown there and kills the tree.
#[cfg(windows)]
async fn stop_child(child: &mut tokio::process::Child, name: &str, graceful: Option<&crate::config::GracefulShutdown>) {
  if graceful.is_some() {
    tracing::info!(name = %name, "[{name}] Graceful shutdown is not supported on Windows; killing the process tree");
  } else {
    tracing::info!(name = %name, "[{name}] Hard-killing child process");
  }
  kill_tree(child).await;
}

#[cfg(unix)]
async fn stop_child(child: &mut tokio::process::Child, name: &str, graceful: Option<&crate::config::GracefulShutdown>) {
  let Some(g) = graceful else {
    tracing::info!(name = %name, "[{name}] Hard-killing child process");
    kill_tree(child).await;
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
  // SIGINT is meant for a whole process group, and SIGTERM goes the same
  // way for consistency, as Playwright's webServer does.
  send_signal_to_group(pid, signum);

  let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(g.timeout);
  // The shell can die of the signal at once while the server it started
  // is still running its shutdown handler, so the whole group is waited
  // for, not only the shell.
  let exited = tokio::time::timeout_at(deadline, async {
    let _ = child.wait().await;
    while group_alive(pid) {
      tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
  })
  .await
  .is_ok();
  if exited {
    tracing::info!(name = %name, "[{name}] Process exited gracefully");
  } else {
    tracing::warn!(
      name = %name,
      "[{name}] Process did not exit within {}ms — escalating to SIGKILL",
      g.timeout
    );
    kill_tree(child).await;
  }
}

#[cfg(unix)]
fn parse_signal(name: &str) -> libc::c_int {
  match name.trim().to_ascii_uppercase().as_str() {
    "SIGINT" => libc::SIGINT,
    "SIGKILL" => libc::SIGKILL,
    _ => libc::SIGTERM,
  }
}

/// Whether any process is left in the group `pid` led.
#[cfg(unix)]
#[allow(unsafe_code)]
fn group_alive(pid: u32) -> bool {
  #[allow(clippy::cast_possible_wrap)]
  let pid_i = pid as libc::pid_t;
  // SAFETY: signal 0 only checks for existence and permission; nothing is
  // delivered.
  unsafe { libc::kill(-pid_i, 0) == 0 }
}

/// Signal the process group `pid` leads.
#[cfg(unix)]
#[allow(unsafe_code)]
fn send_signal_to_group(pid: u32, signum: libc::c_int) {
  // Cast is safe: child PIDs from `tokio::process::Child::id()` fit in
  // i32 on every Unix we target.
  #[allow(clippy::cast_possible_wrap)]
  let pid_i = pid as libc::pid_t;
  // SAFETY: `kill` is async-signal-safe. The PID came from
  // `Child::id()` for a process we spawned as its group leader, so the
  // negative PID names that group; a group that no longer exists, or one
  // we do not own, makes the call a no-op (ESRCH or EPERM).
  unsafe {
    libc::kill(-pid_i, signum);
  }
}

fn spawn_command(
  command: &str,
  cwd: &str,
  env: &std::collections::BTreeMap<String, String>,
  output: &OutputPolicy,
) -> ferridriver::error::Result<(tokio::process::Child, OutputTail)> {
  // The platform shell, so `build && start` runs both halves. On Unix the
  // shell leads a process group and the group is what gets signalled.
  let mut cmd = tokio::process::Command::from(ferridriver::backend::process::shell_command(command));
  #[cfg(unix)]
  cmd.process_group(0);
  cmd.current_dir(cwd);
  for (k, v) in env {
    cmd.env(k, v);
  }
  cmd
    .kill_on_drop(true)
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped());
  let mut child = ferridriver::backend::process::spawn_exclusive(|| cmd.spawn())
    .map_err(|e| ferridriver::FerriError::backend(format!("spawn '{command}': {e}")))?;
  let tail = OutputTail::default();
  if let Some(stdout) = child.stdout.take() {
    drain_output(
      stdout,
      output.stdout.then(|| (Stream::Stdout, output.name.clone())),
      None,
    );
  }
  if let Some(stderr) = child.stderr.take() {
    drain_output(
      stderr,
      output.stderr.then(|| (Stream::Stderr, output.name.clone())),
      Some(tail.clone()),
    );
  }
  Ok((child, tail))
}

/// What a command server's output is forwarded to, as Playwright's
/// `webServer.stdout` (`ignore` unless `pipe`) and `webServer.stderr`
/// (`pipe` unless `ignore`): each line printed with the server's name.
struct OutputPolicy {
  name: String,
  stdout: bool,
  stderr: bool,
}

#[derive(Clone, Copy)]
enum Stream {
  Stdout,
  Stderr,
}

/// The last lines a server wrote to stderr, whatever is forwarded, so a
/// server that exits before it is ready says why.
#[derive(Clone, Default)]
struct OutputTail {
  lines: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<String>>>,
  ended: std::sync::Arc<tokio::sync::Notify>,
  done: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl OutputTail {
  const LINES: usize = 20;

  fn end(&self) {
    self.done.store(true, std::sync::atomic::Ordering::SeqCst);
    self.ended.notify_waiters();
  }

  async fn finished(&self) {
    let ended = self.ended.notified();
    if !self.done.load(std::sync::atomic::Ordering::SeqCst) {
      ended.await;
    }
  }

  fn push(&self, line: String) {
    let mut lines = self.lines.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if lines.len() == Self::LINES {
      lines.pop_front();
    }
    lines.push_back(line);
  }

  fn lines(&self) -> Vec<String> {
    self
      .lines
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .iter()
      .cloned()
      .collect()
  }
}

fn exit_message(name: &str, status: std::process::ExitStatus, tail: &[String]) -> String {
  let mut message = format!("[{name}] webServer process exited: {status}");
  if !tail.is_empty() {
    message.push_str("; its last output:");
    for line in tail {
      message.push_str(&format!("\n[{name}] {line}"));
    }
  }
  message
}

fn drain_output(
  output: impl tokio::io::AsyncRead + Unpin + Send + 'static,
  forward: Option<(Stream, String)>,
  tail: Option<OutputTail>,
) {
  use tokio::io::AsyncBufReadExt as _;
  tokio::spawn(async move {
    let mut reader = tokio::io::BufReader::new(output);
    let mut buffer = Vec::new();
    loop {
      buffer.clear();
      match reader.read_until(b'\n', &mut buffer).await {
        Ok(0) | Err(_) => break,
        Ok(_) => {},
      }
      let line = String::from_utf8_lossy(&buffer)
        .trim_end_matches(['\r', '\n'])
        .to_string();
      match &forward {
        Some((Stream::Stdout, name)) => println!("[{name}] {line}"),
        Some((Stream::Stderr, name)) => eprintln!("[{name}] {line}"),
        None => {},
      }
      if let Some(tail) = &tail {
        tail.push(line);
      }
    }
    if let Some(tail) = &tail {
      tail.end();
    }
  });
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
  http_probe_with_client(&build_probe_client(ignore_https_errors), url).await
}

async fn http_probe_with_client(client: &reqwest::Client, url: &str) -> bool {
  match probe_status(client, url).await {
    Some(s) if (200..404).contains(&s) => true,
    Some(404) => {
      // Retry against /index.html if the URL is a bare host root.
      let index_url = if url.ends_with('/') {
        format!("{url}index.html")
      } else {
        format!("{url}/index.html")
      };
      matches!(probe_status(client, &index_url).await, Some(s) if (200..404).contains(&s))
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
  client: &reqwest::Client,
  url: &str,
  deadline: tokio::time::Instant,
  timeout_ms: u64,
  name: &str,
) -> ferridriver::error::Result<()> {
  tokio::time::timeout_at(deadline, async {
    // Logarithmic backoff: 100ms, 250ms, 500ms, then 1000ms thereafter.
    let mut delays = [100u64, 250, 500].iter().copied();
    loop {
      if http_probe_with_client(client, url).await {
        return;
      }
      let delay = delays.next().unwrap_or(1000);
      tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
    }
  })
  .await
  .map_err(|_| readiness_timeout(url, timeout_ms, name))
}

fn readiness_timeout(url: &str, timeout_ms: u64, name: &str) -> ferridriver::FerriError {
  ferridriver::FerriError::timeout(format!("[{name}] webServer {url}"), timeout_ms)
}

#[cfg(all(test, unix))]
mod command_tests {
  use super::WebServerManager;
  use crate::config::{GracefulShutdown, WebServerConfig};

  /// A bare HTTP responder. `http.server` is avoided on purpose: its bind
  /// does a reverse lookup of the host, which can stall past the readiness
  /// timeout on macOS runners. SIGTERM takes half a second and leaves a
  /// marker, so a stop that returns early is visible.
  const SERVER: &str = "import signal, socket, sys, time
def stop(*_):
    time.sleep(0.5)
    open('marker', 'w').write('done')
    sys.exit(0)
signal.signal(signal.SIGTERM, stop)
s = socket.socket()
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(('127.0.0.1', int(sys.argv[1])))
s.listen(16)
while True:
    c, _ = s.accept()
    c.recv(65536)
    c.sendall(b'HTTP/1.1 200 OK\\r\\nContent-Length: 2\\r\\nConnection: close\\r\\n\\r\\nok')
    c.close()
";

  fn free_port() -> std::io::Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
  }

  /// A temp dir holding `server.py`, and a free port for it.
  fn workspace() -> std::io::Result<(tempfile::TempDir, u16)> {
    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join("server.py"), SERVER)?;
    Ok((dir, free_port()?))
  }

  fn serving(command: String, port: u16, cwd: &std::path::Path) -> WebServerConfig {
    WebServerConfig {
      command: Some(command),
      url: Some(format!("http://127.0.0.1:{port}/")),
      cwd: Some(cwd.display().to_string()),
      timeout: 30_000,
      ..WebServerConfig::default()
    }
  }

  fn sigterm() -> Option<GracefulShutdown> {
    Some(GracefulShutdown {
      signal: "SIGTERM".into(),
      timeout: 5_000,
    })
  }

  async fn answers(port: u16) -> bool {
    tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok()
  }

  #[tokio::test]
  async fn every_part_of_a_compound_command_runs() {
    let (dir, port) = workspace().unwrap();
    let command = format!("printf built > built && python3 server.py {port}");
    let manager = WebServerManager::start(&[serving(command, port, dir.path())])
      .await
      .unwrap();
    assert_eq!(std::fs::read_to_string(dir.path().join("built")).unwrap(), "built");
    manager.stop().await;
  }

  #[tokio::test]
  async fn a_server_that_exits_before_it_is_ready_says_why() {
    let (dir, port) = workspace().unwrap();
    for stderr in [None, Some("ignore".to_string())] {
      let config = WebServerConfig {
        stderr: stderr.clone(),
        name: Some("api".into()),
        ..serving(
          "echo starting; echo 'no database named' >&2; exit 3".into(),
          port,
          dir.path(),
        )
      };
      let Err(error) = WebServerManager::start(&[config]).await else {
        panic!("a server that exited started (stderr: {stderr:?})");
      };
      let message = error.to_string();
      assert!(message.contains("exit status: 3"), "{message}");
      assert!(message.contains("[api] no database named"), "{message}");
      assert!(
        !message.contains("starting"),
        "stdout is not part of the reason: {message}"
      );
    }
  }

  #[tokio::test]
  async fn a_graceful_stop_waits_for_the_server_not_its_shell() {
    let (dir, port) = workspace().unwrap();
    // The trailing `true` keeps the shell from exec'ing the server, so the
    // shell dies of the signal while the server is still shutting down.
    let config = WebServerConfig {
      graceful_shutdown: sigterm(),
      ..serving(format!("python3 server.py {port}; true"), port, dir.path())
    };
    let manager = WebServerManager::start(&[config]).await.unwrap();
    manager.stop().await;
    assert_eq!(
      std::fs::read_to_string(dir.path().join("marker")).ok().as_deref(),
      Some("done"),
      "stop returned before the server finished shutting down"
    );
  }

  #[tokio::test]
  async fn stopping_takes_the_servers_children_too() {
    for graceful in [None, sigterm()] {
      let (dir, port) = workspace().unwrap();
      // The server is the shell's child, not ours.
      let config = WebServerConfig {
        graceful_shutdown: graceful.clone(),
        ..serving(format!("python3 server.py {port}; true"), port, dir.path())
      };
      let manager = WebServerManager::start(&[config]).await.unwrap();
      assert!(answers(port).await);
      manager.stop().await;
      let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
      while answers(port).await && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
      }
      assert!(
        !answers(port).await,
        "the server outlived stop (graceful: {graceful:?})"
      );
    }
  }
}
