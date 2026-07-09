//! `ferridriver session` subcommand: open / host / attach / list / exec /
//! close / close-all.
//!
//! These drive ferridriver's named-session layer (`ferridriver-session`) from
//! the terminal — the token-efficient counterpart to the MCP server for
//! coding agents. `open` launches a browser and binds it under an id in a
//! detached host process; the other verbs are thin
//! [`ferridriver_session::SessionClient`] calls resolved through the registry.

use std::io::Read as _;

use anyhow::Context as _;
use ferridriver::backend::BackendKind;
use ferridriver::browser_type::BrowserType;
use ferridriver::options::{BrowserKind, LaunchOptions};
use ferridriver_session::{BindOptions, Command, Registry, SessionClient, bind_in};

use crate::cli::{
  BrowserArgs, SessionArgs, SessionCommand, SessionExecArgs, SessionHostArgs, SessionListArgs, SessionOpenArgs,
  SessionTargetArgs, backend_to_kind,
};

pub async fn run(args: SessionArgs) -> anyhow::Result<()> {
  match args.command {
    SessionCommand::Open(a) => open(a).await,
    SessionCommand::Host(a) => host(a).await,
    SessionCommand::Attach(a) => attach(a).await,
    SessionCommand::List(a) => list(&a),
    SessionCommand::Exec(a) => exec(a).await,
    SessionCommand::Close(a) => close(&a),
    SessionCommand::CloseAll => close_all(),
  }
}

fn browser_kind_for(backend: BackendKind) -> BrowserKind {
  match backend {
    BackendKind::Bidi => BrowserKind::Firefox,
    BackendKind::WebKit => BrowserKind::WebKit,
    _ => BrowserKind::Chromium,
  }
}

/// Launch a browser for the given CLI browser args.
async fn launch_browser(browser: &BrowserArgs) -> anyhow::Result<ferridriver::Browser> {
  let backend = backend_to_kind(&browser.backend);
  let kind = browser_kind_for(backend);
  let factory = BrowserType::with_backend(kind, backend);
  let options = LaunchOptions {
    headless: Some(browser.headless),
    executable_path: browser.executable_path.clone(),
    ..Default::default()
  };
  factory
    .launch(options)
    .await
    .with_context(|| format!("launching {} browser", kind.name()))
}

/// `open`: spawn a detached `session host` process and wait until its
/// descriptor appears in the registry, then print the endpoint.
async fn open(args: SessionOpenArgs) -> anyhow::Result<()> {
  let registry = Registry::open()?;
  // If a session with this id is already live, refuse rather than clobber.
  if registry.get(&args.id)?.is_some() {
    anyhow::bail!(
      "session '{}' already exists. Close it first with `ferridriver session close {}`.",
      args.id,
      args.id
    );
  }

  let exe = std::env::current_exe().context("resolving the ferridriver executable")?;
  let mut cmd = std::process::Command::new(exe);
  cmd.arg("session").arg("host").arg(&args.id);
  if let Some(url) = &args.url {
    cmd.arg(url);
  }
  cmd.arg("--backend").arg(backend_name(&args.browser));
  if args.browser.headless {
    cmd.arg("--headless");
  }
  if let Some(path) = &args.browser.executable_path {
    cmd.arg("--executable-path").arg(path);
  }
  // Detach: the host owns the browser and outlives this invocation.
  cmd.stdin(std::process::Stdio::null());
  cmd.stdout(std::process::Stdio::null());
  cmd.stderr(std::process::Stdio::null());
  let child = cmd.spawn().context("spawning session host process")?;

  // Wait for the host to publish its descriptor (bounded — the browser
  // launch dominates this).
  let descriptor = wait_for_descriptor(&registry, &args.id, std::time::Duration::from_secs(60)).await?;
  println!(
    "session '{}' open (pid {}) at {}",
    args.id,
    child.id(),
    descriptor.endpoint
  );
  Ok(())
}

/// Poll the registry until `id` appears or the deadline elapses.
async fn wait_for_descriptor(
  registry: &Registry,
  id: &str,
  timeout: std::time::Duration,
) -> anyhow::Result<ferridriver_session::SessionDescriptor> {
  let deadline = std::time::Instant::now() + timeout;
  loop {
    if let Some(d) = registry.get(id)? {
      return Ok(d);
    }
    if std::time::Instant::now() >= deadline {
      anyhow::bail!("session '{id}' did not come up within {timeout:?}");
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
  }
}

/// `host`: the long-lived foreground process. Launch, bind, navigate, serve
/// until killed. `open` spawns this detached.
async fn host(args: SessionHostArgs) -> anyhow::Result<()> {
  let browser = launch_browser(&args.browser).await?;
  // Open the first page (and navigate it if a url was given) so an attaching
  // client sees a ready page immediately.
  let page = browser.new_page().await.context("opening the session's first page")?;
  if let Some(url) = &args.url {
    page.goto(url).await.with_context(|| format!("navigating to {url}"))?;
  }

  let registry = Registry::open()?;
  let session = bind_in(&registry, &browser, &args.id, BindOptions::default(), None)
    .await
    .context("binding the session")?;
  tracing::info!(id = %args.id, endpoint = %session.endpoint(), "session host serving");

  // Serve until a shutdown signal arrives. Racing against the signal (rather
  // than letting SIGTERM default-kill the process) lets the BoundSession drop
  // run on the way out: it stops the server, prunes the descriptor, removes
  // the socket file, and closes the browser.
  let serve = session.server().serve();
  tokio::select! {
    res = serve => { res.context("serving the session")?; },
    () = shutdown_signal() => {
      tracing::info!(id = %args.id, "session host received shutdown signal");
    },
  }
  drop(session);
  browser.close().await.ok();
  Ok(())
}

/// Resolve when the process receives SIGTERM or SIGINT (Ctrl-C).
async fn shutdown_signal() {
  #[cfg(unix)]
  {
    use tokio::signal::unix::{SignalKind, signal};
    let Ok(mut term) = signal(SignalKind::terminate()) else {
      return std::future::pending().await;
    };
    let Ok(mut int) = signal(SignalKind::interrupt()) else {
      return std::future::pending().await;
    };
    tokio::select! {
      _ = term.recv() => {},
      _ = int.recv() => {},
    }
  }
  #[cfg(not(unix))]
  {
    let _ = tokio::signal::ctrl_c().await;
  }
}

/// `attach`: connect and print the current snapshot.
async fn attach(args: SessionTargetArgs) -> anyhow::Result<()> {
  let registry = Registry::open()?;
  let mut client = SessionClient::attach(&registry, &args.id)
    .await
    .with_context(|| format!("attaching to session '{}'", args.id))?;
  let reply = client.call(Command::new(1, "snapshot", serde_json::json!({}))).await?;
  print_reply(&reply, None)
}

/// `list`: read the registry and print live sessions.
fn list(args: &SessionListArgs) -> anyhow::Result<()> {
  let registry = Registry::open()?;
  let sessions = registry.list()?;
  if args.json {
    println!("{}", serde_json::to_string_pretty(&sessions)?);
    return Ok(());
  }
  if sessions.is_empty() {
    println!("no live sessions");
    return Ok(());
  }
  println!("{:<20} {:<10} {:<8} ENDPOINT", "ID", "BROWSER", "PID");
  for s in &sessions {
    println!("{:<20} {:<10} {:<8} {}", s.id, s.browser_name, s.pid, s.endpoint);
  }
  Ok(())
}

/// `exec`: run one verb against a live session.
async fn exec(args: SessionExecArgs) -> anyhow::Result<()> {
  let registry = Registry::open()?;
  let mut client = SessionClient::attach(&registry, &args.id)
    .await
    .with_context(|| format!("attaching to session '{}'", args.id))?;

  let command = build_command(&args)?;
  let reply = client.call(command).await?;
  print_reply(&reply, args.output.as_deref())
}

/// Translate exec CLI flags into a session [`Command`].
fn build_command(args: &SessionExecArgs) -> anyhow::Result<Command> {
  let mut params = serde_json::Map::new();
  let mut put = |k: &str, v: &Option<String>| {
    if let Some(v) = v {
      params.insert(k.to_string(), serde_json::Value::String(v.clone()));
    }
  };
  put("selector", &args.selector);
  put("ref", &args.r#ref);
  put("value", &args.value);
  put("key", &args.key);
  put("url", &args.url);
  put("expression", &args.expression);

  // `run-script` reads its source from --source (or stdin via `-`).
  if args.verb == "run-script" {
    let source = match args.source.as_deref() {
      Some("-") => {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        s
      },
      Some(src) => src.to_string(),
      None => anyhow::bail!("run-script requires --source <code> (or --source - to read stdin)"),
    };
    params.insert("source".to_string(), serde_json::Value::String(source));
  }

  Ok(Command::new(1, args.verb.clone(), serde_json::Value::Object(params)).with_context(args.context.clone()))
}

/// `close`: stop the session. The browser is owned by the detached host
/// process, so signal that process to exit (its [`ferridriver_session::BoundSession`]
/// drop closes the browser and prunes the descriptor); then prune the
/// descriptor directly in case the host already died.
fn close(args: &SessionTargetArgs) -> anyhow::Result<()> {
  let registry = Registry::open()?;
  let descriptor = registry.get(&args.id)?;
  if let Some(d) = &descriptor {
    terminate_owner(d.pid);
  }
  ferridriver_session::unbind(&args.id)?;
  if descriptor.is_some() {
    println!("closed session '{}'", args.id);
  } else {
    println!("no session '{}'", args.id);
  }
  Ok(())
}

/// `close-all`: stop every session.
fn close_all() -> anyhow::Result<()> {
  let registry = Registry::open()?;
  let sessions = registry.list()?;
  for s in &sessions {
    terminate_owner(s.pid);
    ferridriver_session::unbind(&s.id)?;
  }
  println!("closed {} session(s)", sessions.len());
  Ok(())
}

/// Ask the owning host process to exit. SIGTERM lets the host run its
/// `BoundSession` drop (close the browser, remove the socket) cleanly. A no-op
/// when the pid is this process (the rare same-process bind) or already gone.
#[cfg(unix)]
fn terminate_owner(pid: u32) {
  if pid == std::process::id() {
    return;
  }
  let Ok(pid) = libc::pid_t::try_from(pid) else {
    return;
  };
  // SAFETY: kill(2) with SIGTERM on a pid; failure (already dead, not ours)
  // is ignored. No memory is touched.
  #[allow(unsafe_code)]
  unsafe {
    libc::kill(pid, libc::SIGTERM);
  }
}

#[cfg(not(unix))]
fn terminate_owner(_pid: u32) {
  // On non-unix the host is reaped via the registry prune + its own exit;
  // a portable signal path can be added when a Windows host ships.
}

/// Render a session reply to stdout. Binary `data` is written to `output`
/// when provided, otherwise the base64 blob is printed.
fn print_reply(reply: &ferridriver_session::Response, output: Option<&std::path::Path>) -> anyhow::Result<()> {
  if !reply.ok {
    anyhow::bail!("{}", reply.error.as_deref().unwrap_or("session command failed"));
  }
  if let Some(data) = &reply.data {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
      .decode(data)
      .context("decoding session binary payload")?;
    if let Some(path) = output {
      std::fs::write(path, &bytes).with_context(|| format!("writing {}", path.display()))?;
      println!("{} ({} bytes) -> {}", reply.text, bytes.len(), path.display());
    } else {
      // No output file: print the status line, then the base64 so it is
      // still scriptable.
      println!("{}", reply.text);
      println!("{data}");
    }
  } else {
    println!("{}", reply.text);
  }
  Ok(())
}

fn backend_name(browser: &BrowserArgs) -> &'static str {
  match backend_to_kind(&browser.backend) {
    BackendKind::CdpPipe => "cdp-pipe",
    BackendKind::CdpRaw => "cdp-raw",
    BackendKind::WebKit => "webkit",
    BackendKind::Bidi => "bidi",
  }
}
